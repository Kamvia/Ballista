// File: ballista/sdks/java/src/test/java/ballista/client/ModelsTest.java
package ballista.client;

import static org.junit.jupiter.api.Assertions.assertDoesNotThrow;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import ballista.client.exceptions.ConfigurationException;
import ballista.client.exceptions.ResponseException;
import ballista.client.internal.Fields;
import ballista.client.internal.Json;
import ballista.client.internal.Rules;
import ballista.client.models.CompareMetric;
import ballista.client.models.Overrides;
import ballista.client.models.RegressionGate;
import ballista.client.models.Run;
import ballista.client.models.RunState;
import ballista.client.models.Snapshot;

import java.util.HashMap;
import java.util.List;
import java.util.Map;

import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.function.Executable;

class ModelsTest {
    @Test
    void runStates() {
        for (RunState s : RunState.values()) {
            assertEquals(s, RunState.fromWire(s.wire()));
            assertEquals(s.wire(), s.toString());
        }
        assertFalse(RunState.QUEUED.isTerminal());
        assertFalse(RunState.STOPPING.isTerminal());
        assertTrue(RunState.CANCELLED.isTerminal());
        assertTrue(RunState.ERROR.isTerminal());
        assertThrows(ResponseException.class, () -> RunState.fromWire("paused"));
        assertThrows(ResponseException.class, () -> CompareMetric.fromWire("p42"));
    }

    @Test
    void overridesValidation() {
        assertDoesNotThrow(() -> Overrides.builder().users(1).spawnRate(0.5).runTime("1h 30m").iterations(1).workers(0)
                .tags("read", "a.b-c_d").excludeTags("slow").host("https://shop.example/base").build());
        assertEquals(512, Overrides.builder().workers(512).build().workers());
        String[] many = new String[65];
        java.util.Arrays.setAll(many, i -> "t" + i);
        List<Executable> bad = List.of(
                () -> Overrides.builder().users(0).build(), () -> Overrides.builder().users(Double.NaN).build(),
                () -> Overrides.builder().users(Double.POSITIVE_INFINITY).build(), () -> Overrides.builder().spawnRate(-1).build(),
                () -> Overrides.builder().iterations(0).build(), () -> Overrides.builder().workers(-1).build(),
                () -> Overrides.builder().workers(513).build(), () -> Overrides.builder().runTime("10").build(),
                () -> Overrides.builder().runTime("5s; rm -rf").build(), () -> Overrides.builder().tags("has space").build(),
                () -> Overrides.builder().excludeTags("").build(), () -> Overrides.builder().tags(many).build(),
                () -> Overrides.builder().host("ftp://x").build(), () -> Overrides.builder().host("https://u:p@x").build(),
                () -> Overrides.builder().host("https://x/ y").build());
        for (int i = 0; i < bad.size(); i++) {
            assertThrows(ConfigurationException.class, bad.get(i), "case " + i);
        }
    }

    @Test
    void overridesRoundTrip() {
        Overrides o = Overrides.builder().users(10).runTime("30s").tags("a").workers(2).build();
        Map<String, Object> json = o.toJson();
        assertEquals(List.of("users", "run_time", "tags", "workers"), List.copyOf(json.keySet()));
        assertEquals(o, Overrides.from(Json.parse(Json.write(json))));
        assertEquals(Overrides.NONE, Overrides.from(null));
        assertTrue(Overrides.NONE.toJson().isEmpty());
    }

    @Test
    void gateValidationAndJson() {
        RegressionGate g = RegressionGate.builder().baseline("r0123456789abcdef01234567").maxRegressionPct(0).metrics(CompareMetric.values())
                .minSamples(0).minChangeMs(0).maxErrorRateIncrease(1).build();
        assertEquals(6, ((List<?>) g.toJson().get("metrics")).size());
        assertEquals(g, RegressionGate.from(Json.parse(Json.write(g.toJson()))));
        assertTrue(RegressionGate.builder().build().toJson().isEmpty());
        List<Executable> bad = List.of(
                () -> RegressionGate.builder().baseline("latest").build(), () -> RegressionGate.builder().baseline("r0123").build(),
                () -> RegressionGate.builder().maxRegressionPct(-0.1).build(), () -> RegressionGate.builder().maxRegressionPct(10_001).build(),
                () -> RegressionGate.builder().maxRegressionPct(Double.NaN).build(), () -> RegressionGate.builder().minChangeMs(-1).build(),
                () -> RegressionGate.builder().maxErrorRateIncrease(1.5).build(), () -> RegressionGate.builder().minSamples(-1).build(),
                () -> RegressionGate.builder().metrics().build());
        for (int i = 0; i < bad.size(); i++) {
            assertThrows(ConfigurationException.class, bad.get(i), "case " + i);
        }
    }

    @Test
    void labelRules() {
        assertDoesNotThrow(() -> Rules.labels(Map.of("build", "ü".repeat(256), "a.b-c_d", "")));
        Map<String, String> many = new HashMap<>();
        for (int i = 0; i < 33; i++) {
            many.put("k" + i, "v");
        }
        for (Map<String, String> bad : List.of(many, Map.of("", "v"), Map.of("bad key", "v"), Map.of("k".repeat(65), "v"),
                Map.of("k", "v".repeat(257)), Map.of("k", "line\nbreak"), Map.of("k", "tab\there"), Map.of("k", "del\u007f"))) {
            assertThrows(ConfigurationException.class, () -> Rules.labels(bad));
        }
    }

    @Test
    void runParsing() {
        String full = "{\"id\":\"r0123456789abcdef01234567\",\"seq\":3,\"bundle\":\"b\",\"plan_name\":\"p\",\"state\":\"failed\",\"created_at\":\"t\","
                + "\"created_by\":\"ci\",\"started_at\":\"s\",\"ended_at\":\"e\",\"labels\":{\"k\":\"v\"},\"overrides\":{\"workers\":2},\"passed\":false,"
                + "\"error\":\"1 regression(s)\",\"workers\":2,\"queue_position\":null,\"regression\":{\"baseline\":\"auto\"},"
                + "\"summary\":{\"requests\":5,\"thresholds\":[{\"metric\":\"p95\",\"request\":\"*\",\"max\":10,\"actual\":null,\"passed\":true}]},"
                + "\"comparison\":{\"baseline\":null,\"passed\":false,\"regressions\":1,\"changes\":[{\"kind\":\"total\",\"method\":\"\","
                + "\"name\":\"Aggregated\",\"metric\":\"error_rate\",\"baseline\":0,\"current\":0.5,\"change_pct\":null,\"regressed\":true}],\"unmatched\":[]}}";
        Run r = Run.from(Json.parse(full));
        assertEquals(RunState.FAILED, r.state());
        assertTrue(r.isFinished());
        assertEquals(Boolean.FALSE, r.passed());
        assertEquals(2, r.workers());
        assertNull(r.queuePosition());
        assertEquals("auto", r.regression().baseline());
        assertNull(r.summary().thresholds().getFirst().actual());
        assertNull(r.comparison().baseline());
        assertNull(r.comparison().changes().getFirst().changePct());
        assertEquals(CompareMetric.ERROR_RATE, r.comparison().changes().getFirst().metric());
        assertEquals(Map.of("k", "v"), r.labels());
        assertThrows(UnsupportedOperationException.class, () -> r.labels().put("x", "y"));
        String minimal = "{\"id\":\"r0123456789abcdef01234567\",\"seq\":1,\"bundle\":\"b\",\"plan_name\":\"p\",\"state\":\"queued\",\"created_at\":\"t\",\"created_by\":\"c\"}";
        Run m = Run.from(Json.parse(minimal));
        assertEquals(Map.of(), m.labels());
        assertEquals(Overrides.NONE, m.overrides());
        assertNull(m.summary());
        assertNull(m.comparison());
        for (String bad : List.of("[]", "{\"id\":5}", minimal.replace("\"seq\":1", "\"seq\":1.5"), minimal.replace("\"seq\":1", "\"seq\":\"1\""),
                minimal.replace("}", ",\"labels\":{\"k\":1}}"), minimal.replace("}", ",\"passed\":\"yes\"}"))) {
            assertThrows(ResponseException.class, () -> Run.from(Json.parse(bad)), bad);
        }
    }

    @Test
    void snapshotDefaults() {
        Snapshot s = Snapshot.from(Json.parse("{}"));
        assertEquals("", s.plan());
        assertEquals(0, s.total().count());
        assertTrue(s.rows().isEmpty());
        assertTrue(s.findRow("x", null).isEmpty());
        Fields f = Fields.of(Json.parse("{\"n\":1e300,\"s\":\"x\",\"l\":[1,\"a\"]}"), "f");
        assertEquals(1e300, f.num("n"));
        assertThrows(ResponseException.class, () -> f.lng("n"));
        assertThrows(ResponseException.class, () -> f.numList("l"));
        assertThrows(ResponseException.class, () -> f.obj("s"));
        assertEquals(0, f.lng("missing"));
        assertNull(f.optStr("missing"));
    }
}
