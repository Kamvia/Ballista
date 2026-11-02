// File: ballista/sdks/java/src/test/java/ballista/client/EndpointsTest.java
package ballista.client;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import ballista.client.exceptions.AuthenticationException;
import ballista.client.exceptions.ConfigurationException;
import ballista.client.exceptions.NotFoundException;
import ballista.client.internal.Fields;
import ballista.client.models.BundleInfo;
import ballista.client.models.CompareMetric;
import ballista.client.models.Comparison;
import ballista.client.models.Overrides;
import ballista.client.models.Page;
import ballista.client.models.PlanCheck;
import ballista.client.models.RegressionGate;
import ballista.client.models.Run;
import ballista.client.models.RunState;
import ballista.client.models.ServerStatus;
import ballista.client.models.Snapshot;

import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.attribute.PosixFilePermissions;
import java.util.Base64;
import java.util.List;
import java.util.Map;
import java.util.stream.Collectors;

import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class EndpointsTest {
    private FakeApi api;
    private Client c;

    @BeforeEach
    void start() throws Exception {
        api = new FakeApi();
        c = api.client();
    }

    @AfterEach
    void stop() {
        api.close();
    }

    private String uploaded() {
        return c.uploadPlan(PlanTest.shopPlan("http://127.0.0.1:8080/")).id();
    }

    @Test
    void serverInformation() {
        assertEquals("0.1.0", c.health().version());
        assertTrue(api.headers.get("health").get("user-agent").startsWith("ballista-client-java/"));
        assertEquals("application/json", api.headers.get("health").get("accept"));
        assertEquals("run", c.whoami().scope());
        ServerStatus st = c.status();
        assertEquals(1, st.queued());
        assertEquals(50, st.limits().maxQueue());
        assertNull(st.activeRun());
        assertEquals(8, st.workers().getFirst().cores());
        assertNull(st.workers().getFirst().cpuPct());
        Client wrong = Client.builder().url(api.url()).token("x".repeat(40)).build();
        assertThrows(AuthenticationException.class, wrong::health);
    }

    @Test
    void validateAndBundles() {
        PlanCheck check = c.validatePlan(PlanTest.shopPlan("http://127.0.0.1:8080/"), Overrides.builder().runTime("5s").workers(0).build());
        assertEquals("shop api", check.name());
        assertEquals(List.of(), check.files());
        Fields ov = Fields.of(api.bodies.get("validate"), "body").obj("overrides");
        assertEquals("5s", ov.str("run_time"));
        assertEquals(0, ov.lng("workers"));
        assertThrows(ConfigurationException.class, () -> c.validatePlan(42));
        assertThrows(ConfigurationException.class, () -> c.validatePlan(Map.of()));

        Bundle b = new Bundle(PlanTest.shopPlan("http://127.0.0.1:8080/"), Map.of("suite", "unit")).addBytes("data/bin.dat", new byte[] {0, 1, 2, (byte) 255});
        BundleInfo info = c.uploadBundle(b);
        Fields sent = Fields.of(api.bodies.get("upload"), "upload");
        assertArrayEquals(new byte[] {0, 1, 2, (byte) 255}, Base64.getDecoder().decode(sent.obj("files").str("data/bin.dat")));
        assertEquals("unit", sent.obj("labels").str("suite"));
        assertEquals("shop api", sent.obj("plan").str("name"));
        assertEquals(info.id(), uploaded());
        assertEquals(1, c.listBundles().size());
        assertEquals(info.id(), c.getBundle(info.id()).id());
        c.deleteBundle(info.id());
        assertThrows(NotFoundException.class, () -> c.getBundle(info.id()));
        assertThrows(ConfigurationException.class, () -> c.deleteBundle("ABC"));
    }

    @Test
    void runLifecycle() {
        String bundle = uploaded();
        RunOptions opts = RunOptions.DEFAULT.withLabels(Map.of("build", "7")).withIdempotencyKey("build-7");
        Run first = c.createRun(bundle, opts);
        assertEquals("7", first.labels().get("build"));
        assertEquals(first.id(), c.createRun(bundle, opts).id());
        assertEquals("build-7", api.headers.get("create").get("idempotency-key"));
        assertThrows(NotFoundException.class, () -> c.createRun("a".repeat(32), RunOptions.DEFAULT));
        Fields body = Fields.of(api.bodies.get("create"), "create");
        assertNotNull(body.raw().get("overrides"));
        assertNotNull(body.raw().get("labels"));
        assertFalse(body.has("regression"));
        for (int i = 0; i < 4; i++) {
            c.createRun(bundle, null);
        }
        Page<Run> page = c.listRuns(RunState.QUEUED, 500, "0");
        assertEquals(5, page.items().size());
        assertNull(page.nextCursor());
        assertEquals("200", api.queries.get("list").get("limit"));
        assertEquals("queued", api.queries.get("list").get("state"));

        int before = api.calls.getOrDefault("list", 0);
        assertEquals(5, c.runs(null, 2).count());
        assertEquals(3, api.calls.get("list") - before);
        before = api.calls.get("list");
        assertEquals(1, c.runs(null, 2).limit(1).count());
        assertEquals(1, api.calls.get("list") - before, "pages are fetched lazily");

        Run stopped = c.stopRun(first.id());
        assertEquals(RunState.CANCELLED, stopped.state());
        assertEquals("ci", stopped.stoppedBy());
        c.deleteRun(first.id());
        assertEquals(1, api.calls.get("delete-run"));
        assertThrows(NotFoundException.class, () -> c.stopRun("r000000000000000000000fff"));
    }

    @Test
    void statsCompareAndArtifacts(@TempDir Path dir) throws Exception {
        String id = "r000000000000000000000001";
        Snapshot s = c.getStats(id);
        assertEquals(40, s.total().count());
        assertEquals(4, s.users());
        assertEquals(2, s.distribution().size());
        assertNull(s.distribution().get(1).upperMs());
        assertEquals(3, s.findRow("item", "SSE").orElseThrow().count());
        assertEquals("TXN", s.findRow("purchase", null).orElseThrow().method());
        assertTrue(s.findRow("missing", null).isEmpty());

        Comparison cmp = c.compare(id, new CompareOptions("r000000000000000000000002", 12.5, List.of(CompareMetric.P95, CompareMetric.RPS)));
        assertFalse(cmp.passed());
        assertEquals(1, cmp.regressions());
        assertEquals(80.0, cmp.changes().getFirst().changePct());
        assertEquals(CompareMetric.P95, cmp.changes().getFirst().metric());
        assertEquals(List.of("GET /gone"), cmp.unmatched());
        Map<String, String> q = api.queries.get("compare");
        assertEquals("r000000000000000000000002", q.get("baseline"));
        assertEquals("12.5", q.get("max_regression_pct"));
        assertEquals("p95,rps", q.get("metrics"));
        c.compare(id, null);
        assertEquals("auto", api.queries.get("compare").get("baseline"));
        assertFalse(api.queries.get("compare").containsKey("metrics"));
        assertThrows(ConfigurationException.class, () -> c.compare(id, new CompareOptions("latest", null, null)));
        assertThrows(ConfigurationException.class, () -> c.compare(id, new CompareOptions(null, -1.0, null)));
        assertThrows(ConfigurationException.class, () -> c.compare(id, new CompareOptions(null, null, List.of())));

        assertEquals("p", c.report(id).get("plan"));
        assertThrows(ConfigurationException.class, () -> c.artifact(id, "../plan.toml"));
        Path p = c.download(id, "report.html", dir);
        assertEquals(dir.resolve(id + "-report.html"), p);
        assertEquals("rw-r--r--", PosixFilePermissions.toString(Files.getPosixFilePermissions(p)));
        Path custom = dir.resolve("custom.html");
        assertEquals(custom, c.download(id, "report.html", custom));
        c.download(id, "report.html", custom);
        try (var files = Files.list(dir)) {
            assertEquals(List.of(id + "-report.html", "custom.html").stream().sorted().toList(),
                    files.map(f -> f.getFileName().toString()).sorted().collect(Collectors.toList()), "no temporary files left");
        }
        assertThrows(RuntimeException.class, () -> c.download(id, "report.html", dir.resolve("missing/x.html")));
    }

    @Test
    void runConvenience() {
        Run done = c.run(PlanTest.shopPlan("http://127.0.0.1:8080/"), RunOptions.DEFAULT.withRegression(RegressionGate.builder().baseline("auto").build()));
        assertEquals(RunState.PASSED, done.state());
        assertEquals(40, done.summary().requests());
        assertEquals("auto", Fields.of(api.bodies.get("create"), "c").obj("regression").str("baseline"));
        assertEquals("auto", done.regression().baseline());
        assertEquals(RunState.QUEUED, c.run(done.bundle(), RunOptions.DEFAULT.withNoWait()).state());
        BundleInfo info = c.getBundle(done.bundle());
        assertEquals(info.id(), c.run(info, RunOptions.DEFAULT.withNoWait()).bundle());
        api.finalState = "failed";
        assertEquals(RunState.FAILED, c.run(new Bundle(PlanTest.shopPlan("http://127.0.0.1:8080/")), null).state());
        assertThrows(ConfigurationException.class, () -> c.run("not-a-bundle", null));
        assertThrows(ConfigurationException.class, () -> c.run(3.14, null));
    }
}
