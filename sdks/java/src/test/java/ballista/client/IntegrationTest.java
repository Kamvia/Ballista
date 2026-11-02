// File: ballista/sdks/java/src/test/java/ballista/client/IntegrationTest.java
package ballista.client;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import ballista.client.exceptions.PermissionDeniedException;
import ballista.client.exceptions.ValidationException;
import ballista.client.models.BundleInfo;
import ballista.client.models.CompareMetric;
import ballista.client.models.Comparison;
import ballista.client.models.Overrides;
import ballista.client.models.PlanCheck;
import ballista.client.models.Run;
import ballista.client.models.RunState;

import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Duration;
import java.util.List;
import java.util.Map;
import java.util.concurrent.atomic.AtomicInteger;

import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import static org.junit.jupiter.api.Assumptions.assumeTrue;

/** Requires BALLISTA_URL, BALLISTA_TOKEN (run scope), BALLISTA_READ_TOKEN and BALLISTA_DEMO_URL; skipped otherwise. */
class IntegrationTest {
    private Client c;
    private String demo;

    @BeforeEach
    void setUp() {
        for (String k : List.of("BALLISTA_URL", "BALLISTA_TOKEN", "BALLISTA_READ_TOKEN", "BALLISTA_DEMO_URL")) {
            assumeTrue(System.getenv(k) != null && !System.getenv(k).isEmpty(), "integration environment not configured");
        }
        c = Client.builder().timeout(Duration.ofMinutes(1)).build();
        demo = System.getenv("BALLISTA_DEMO_URL");
    }

    @Test
    void fullLifecycle(@TempDir Path dir) throws Exception {
        assertEquals("run", c.whoami().scope());
        PlanCheck check = c.validatePlan(PlanTest.shopPlan(demo));
        assertEquals(List.of(), check.files());
        BundleInfo info = c.uploadPlan(PlanTest.shopPlan(demo));
        assertEquals(info.id(), c.uploadBundle(new Bundle(PlanTest.shopPlan(demo))).id());
        RunOptions opts = RunOptions.of(Overrides.builder().runTime("5s").build()).withLabels(Map.of("build", "7"))
                .withIdempotencyKey("java-" + System.nanoTime());
        Run run = c.createRun(info, opts);
        assertEquals(run.id(), c.createRun(info, opts).id());
        AtomicInteger stats = new AtomicInteger();
        Run done = c.waitFor(run.id(), WaitOptions.timeout(Duration.ofSeconds(90)).withOnStats(s -> stats.incrementAndGet()));
        assertEquals(RunState.PASSED, done.state(), done.error());
        assertTrue(done.summary().requests() > 10);
        assertTrue(stats.get() > 0);
        assertTrue(c.getStats(run.id()).findRow("purchase", null).isPresent());
        Path html = c.download(run.id(), "report.html", dir);
        assertTrue(Files.readString(html).startsWith("<!DOCTYPE html>"));
        assertTrue(c.runs(RunState.PASSED, 50).anyMatch(r -> r.id().equals(run.id())));
        Comparison cmp = c.compare(run.id(), new CompareOptions(null, 10000.0, List.of(CompareMetric.P95, CompareMetric.ERROR_RATE)));
        assertTrue(cmp.baseline() == null || cmp.passed());
        assertNotNull(cmp.changes());
    }

    @Test
    void planTextReportsMissingAssets() {
        String text = "name = \"text\"\n[target]\nbase_url = \"" + demo + "\"\n[load]\nstages = [{ duration = \"1s\", target = 1 }]\n"
                + "[[scenario]]\nname = \"s\"\nscript = \"scenarios/s.rhai\"\n";
        assertEquals(List.of("scenarios/s.rhai"), c.validatePlan(text).files());
        assertThrows(ValidationException.class, () -> c.uploadPlan(text));
        Bundle b = new Bundle(text).addText("scenarios/s.rhai", "fn run(step, res) { if step == 0 { get(\"/items\") } }");
        Run done = c.run(b, RunOptions.of(Overrides.builder().runTime("2s").build()).withWait(WaitOptions.timeout(Duration.ofMinutes(1))));
        assertEquals(RunState.PASSED, done.state(), done.error());
    }

    @Test
    void stopCancelPolicyAndScopes() throws Exception {
        BundleInfo info = c.uploadPlan(PlanTest.shopPlan(demo));
        RunOptions slow = RunOptions.of(Overrides.builder().runTime("60s").build());
        Run first = c.createRun(info, slow);
        Run second = c.createRun(info, slow);
        assertEquals(RunState.CANCELLED, c.stopRun(second.id()).state());
        for (int i = 0; i < 100 && c.getRun(first.id()).state() == RunState.QUEUED; i++) {
            Thread.sleep(200);
        }
        c.stopRun(first.id());
        assertEquals(RunState.STOPPED, c.waitFor(first.id(), WaitOptions.timeout(Duration.ofMinutes(1))).state());
        ValidationException e = assertThrows(ValidationException.class, () -> c.validatePlan(PlanTest.shopPlan("http://10.0.0.1:8080/")));
        assertTrue(e.detail().contains("not permitted"), e.detail());
        assertThrows(ValidationException.class, () -> c.createRun(info,
                RunOptions.of(Overrides.builder().users(100_000).spawnRate(100_000).runTime("10s").build())));
        Client ro = Client.builder().token(System.getenv("BALLISTA_READ_TOKEN")).build();
        assertTrue(ro.status().limits().maxQueue() > 0);
        assertThrows(PermissionDeniedException.class, () -> ro.createRun(info, RunOptions.DEFAULT));
    }
}
