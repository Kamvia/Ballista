// File: ballista/sdks/java/src/test/java/ballista/client/PlanTest.java
package ballista.client;

import static ballista.client.plan.Steps.batch;
import static ballista.client.plan.Steps.feed;
import static ballista.client.plan.Steps.get;
import static ballista.client.plan.Steps.post;
import static ballista.client.plan.Steps.think;
import static ballista.client.plan.Steps.transaction;
import static org.junit.jupiter.api.Assertions.assertEquals;
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
import ballista.client.plan.Step;
import ballista.client.plan.Target;
import ballista.client.plan.Threshold;
import ballista.client.plan.Wait;

import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.function.Executable;

class PlanTest {
    static Plan shopPlan(String base) {
        List<Map<String, Object>> users = new ArrayList<>();
        for (int i = 1; i <= 5; i++) {
            users.add(Map.of("username", "user" + i, "password", "secret-pass"));
        }
        return Plan.builder("shop api")
                .target(Target.builder(base).header("accept", "application/json").build())
                .load(Load.users(Stage.of("2s", 4), Stage.of("4s", 4)))
                .scenario(Scenario.builder("buyer")
                        .waitBetween(Wait.uniform("50ms", "100ms"))
                        .onStart(feed("users"), post("/login")
                                .json(Map.of("username", "${users.username}", "password", "${users.password}"))
                                .capture("token", "json.token"))
                        .step(get("/items").name("list items").capture("item_id", "json.items.1.id"), think("20ms"))
                        .steps(transaction("purchase",
                                get("/items/${item_id}").name("item").contains("stock"),
                                post("/cart").name("cart").header("authorization", "Bearer ${token}")
                                        .json(Map.of("item", "${item_id}", "qty", 1)).expect(201, 503)))
                        .step(batch("page", 2, get("/items"), get("/slow").name("slow page")))
                        .build())
                .feeder(Feeder.rows("users", users))
                .threshold(Threshold.max(Metric.P95, 1000).on("purchase"))
                .percentiles(75, 99.9)
                .build();
    }

    @Test
    void buildsDocument() {
        String doc = Json.write(shopPlan("http://127.0.0.1:8080/").toDocument());
        for (String want : List.of("\"mode\":\"users\"", "\"txn_begin\":\"purchase\"", "\"expect\":[201,503]", "\"between\":[\"50ms\",\"100ms\"]",
                "\"metric\":\"p95\",\"request\":\"purchase\",\"max\":1000", "\"report\":{\"percentiles\":[75,99.9]}", "\"concurrency\":2")) {
            assertTrue(doc.contains(want), want + " in " + doc);
        }
    }

    @Test
    void rejectsInvalidPlans() {
        List<Executable> bad = List.of(
                () -> Target.of("javascript:alert(1)"),
                () -> Target.of("https://u:p@host/"),
                () -> Stage.of("forever", 1),
                () -> Stage.of("1s", -1),
                () -> Wait.fixed("soon"),
                () -> Scenario.builder("bad name").step(get("/")).build(),
                () -> Scenario.builder("s").build(),
                () -> Scenario.builder("s").source("fn run(s, r) {}").step(get("/")).build(),
                () -> Scenario.builder("s").step(get("/").capture("1x", "json.a")).build(),
                () -> Scenario.builder("s").step(get("/").retries(11)).build(),
                () -> Scenario.builder("s").step(batch("b", 0, get("/"))).build(),
                () -> Feeder.rows("f", List.of()),
                () -> Feeder.table("f", List.of("a", "b"), List.of(List.of(1))),
                () -> Plan.builder("p").target("http://x/").load(Load.users(Stage.of("1s", 1))).build(),
                () -> Plan.builder("p").target("http://x/").load(Load.users(Stage.of("1s", 1)))
                        .scenario(Scenario.builder("s").step(get("/")).build(), Scenario.builder("s").step(get("/")).build()).build(),
                () -> Plan.builder("p").target("http://x/").load(Load.users(Stage.of("1s", 1)))
                        .scenario(Scenario.builder("s").step(get("/")).build()).percentiles(0).build());
        for (int i = 0; i < bad.size(); i++) {
            assertThrows(ConfigurationException.class, bad.get(i), "case " + i);
        }
    }

    @Test
    void stepsAreImmutable() {
        Step base = get("/x").header("a", "1");
        Step derived = base.header("b", "2");
        String doc = Json.write(Plan.builder("p").target("http://x/").load(Load.users(Stage.of("1s", 1)))
                .scenario(Scenario.builder("s").step(base).build()).build().toDocument());
        assertTrue(doc.contains("\"headers\":{\"a\":\"1\"}"), doc);
        assertFalse(doc.contains("\"b\""));
        assertEquals("Step[get, headers]", derived.toString());
    }
}
