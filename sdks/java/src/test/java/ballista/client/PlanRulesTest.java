// File: ballista/sdks/java/src/test/java/ballista/client/PlanRulesTest.java
package ballista.client;

import static ballista.client.plan.Steps.assign;
import static ballista.client.plan.Steps.batch;
import static ballista.client.plan.Steps.delete;
import static ballista.client.plan.Steps.feed;
import static ballista.client.plan.Steps.get;
import static ballista.client.plan.Steps.head;
import static ballista.client.plan.Steps.patch;
import static ballista.client.plan.Steps.post;
import static ballista.client.plan.Steps.put;
import static ballista.client.plan.Steps.think;
import static ballista.client.plan.Steps.transaction;
import static org.junit.jupiter.api.Assertions.assertDoesNotThrow;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import ballista.client.exceptions.ConfigurationException;
import ballista.client.internal.Json;
import ballista.client.plan.Feeder;
import ballista.client.plan.Load;
import ballista.client.plan.Metric;
import ballista.client.plan.Plan;
import ballista.client.plan.Scenario;
import ballista.client.plan.Stage;
import ballista.client.plan.Target;
import ballista.client.plan.Threshold;
import ballista.client.plan.Wait;

import java.util.List;
import java.util.Map;

import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.function.Executable;

class PlanRulesTest {
    private static Plan.Builder base() {
        return Plan.builder("p").target("http://127.0.0.1:8080/").load(Load.users(Stage.of("1s", 1)))
                .scenario(Scenario.builder("s").step(get("/")).build());
    }

    private static String doc(Plan p) {
        return Json.write(p.toDocument());
    }

    private static void contains(String doc, String... wants) {
        for (String w : wants) {
            assertTrue(doc.contains(w), w + " in " + doc);
        }
    }

    @Test
    void loadModels() {
        contains(doc(base().load(Load.arrivalRate(Stage.of("10s", 50), Stage.of("1m", 50)).maxUsers(500)).build()),
                "\"mode\":\"arrival_rate\"", "\"max_users\":500");
        String shape = doc(base().load(Load.shape("fn shape(t) { 5.0 }", Load.Mode.USERS).gracefulStop("3s").tags("a").excludeTags("b").iterations(3)).build());
        contains(shape, "\"mode\":\"users\",\"shape_source\":\"fn shape(t) { 5.0 }\"", "\"graceful_stop\":\"3s\"", "\"iterations\":3", "\"exclude_tags\":[\"b\"]");
        assertFalse(shape.contains("stages"));
        List<Executable> bad = List.of(() -> Load.users(), () -> Load.shape(" ", Load.Mode.USERS), () -> Stage.of("1s", Double.NaN),
                () -> Load.users(Stage.of("1s", 1)).gracefulStop("later"), () -> Load.users(Stage.of("1s", 1)).tags("no spaces"));
        bad.forEach(e -> assertThrows(ConfigurationException.class, e));
    }

    @Test
    void waitKinds() {
        for (Wait w : List.of(Wait.uniform("1s", "2s"), Wait.fixed("1s"), Wait.pace("3s"), Wait.perSecond(2), Wait.exponential("500ms"))) {
            assertDoesNotThrow(() -> base().scenario(Scenario.builder("w").waitBetween(w).step(get("/")).build()).build());
        }
        List<Executable> bad = List.of(() -> Wait.uniform("1s", "x"), () -> Wait.perSecond(0), () -> Wait.perSecond(Double.NaN),
                () -> Wait.pace(null), () -> Wait.exponential("soon"));
        bad.forEach(e -> assertThrows(ConfigurationException.class, e));
    }

    @Test
    void scenarioAndStepRules() {
        List<Executable> bad = List.of(
                () -> Scenario.builder("s").weight(10_001),
                () -> Scenario.builder("s").weight(0),
                () -> Scenario.builder("s").tags("no spaces"),
                () -> Scenario.builder("s").script("s.rhai").step(get("/")).build(),
                () -> Scenario.builder("s").steps(transaction("bad name", get("/"))).build(),
                () -> Scenario.builder("s").step(get("/").timeout("1 week")).build(),
                () -> Scenario.builder("s").step(assign(Map.of("a-b", "x"))).build(),
                () -> Scenario.builder("s").step(batch("b", 2, get("/"), think("x"))).build(),
                () -> Scenario.builder("s").step(batch("b", 65, get("/"))).build(),
                () -> Scenario.builder("s").onStart(get("/").capture("1x", "json.a")).step(get("/")).build(),
                () -> get(null),
                () -> get("/").header("k", null),
                () -> batch("bad name", 2, get("/")),
                () -> Threshold.max(Metric.P95, Double.POSITIVE_INFINITY),
                () -> Threshold.max(Metric.P95, 1).abortDelay("x"),
                () -> Target.builder("http://x/").timeout("x"),
                () -> Target.builder("http://x/").connectTimeout("1 fortnight"),
                () -> Plan.builder("").target("http://x/").load(Load.users(Stage.of("1s", 1))).scenario(Scenario.builder("s").step(get("/")).build()).build(),
                () -> Plan.builder("n".repeat(129)).target("http://x/").load(Load.users(Stage.of("1s", 1))).scenario(Scenario.builder("s").step(get("/")).build()).build(),
                () -> Plan.builder("p").load(Load.users(Stage.of("1s", 1))).scenario(Scenario.builder("s").step(get("/")).build()).build(),
                () -> base().percentiles(new double[13]).build(),
                () -> base().percentiles(100.1).build());
        for (int i = 0; i < bad.size(); i++) {
            assertThrows(ConfigurationException.class, bad.get(i), "case " + i);
        }
    }

    @Test
    void stepKinds() {
        Plan p = base().scenario(Scenario.builder("all").weight(3).fixedCount(2).tags("read")
                .step(get("/a"), post("/b").body("raw").form("f", "1"), put("/c"), patch("/d"), delete("/e"), head("/f"))
                .step(think("1s", "2s"), feed("users"), assign(Map.of("x", "${y}")))
                .step(get("/s").sse(3).reconnects(2).continueOnFailure().query("q", "1").context("region", "eu").retries(0))
                .steps(transaction("t", get("/g")))
                .build())
                .feeder(Feeder.table("users", List.of("name", "n"), List.of(List.of("a", 1), List.of("b", 2))).random(), Feeder.file("ids", "data/ids.csv"))
                .threshold(Threshold.min(Metric.RPS, 5).abortOnFail().abortDelay("10s"))
                .build();
        contains(doc(p), "\"put\":\"/c\"", "\"patch\":\"/d\"", "\"delete\":\"/e\"", "\"head\":\"/f\"", "\"body\":\"raw\"", "\"form\":{\"f\":\"1\"}",
                "\"think\":[\"1s\",\"2s\"]", "\"set\":{\"x\":\"${y}\"}", "\"sse\":3", "\"reconnects\":2", "\"continue_on_failure\":true",
                "\"query\":{\"q\":\"1\"}", "\"context\":{\"region\":\"eu\"}", "\"retries\":0", "\"txn_end\":\"t\"", "\"weight\":3", "\"fixed_count\":2",
                "\"columns\":[\"name\",\"n\"]", "\"mode\":\"random\"", "\"path\":\"data/ids.csv\"", "\"min\":5", "\"abort_on_fail\":true");
    }

    @Test
    void targetDocument() {
        Target t = Target.builder("https://shop.example/").timeout("5s").connectTimeout("2s").retries(0).retryBackoff("100ms").retryOn(502, 503)
                .allowedHosts("cdn.shop.example").cookies(true).maxBodyBytes(1 << 20).http2PriorKnowledge(true).caCert("certs/ca.pem").insecureTls(true).build();
        contains(doc(base().target(t).build()), "\"retries\":0", "\"retry_on\":[502,503]", "\"insecure_tls\":true", "\"ca_cert\":\"certs/ca.pem\"",
                "\"http2_prior_knowledge\":true", "\"allowed_hosts\":[\"cdn.shop.example\"]", "\"max_body_bytes\":1048576");
        for (String u : new String[] {null, "", "shop.example", "file:///etc/passwd", "https://a:b@shop.example/", "https:// shop.example", "javascript:alert(1)"}) {
            assertThrows(ConfigurationException.class, () -> Target.of(u), String.valueOf(u));
        }
    }

    @Test
    void documentIsDeterministicAndDetached() {
        String a = doc(PlanTest.shopPlan("http://127.0.0.1:8080/"));
        String b = doc(PlanTest.shopPlan("http://127.0.0.1:8080/"));
        assertTrue(a.equals(b), "same plan, same document");
        Plan p = base().build();
        p.toDocument().put("name", "changed");
        contains(doc(p), "\"name\":\"p\"");
        contains(doc(base().threshold(Threshold.max(Metric.ERROR_RATE, 0)).build()), "\"request\":\"*\"", "\"max\":0");
    }
}
