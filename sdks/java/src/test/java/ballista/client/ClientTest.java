// File: ballista/sdks/java/src/test/java/ballista/client/ClientTest.java
package ballista.client;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import ballista.client.exceptions.ApiException;
import ballista.client.exceptions.ConfigurationException;
import ballista.client.exceptions.PermissionDeniedException;
import ballista.client.exceptions.RunTimeoutException;
import ballista.client.internal.Fields;
import ballista.client.internal.Json;
import ballista.client.models.CompareMetric;
import ballista.client.models.Overrides;
import ballista.client.models.RegressionGate;
import ballista.client.models.Run;
import ballista.client.models.RunState;

import com.sun.net.httpserver.HttpExchange;
import com.sun.net.httpserver.HttpServer;

import java.io.IOException;
import java.io.OutputStream;
import java.net.InetSocketAddress;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.function.Consumer;

import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Test;

class ClientTest {
    static final String TOKEN = "bt_0123456789abcdef0123456789abcdef";
    static final String RUN = "{\"id\":\"r0123456789abcdef01234567\",\"seq\":1,\"bundle\":\"0123456789abcdef0123456789abcdef\",\"plan_name\":\"p\","
            + "\"state\":\"%s\",\"created_at\":\"t\",\"created_by\":\"ci\",\"labels\":{},\"overrides\":{\"workers\":2}}";

    private HttpServer server;

    private Client serve(Consumer<HttpExchange> handler) throws IOException {
        server = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        server.createContext("/", ex -> {
            try {
                handler.accept(ex);
            } finally {
                ex.close();
            }
        });
        server.start();
        return Client.builder().url("http://127.0.0.1:" + server.getAddress().getPort()).token(TOKEN)
                .retry(new RetryPolicy(3, Duration.ofMillis(1), Duration.ofMillis(5))).build();
    }

    @AfterEach
    void stop() {
        if (server != null) {
            server.stop(0);
        }
    }

    static void reply(HttpExchange ex, int status, String body) {
        try {
            byte[] b = body.getBytes(StandardCharsets.UTF_8);
            ex.sendResponseHeaders(status, b.length == 0 ? -1 : b.length);
            try (OutputStream o = ex.getResponseBody()) {
                o.write(b);
            }
        } catch (IOException e) {
            throw new RuntimeException(e);
        }
    }

    static String body(HttpExchange ex) {
        try {
            return new String(ex.getRequestBody().readAllBytes(), StandardCharsets.UTF_8);
        } catch (IOException e) {
            throw new RuntimeException(e);
        }
    }

    @Test
    void rejectsUnsafeConfiguration() {
        assertThrows(ConfigurationException.class, () -> Client.builder().url("https://example.com").token("short").build());
        assertThrows(ConfigurationException.class, () -> Client.builder().url("http://example.com").token(TOKEN).build());
        assertThrows(ConfigurationException.class, () -> Client.builder().url("https://u:p@example.com").token(TOKEN).build());
        assertThrows(ConfigurationException.class, () -> Client.builder().url("ftp://example.com").token(TOKEN).build());
        Client c = Client.builder().url("http://127.0.0.1:8070/x").token(TOKEN).build();
        assertEquals("http://127.0.0.1:8070/api/v1", c.baseUrl());
        assertFalse(c.toString().contains(TOKEN));
    }

    @Test
    void retriesMapsErrorsAndRefusesRedirects() throws IOException {
        AtomicInteger calls = new AtomicInteger();
        Client c = serve(ex -> {
            assertEquals("Bearer " + TOKEN, ex.getRequestHeaders().getFirst("Authorization"));
            switch (ex.getRequestURI().getPath()) {
                case "/api/v1/health" -> {
                    if (calls.incrementAndGet() < 3) {
                        reply(ex, 503, "");
                    } else {
                        reply(ex, 200, "{\"status\":\"ok\",\"version\":\"1\"}");
                    }
                }
                case "/api/v1/whoami" -> reply(ex, 403, "{\"error\":{\"code\":\"forbidden\",\"message\":\"scope too low\"}}");
                default -> {
                    ex.getResponseHeaders().set("Location", "https://evil.example/");
                    reply(ex, 302, "");
                }
            }
        });
        assertEquals("ok", c.health().status());
        assertEquals(3, calls.get());
        PermissionDeniedException e = assertThrows(PermissionDeniedException.class, c::whoami);
        assertEquals("scope too low", e.detail());
        assertEquals(302, assertThrows(ApiException.class, c::status).status());
    }

    @Test
    void createRunSendsKeyOverridesAndGate() throws IOException {
        List<String> keys = new ArrayList<>();
        List<Object> bodies = new ArrayList<>();
        Client c = serve(ex -> {
            keys.add(ex.getRequestHeaders().getFirst("Idempotency-Key"));
            bodies.add(Json.parse(body(ex)));
            reply(ex, 202, String.format(RUN, "queued"));
        });
        RunOptions opts = RunOptions.of(Overrides.builder().workers(2).runTime("5s").build())
                .withRegression(RegressionGate.builder().maxRegressionPct(15).metrics(CompareMetric.P95).build());
        Run r = c.createRun("0".repeat(32), opts);
        assertEquals(RunState.QUEUED, r.state());
        assertEquals(2, r.overrides().workers());
        assertEquals(36, keys.getFirst().length());
        Fields f = Fields.of(bodies.getFirst(), "body");
        assertEquals(2, f.obj("overrides").lng("workers"));
        assertEquals(15.0, f.obj("regression").num("max_regression_pct"));
        assertEquals(List.of("p95"), f.obj("regression").strList("metrics"));

        List<Runnable> bad = List.of(
                () -> Overrides.builder().users(-1).build(),
                () -> Overrides.builder().workers(513).build(),
                () -> Overrides.builder().runTime("soon").build(),
                () -> Overrides.builder().host("http://u:p@x").build(),
                () -> RegressionGate.builder().baseline("../etc").build(),
                () -> c.createRun("0".repeat(32), RunOptions.DEFAULT.withLabels(Map.of("bad key", "v"))),
                () -> c.createRun("0".repeat(32), RunOptions.DEFAULT.withIdempotencyKey("has space")),
                () -> c.getRun("../runs"),
                () -> c.artifact("r0123456789abcdef01234567", "../../etc/passwd"));
        for (Runnable b : bad) {
            assertThrows(ConfigurationException.class, b::run);
        }
        assertEquals(1, keys.size());
    }

    @Test
    void waitFollowsEvents() throws IOException {
        Client c = serve(ex -> {
            if (!ex.getRequestURI().getPath().endsWith("/events")) {
                reply(ex, 200, String.format(RUN, "passed"));
                return;
            }
            ex.getResponseHeaders().set("Content-Type", "text/event-stream");
            reply(ex, 200, ": keepalive\n\nevent: run\ndata: " + String.format(RUN, "running") + "\n\n"
                    + "event: stats\ndata: {\"elapsed_secs\":1.5,\"users\":3,\ndata: \"total\":{\"count\":9}}\r\n\r\n"
                    + "event: end\ndata: " + String.format(RUN, "passed") + "\n\n");
        });
        List<RunState> states = new ArrayList<>();
        List<Long> counts = new ArrayList<>();
        Run r = c.waitFor("r0123456789abcdef01234567", WaitOptions.DEFAULT
                .withOnState(x -> states.add(x.state())).withOnStats(s -> counts.add(s.total().count())));
        assertEquals(RunState.PASSED, r.state());
        assertEquals(List.of(RunState.RUNNING, RunState.PASSED), states);
        assertEquals(List.of(9L), counts);
    }

    @Test
    void waitTimesOut() throws IOException {
        Client c = serve(ex -> {
            if (ex.getRequestURI().getPath().endsWith("/events")) {
                reply(ex, 503, "");
            } else {
                reply(ex, 200, String.format(RUN, "running"));
            }
        });
        RunTimeoutException e = assertThrows(RunTimeoutException.class, () -> c.waitFor("r0123456789abcdef01234567",
                WaitOptions.timeout(Duration.ofMillis(300)).withPollInterval(Duration.ofMillis(20))));
        assertInstanceOf(RunTimeoutException.class, e);
    }

    @Test
    void bundleRules() {
        Bundle b = new Bundle("name = \"x\"");
        for (String rel : List.of("../x", "/abs", ".hidden", "a/../b", "plan.toml", "bundle.json", "inline/x", "a//b")) {
            assertThrows(ConfigurationException.class, () -> b.addText(rel, "x"), rel);
        }
        b.addText("scenarios/s.rhai", "fn run(s, r) {}");
        assertEquals(15, b.files().get("scenarios/s.rhai"));
        assertThrows(ConfigurationException.class, () -> b.addFile("x", java.nio.file.Path.of("/etc")));
        assertThrows(ConfigurationException.class, () -> new Bundle("   "));
        assertTrue(b.toString().contains("files=1"));
    }
}
