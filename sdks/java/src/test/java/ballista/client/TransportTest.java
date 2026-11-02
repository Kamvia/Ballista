// File: ballista/sdks/java/src/test/java/ballista/client/TransportTest.java
package ballista.client;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;
import static org.junit.jupiter.api.Assumptions.assumeTrue;

import ballista.client.exceptions.ApiException;
import ballista.client.exceptions.BallistaException;
import ballista.client.exceptions.ConfigurationException;
import ballista.client.exceptions.ConflictException;
import ballista.client.exceptions.RateLimitException;
import ballista.client.exceptions.ResponseException;
import ballista.client.exceptions.ServerException;
import ballista.client.exceptions.TransportException;
import ballista.client.exceptions.ValidationException;

import com.sun.net.httpserver.HttpExchange;
import com.sun.net.httpserver.HttpServer;
import com.sun.net.httpserver.HttpsConfigurator;
import com.sun.net.httpserver.HttpsServer;

import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.InetSocketAddress;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.KeyStore;
import java.time.Duration;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.function.Consumer;

import javax.net.ssl.KeyManagerFactory;
import javax.net.ssl.SSLContext;

import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class TransportTest {
    private static final String TOKEN = FakeApi.TOKEN;
    private HttpServer server;

    private Client serve(Consumer<HttpExchange> h, RetryPolicy retry, Duration timeout) throws IOException {
        server = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        server.createContext("/", ex -> {
            try {
                h.accept(ex);
            } finally {
                ex.close();
            }
        });
        server.setExecutor(java.util.concurrent.Executors.newVirtualThreadPerTaskExecutor());
        server.start();
        return Client.builder().url("http://127.0.0.1:" + server.getAddress().getPort()).token(TOKEN).retry(retry).timeout(timeout).build();
    }

    private Client serve(Consumer<HttpExchange> h) throws IOException {
        return serve(h, new RetryPolicy(3, Duration.ofMillis(1), Duration.ofMillis(5)), Duration.ofSeconds(10));
    }

    @AfterEach
    void stop() {
        if (server != null) {
            server.stop(0);
        }
    }

    @Test
    void retryAfterIsCappedByMaxDelay() throws IOException {
        AtomicInteger calls = new AtomicInteger();
        Client c = serve(ex -> {
            if (calls.incrementAndGet() == 1) {
                ex.getResponseHeaders().set("Retry-After", "3600");
                FakeApi.reply(ex, 429, "");
            } else {
                FakeApi.reply(ex, 200, "{\"status\":\"ok\",\"version\":\"1\"}");
            }
        });
        long start = System.nanoTime();
        assertEquals("ok", c.health().status());
        assertTrue(Duration.ofNanos(System.nanoTime() - start).toMillis() < 2000);
    }

    @Test
    void exhaustedRetriesAndStatusMapping() throws IOException {
        AtomicInteger calls = new AtomicInteger();
        int[] status = {503};
        Client c = serve(ex -> {
            calls.incrementAndGet();
            ex.getResponseHeaders().set("Retry-After", "0");
            FakeApi.reply(ex, status[0], "<html>gateway</html>");
        });
        ServerException e = assertThrows(ServerException.class, c::health);
        assertEquals(503, e.status());
        assertEquals("error", e.code());
        assertEquals(3, calls.get());
        assertEquals(Duration.ZERO, e.retryAfter());
        calls.set(0);
        status[0] = 422;
        assertThrows(ValidationException.class, c::health);
        assertEquals(1, calls.get(), "client errors are not retried");
        status[0] = 409;
        assertThrows(ConflictException.class, c::health);
        status[0] = 429;
        assertInstanceOf(RateLimitException.class, assertThrows(ApiException.class, c::health));
        status[0] = 418;
        ApiException plain = assertThrows(ApiException.class, c::health);
        assertEquals(ApiException.class, plain.getClass());
    }

    @Test
    void errorDetailIsBoundedAndTokenFree() throws IOException {
        String longDetail = "e".repeat(5000);
        Client c = serve(ex -> FakeApi.reply(ex, 400, "{\"error\":{\"code\":\"bad_request\",\"message\":\"" + longDetail + "\"}}"));
        ApiException e = assertThrows(ApiException.class, c::health);
        assertEquals(1000, e.detail().length());
        assertEquals("bad_request", e.code());
        assertFalse(e.getMessage().contains(TOKEN));
    }

    @Test
    void connectionRefused() throws IOException {
        HttpServer s = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        s.start();
        int port = s.getAddress().getPort();
        s.stop(0);
        Client c = Client.builder().url("http://127.0.0.1:" + port).token(TOKEN).timeout(Duration.ofSeconds(5))
                .retry(new RetryPolicy(2, Duration.ofMillis(1), Duration.ofMillis(1))).build();
        long start = System.nanoTime();
        TransportException e = assertThrows(TransportException.class, c::health);
        assertTrue(Duration.ofNanos(System.nanoTime() - start).toMillis() < 3000, "refused connections fail fast");
        assertTrue(e.getMessage().startsWith("GET /health"), e.getMessage());
        assertFalse(e.getMessage().contains(TOKEN));
    }

    @Test
    void slowServerHitsRequestTimeout() throws IOException {
        Client c = serve(ex -> {
            try {
                Thread.sleep(3000);
            } catch (InterruptedException ignored) {
                Thread.currentThread().interrupt();
            }
        }, new RetryPolicy(1, Duration.ZERO, Duration.ZERO), Duration.ofMillis(100));
        long start = System.nanoTime();
        assertThrows(TransportException.class, c::health);
        assertTrue(Duration.ofNanos(System.nanoTime() - start).toMillis() < 2000);
    }

    @Test
    void malformedResponsesAreRejected() throws IOException {
        Client c = serve(ex -> {
            String body = switch (ex.getRequestURI().getPath()) {
                case "/api/v1/health" -> "not json";
                case "/api/v1/whoami" -> "{\"name\":\"x\"}";
                case "/api/v1/runs/r000000000000000000000001" -> "{\"id\":\"../../etc\",\"state\":\"passed\"}";
                case "/api/v1/runs/r000000000000000000000002" -> "{\"id\":\"r000000000000000000000002\",\"seq\":1,\"bundle\":\"b\",\"plan_name\":\"p\","
                        + "\"state\":\"exploded\",\"created_at\":\"t\",\"created_by\":\"c\"}";
                case "/api/v1/status" -> "[1,2]";
                default -> "{\"id\":\"0123456789abcdef0123456789abcdef\",\"plan_name\":\"p\",\"files\":\"oops\",\"created_at\":\"t\",\"created_by\":\"c\"}";
            };
            FakeApi.reply(ex, 200, body);
        });
        assertThrows(ResponseException.class, c::health);
        assertThrows(ResponseException.class, c::whoami);
        assertThrows(ResponseException.class, c::status);
        assertThrows(ResponseException.class, () -> c.getRun("r000000000000000000000001"));
        assertThrows(ResponseException.class, () -> c.getRun("r000000000000000000000002"));
        assertThrows(ResponseException.class, () -> c.getBundle("0123456789abcdef0123456789abcdef"));
    }

    @Test
    void oversizedResponseIsRejected() throws IOException {
        Client c = serve(ex -> {
            try {
                ex.sendResponseHeaders(200, 0);
                try (OutputStream o = ex.getResponseBody()) {
                    byte[] chunk = "a".repeat(1 << 20).getBytes(StandardCharsets.US_ASCII);
                    for (int i = 0; i < 33; i++) {
                        o.write(chunk);
                    }
                }
            } catch (IOException ignored) {
                // client stops reading at the limit
            }
        });
        TransportException e = assertThrows(TransportException.class, c::health);
        assertTrue(e.getMessage().contains("exceeds"), e.getMessage());
    }

    @Test
    void redirectsAreNeverFollowed() throws IOException {
        List<String> paths = new ArrayList<>();
        Client c = serve(ex -> {
            paths.add(ex.getRequestURI().getPath());
            ex.getResponseHeaders().set("Location", "/elsewhere");
            FakeApi.reply(ex, 307, "");
        });
        assertEquals(307, assertThrows(ApiException.class, c::health).status());
        assertEquals(List.of("/api/v1/health"), paths);
    }

    @Test
    void configurationRules() {
        assertThrows(ConfigurationException.class, () -> Client.builder().url("http://10.1.2.3").token(TOKEN).build());
        assertTrue(Client.builder().url("http://10.1.2.3").token(TOKEN).allowInsecureHttp(true).build().baseUrl().startsWith("http://10.1.2.3"));
        for (String u : List.of("http://[::1]:8070", "http://127.10.0.1", "http://localhost:1")) {
            Client.builder().url(u).token(TOKEN).build();
        }
        for (String u : List.of("http://localhost.evil.example", "http://127.0.0.1.nip.io", "not a url", "https://")) {
            assertThrows(ConfigurationException.class, () -> Client.builder().url(u).token(TOKEN).build(), u);
        }
        for (String t : List.of("", "short", "a".repeat(31), "a".repeat(40) + "é", "a".repeat(20) + " " + "a".repeat(20), "a".repeat(4097))) {
            assertThrows(ConfigurationException.class, () -> Client.builder().url("https://x.example").token(t).build());
        }
        assertEquals("https://x.example:8443/api/v1", Client.builder().url(" https://x.example:8443/ignored?x=1 ").token("  " + TOKEN + "\n").build().baseUrl());
        assertThrows(IllegalArgumentException.class, () -> new RetryPolicy(1, Duration.ofSeconds(-1), Duration.ZERO));
        assertEquals(1, new RetryPolicy(0, Duration.ZERO, Duration.ZERO).attempts());
    }

    private static void keytool(Path dir, String... args) throws Exception {
        List<String> cmd = new ArrayList<>(List.of(Path.of(System.getProperty("java.home"), "bin", "keytool").toString()));
        cmd.addAll(List.of(args));
        Process p = new ProcessBuilder(cmd).directory(dir.toFile()).redirectErrorStream(true).start();
        try (InputStream in = p.getInputStream()) {
            in.readAllBytes();
        }
        assumeTrue(p.waitFor() == 0, "keytool unavailable");
    }

    @Test
    void customCertificateAuthority(@TempDir Path dir) throws Exception {
        keytool(dir, "-genkeypair", "-alias", "srv", "-keyalg", "EC", "-groupname", "secp256r1", "-dname", "CN=localhost", "-ext", "SAN=ip:127.0.0.1,dns:localhost",
                "-validity", "2", "-keystore", "srv.p12", "-storetype", "PKCS12", "-storepass", "changeit");
        keytool(dir, "-exportcert", "-rfc", "-alias", "srv", "-keystore", "srv.p12", "-storepass", "changeit", "-file", "ca.pem");
        KeyStore ks = KeyStore.getInstance("PKCS12");
        try (InputStream in = Files.newInputStream(dir.resolve("srv.p12"))) {
            ks.load(in, "changeit".toCharArray());
        }
        KeyManagerFactory kmf = KeyManagerFactory.getInstance(KeyManagerFactory.getDefaultAlgorithm());
        kmf.init(ks, "changeit".toCharArray());
        SSLContext ctx = SSLContext.getInstance("TLS");
        ctx.init(kmf.getKeyManagers(), null, null);
        HttpsServer https = HttpsServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        https.setHttpsConfigurator(new HttpsConfigurator(ctx));
        https.createContext("/", ex -> {
            FakeApi.reply(ex, 200, "{\"status\":\"ok\",\"version\":\"tls\"}");
            ex.close();
        });
        https.start();
        try {
            String url = "https://127.0.0.1:" + https.getAddress().getPort();
            RetryPolicy once = new RetryPolicy(1, Duration.ZERO, Duration.ZERO);
            assertThrows(TransportException.class, () -> Client.builder().url(url).token(TOKEN).retry(once).build().health());
            Client trusted = Client.builder().url(url).token(TOKEN).retry(once).caCertFile(dir.resolve("ca.pem")).build();
            assertEquals("tls", trusted.health().version());
            Files.writeString(dir.resolve("empty.pem"), "no certificates here");
            assertThrows(ConfigurationException.class, () -> Client.builder().url(url).token(TOKEN).caCertFile(dir.resolve("empty.pem")).build());
            assertThrows(ConfigurationException.class, () -> Client.builder().url(url).token(TOKEN).caCertFile(dir.resolve("missing.pem")).build());
        } finally {
            https.stop(0);
        }
    }

    @Test
    void exceptionHierarchy() {
        assertInstanceOf(BallistaException.class, new ResponseException("x"));
        assertEquals("unexpected response: x", new ResponseException("x").getMessage());
        ApiException e = ApiException.of(404, "not_found", "gone", null);
        assertEquals("404 not_found: gone", e.getMessage());
        assertNull(e.retryAfter());
        assertInstanceOf(ServerException.class, ApiException.of(599, "x", "y", null));
    }
}
