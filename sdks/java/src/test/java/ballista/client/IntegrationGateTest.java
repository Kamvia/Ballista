// File: ballista/sdks/java/src/test/java/ballista/client/IntegrationGateTest.java
package ballista.client;

import static ballista.client.plan.Steps.get;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;
import static org.junit.jupiter.api.Assumptions.assumeTrue;

import ballista.client.exceptions.ConflictException;
import ballista.client.exceptions.NotFoundException;
import ballista.client.exceptions.PermissionDeniedException;
import ballista.client.models.BundleInfo;
import ballista.client.models.CompareMetric;
import ballista.client.models.Comparison;
import ballista.client.models.Overrides;
import ballista.client.models.RegressionGate;
import ballista.client.models.Run;
import ballista.client.models.RunState;
import ballista.client.plan.Load;
import ballista.client.plan.Plan;
import ballista.client.plan.Scenario;
import ballista.client.plan.Stage;
import ballista.client.plan.Target;
import ballista.client.plan.Wait;

import java.time.Duration;
import java.util.List;
import java.util.Map;

import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;

/** Requires BALLISTA_URL, BALLISTA_TOKEN, BALLISTA_READ_TOKEN and BALLISTA_DEMO_URL; deletion also BALLISTA_ADMIN_TOKEN. */
class IntegrationGateTest {
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

    /** A unique plan name keeps the automatic baseline inside this test. */
    private Plan plan(String name, String path) {
        return Plan.builder(name).target(Target.builder(demo).timeout("5s").build()).load(Load.users(Stage.of("1s", 10), Stage.of("4s", 10)))
                .scenario(Scenario.builder("detail").waitBetween(Wait.fixed("20ms")).step(get(path).name("item detail").expect(200)).build())
                .build();
    }

    @Test
    void regressionGateAcrossRuns() {
        String name = "java gate " + System.nanoTime();
        RegressionGate gate = RegressionGate.builder().maxRegressionPct(50).minChangeMs(5).metrics(CompareMetric.P95, CompareMetric.ERROR_RATE).build();
        RunOptions local = RunOptions.of(Overrides.builder().workers(0).build()).withRegression(gate).withWait(WaitOptions.timeout(Duration.ofMinutes(1)));

        Run first = c.run(plan(name, "/items/1"), local);
        assertEquals(RunState.PASSED, first.state(), first.error());
        assertNull(first.comparison().baseline());
        assertNotNull(first.comparison().note());
        assertEquals(50.0, first.regression().maxRegressionPct());

        int workers = c.status().workers().size();
        RunOptions spread = workers > 0 ? local.withOverrides(Overrides.builder().workers(workers).build()) : local;
        Run second = c.run(plan(name, "/items/1"), spread);
        assertEquals(RunState.PASSED, second.state(), second.error());
        assertEquals(first.id(), second.comparison().baseline());
        assertTrue(second.comparison().passed());
        if (workers > 0) {
            assertEquals(workers, second.workers());
        }

        Run slow = c.run(plan(name, "/slow"), local);
        assertEquals(RunState.FAILED, slow.state());
        Comparison cmp = slow.comparison();
        assertFalse(cmp.passed());
        assertTrue(cmp.regressions() > 0);
        assertEquals(second.id(), cmp.baseline());
        assertTrue(slow.error().contains("regression"), slow.error());
        assertTrue(cmp.changes().getFirst().regressed());

        Comparison wide = c.compare(slow.id(), new CompareOptions(first.id(), 10000.0, null));
        assertEquals(first.id(), wide.baseline());
        assertTrue(wide.passed());
    }

    @Test
    void compareRejectsUnfinishedRun() {
        Run run = c.run(plan("java unfinished", "/items/1"), RunOptions.of(Overrides.builder().runTime("60s").build()).withNoWait());
        try {
            assertThrows(ConflictException.class, () -> c.compare(run.id(), null));
            assertThrows(NotFoundException.class, () -> c.compare("r000000000000000000000000", null));
        } finally {
            c.stopRun(run.id());
            c.waitFor(run.id(), WaitOptions.timeout(Duration.ofMinutes(1)));
        }
    }

    @Test
    void adminDeletion() {
        String admin = System.getenv("BALLISTA_ADMIN_TOKEN");
        assumeTrue(admin != null && !admin.isEmpty(), "BALLISTA_ADMIN_TOKEN not set");
        Plan p = plan("java delete " + System.nanoTime(), "/items/1");
        BundleInfo info = c.uploadBundle(new Bundle(p, Map.of("purpose", "deletion")));
        assertTrue(c.listBundles().stream().anyMatch(b -> b.id().equals(info.id())));
        assertEquals("deletion", c.getBundle(info.id()).labels().get("purpose"));
        assertEquals(p.name(), c.getBundle(info.id()).planName());
        Run run = c.run(info, RunOptions.of(Overrides.builder().runTime("1s").build()).withWait(WaitOptions.timeout(Duration.ofMinutes(1))));
        assertTrue(run.isFinished());
        assertThrows(PermissionDeniedException.class, () -> c.deleteRun(run.id()));
        Client ac = Client.builder().token(admin).build();
        ac.deleteRun(run.id());
        assertThrows(NotFoundException.class, () -> c.getRun(run.id()));
        ac.deleteBundle(info.id());
        assertThrows(NotFoundException.class, () -> c.getBundle(info.id()));
    }

    @Test
    void stopDuringStartup() throws Exception {
        var st = c.status();
        assumeTrue(!st.workers().isEmpty() && st.activeRun() == null && st.queued() == 0, "needs an idle server with pool workers");
        // More workers than are connected keeps the run in its startup phase while the server waits for them.
        Run run = c.run(plan("java startup stop", "/items/1"), RunOptions.of(Overrides.builder().workers(st.workers().size() + 5).build()).withNoWait());
        Thread.sleep(500);
        Run stopped = c.stopRun(run.id());
        assertTrue(stopped.state() == RunState.STOPPING || stopped.state() == RunState.CANCELLED, stopped.state().wire());
        long start = System.nanoTime();
        Run done = c.waitFor(run.id(), WaitOptions.timeout(Duration.ofSeconds(30)).withPollInterval(Duration.ofMillis(100)));
        assertEquals(RunState.CANCELLED, done.state());
        assertNotNull(done.stoppedBy());
        assertNull(done.startedAt());
        assertTrue(Duration.ofNanos(System.nanoTime() - start).toSeconds() < 5, "cancellation must not wait for the worker timeout");
    }
}
