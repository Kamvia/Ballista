// File: ballista/sdks/java/src/test/java/ballista/client/cli/MainTest.java
package ballista.client.cli;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import ballista.client.exceptions.BallistaException;
import ballista.client.exceptions.ConfigurationException;
import ballista.client.internal.Fields;
import ballista.client.internal.Json;

import com.sun.net.httpserver.HttpServer;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.io.PrintStream;
import java.net.InetSocketAddress;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Map;

import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class MainTest {
    private static final String TOKEN = "bt_0123456789abcdef0123456789abcdef";
    private static final String ID = "r0123456789abcdef01234567";
    private static final String RUN = "{\"id\":\"" + ID + "\",\"seq\":1,\"bundle\":\"0123456789abcdef0123456789abcdef\",\"plan_name\":\"p\",\"state\":\"%s\","
            + "\"created_at\":\"t\",\"created_by\":\"ci\",\"labels\":{},\"overrides\":{},"
            + "\"summary\":{\"requests\":10,\"failures\":0,\"rps\":2,\"thresholds\":[{\"metric\":\"p95\",\"request\":\"*\",\"max\":100,\"actual\":5,\"passed\":true}]},"
            + "\"comparison\":{\"baseline\":null,\"passed\":true,\"regressions\":0,\"changes\":[],\"unmatched\":[],\"note\":\"first run\"}}";

    private HttpServer server;
    private Map<String, String> env;
    private volatile String state = "passed";
    private volatile boolean comparePassed = true;
    private volatile Object created;
    private final ByteArrayOutputStream out = new ByteArrayOutputStream();
    private PrintStream oldOut;
    private PrintStream oldErr;

    @BeforeEach
    void start() throws IOException {
        server = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        server.createContext("/", ex -> {
            String p = ex.getRequestURI().getPath();
            String body;
            int status = 200;
            if (!("Bearer " + TOKEN).equals(ex.getRequestHeaders().getFirst("Authorization"))) {
                status = 401;
                body = "";
            } else if (p.equals("/api/v1/bundles")) {
                body = "{\"id\":\"0123456789abcdef0123456789abcdef\",\"plan_name\":\"p\",\"scenarios\":[],\"files\":[],\"labels\":{},\"created_at\":\"t\",\"created_by\":\"ci\"}";
            } else if (p.equals("/api/v1/runs") && ex.getRequestMethod().equals("POST")) {
                created = Json.parse(new String(ex.getRequestBody().readAllBytes(), StandardCharsets.UTF_8));
                status = 202;
                body = String.format(RUN, "queued");
            } else if (p.equals("/api/v1/runs")) {
                body = "{\"items\":[" + String.format(RUN, state) + "],\"next_cursor\":null}";
            } else if (p.endsWith("/events")) {
                body = "event: end\ndata: " + String.format(RUN, state) + "\n\n";
            } else if (p.endsWith("/compare")) {
                body = "{\"baseline\":\"r000000000000000000000001\",\"passed\":" + comparePassed + ",\"regressions\":" + (comparePassed ? 0 : 1)
                        + ",\"changes\":[{\"kind\":\"total\",\"method\":\"\",\"name\":\"Aggregated\",\"metric\":\"p95\",\"baseline\":1,\"current\":2,"
                        + "\"change_pct\":100,\"regressed\":" + !comparePassed + "}],\"unmatched\":[]}";
            } else if (p.contains("/artifacts/")) {
                body = "artifact";
            } else if (p.endsWith("/stop")) {
                body = String.format(RUN, "cancelled");
            } else {
                body = String.format(RUN, state);
            }
            byte[] b = body.getBytes(StandardCharsets.UTF_8);
            ex.sendResponseHeaders(status, b.length == 0 ? -1 : b.length);
            if (b.length > 0) {
                try (OutputStream o = ex.getResponseBody()) {
                    o.write(b);
                }
            }
            ex.close();
        });
        server.start();
        env = Map.of("BALLISTA_URL", "http://127.0.0.1:" + server.getAddress().getPort(), "BALLISTA_TOKEN", TOKEN);
        oldOut = System.out;
        oldErr = System.err;
        PrintStream capture = new PrintStream(out, true, StandardCharsets.UTF_8);
        System.setOut(capture);
        System.setErr(capture);
    }

    @AfterEach
    void stop() {
        System.setOut(oldOut);
        System.setErr(oldErr);
        server.stop(0);
    }

    private String output() {
        String s = out.toString(StandardCharsets.UTF_8);
        out.reset();
        return s;
    }

    private int run(String... args) throws IOException {
        return Main.run(args, env);
    }

    private static Path write(Path dir, String name, String content) throws IOException {
        Path p = dir.resolve(name);
        Files.writeString(p, content);
        return p;
    }

    @Test
    void submitSendsOverridesGateAndLabels(@TempDir Path dir) throws IOException {
        Path plan = write(dir, "plan.json", "{\"name\":\"p\",\"target\":{\"base_url\":\"http://127.0.0.1:8080/\"},\"load\":{\"stages\":[{\"duration\":\"1s\",\"target\":1}]},"
                + "\"scenario\":[{\"name\":\"s\",\"step\":[{\"get\":\"/\"}]}]}");
        Path asset = write(dir, "s.rhai", "fn run(s, r) {}");
        Path reports = dir.resolve("reports");
        int code = run("submit", plan.toString(), "--users", "5", "--run-time=10s", "--workers", "0", "--tags", "a,b", "--label", "build=7",
                "--label", "branch=main", "--file", "scenarios/s.rhai=" + asset, "--gate", "15", "--baseline", "auto", "--quiet", "--report", reports.toString());
        String text = output();
        assertEquals(0, code, text);
        Fields body = Fields.of(created, "created");
        assertEquals(5.0, body.obj("overrides").num("users"));
        assertEquals("10s", body.obj("overrides").str("run_time"));
        assertEquals(0, body.obj("overrides").lng("workers"));
        assertEquals(15.0, body.obj("regression").num("max_regression_pct"));
        assertEquals("auto", body.obj("regression").str("baseline"));
        assertEquals(Map.of("build", "7", "branch", "main"), body.strMap("labels"));
        for (String want : new String[] {"queued run " + ID, "requests=10", "threshold p95 * actual=5.000 pass", "note: first run"}) {
            assertTrue(text.contains(want), want + " in " + text);
        }
        try (var files = Files.list(reports)) {
            assertEquals(2, files.count());
        }
    }

    @Test
    void submitOmitsUnsetOptionsAndMapsFailure(@TempDir Path dir) throws IOException {
        Path plan = write(dir, "plan.toml", "name = \"p\"\n");
        assertEquals(0, run("submit", plan.toString(), "--no-wait"));
        Fields body = Fields.of(created, "created");
        assertFalse(body.has("regression"));
        assertFalse(body.obj("overrides").has("workers"));
        state = "failed";
        assertEquals(2, run("submit", plan.toString(), "--quiet"));
    }

    @Test
    void submitInputErrors(@TempDir Path dir) throws IOException {
        String good = write(dir, "p.toml", "name = \"p\"\n").toString();
        String[][] cases = {
            {"submit"},
            {"submit", good, "--label", "novalue"},
            {"submit", good, "--file", "noequals"},
            {"submit", good, "--file", "../x=" + good},
            {"submit", good, "--users", "many"},
            {"submit", good, "--run-time", "forever"},
            {"submit", good, "--users"},
            {"submit", write(dir, "p.json", "[1,2]").toString()},
            {"submit", write(dir, "q.json", "{broken").toString()},
            {"submit", write(dir, "big.toml", "#".repeat((1 << 20) + 1)).toString()},
        };
        for (String[] args : cases) {
            assertThrows(BallistaException.class, () -> run(args), String.join(" ", args));
        }
        assertThrows(IOException.class, () -> run("submit", dir.resolve("missing.toml").toString()));
    }

    @Test
    void otherCommands(@TempDir Path dir) throws IOException {
        state = "stopped";
        assertEquals(0, run("status", ID));
        assertTrue(output().contains("run " + ID + " stopped"));
        assertEquals(0, run("stop", ID));
        assertTrue(output().contains("cancelled"));
        assertEquals(2, run("wait", ID));
        assertEquals(0, run("list"));
        assertTrue(output().contains("\"state\":\"stopped\""));
        assertEquals(0, run("compare", ID, "--max-regression-pct", "5"));
        assertTrue(output().contains("passed=true"));
        comparePassed = false;
        assertEquals(2, run("compare", ID));
        assertTrue(output().contains("REGRESSED"));
        assertEquals(0, run("report", ID, "--name", "stats.csv", "--out", dir.toString()));
        assertTrue(output().contains(ID + "-stats.csv"));
        assertThrows(BallistaException.class, () -> run("report", ID, "--name", "../../etc/passwd"));
        assertThrows(BallistaException.class, () -> run("status"));
        assertThrows(ConfigurationException.class, () -> run("status", "../x"));
        assertEquals(1, run("dance"));
        assertEquals(1, run());
        assertEquals(0, run("--help"));
        assertTrue(output().contains("usage: ballista-client"));
    }

    @Test
    void configurationErrors() {
        ConfigurationException e = assertThrows(ConfigurationException.class, () -> Main.run(new String[] {"list"}, Map.of("BALLISTA_URL", env.get("BALLISTA_URL"), "BALLISTA_TOKEN", "short")));
        assertFalse(e.getMessage().contains("short"));
        assertThrows(ConfigurationException.class, () -> run("--url", "http://203.0.113.9:8070", "list"));
    }
}
