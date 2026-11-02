// File: ballista/sdks/java/src/main/java/ballista/client/plan/Steps.java
package ballista.client.plan;

import ballista.client.internal.Rules;

import java.util.ArrayList;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/** Factories for declarative steps. */
public final class Steps {
    private Steps() {
    }

    public static Step get(String path) { return Step.of("get", path); }
    public static Step post(String path) { return Step.of("post", path); }
    public static Step put(String path) { return Step.of("put", path); }
    public static Step patch(String path) { return Step.of("patch", path); }
    public static Step delete(String path) { return Step.of("delete", path); }
    public static Step head(String path) { return Step.of("head", path); }

    public static Step think(String d) { return Step.of("think", d); }

    /** Uniform random pause between low and high. */
    public static Step think(String low, String high) { return Step.of("think", List.of(low, high)); }

    /** Takes the next row of a feeder into the user's variables. */
    public static Step feed(String feeder) { return Step.of("feed", feeder); }

    /** Sets variables from templates. */
    public static Step assign(Map<String, String> vars) { return Step.of("set", Collections.unmodifiableMap(new LinkedHashMap<>(vars))); }

    /** Sends requests in parallel, like a browser loading a page. */
    public static Step batch(String name, int concurrency, Step... requests) {
        Rules.name(name, "batch");
        Map<String, Object> m = new LinkedHashMap<>();
        m.put("batch", List.of(requests));
        m.put("name", name);
        m.put("concurrency", (long) concurrency);
        return new Step(m);
    }

    /** Wraps steps so their total duration is recorded as one transaction. */
    public static List<Step> transaction(String name, Step... steps) {
        List<Step> out = new ArrayList<>(steps.length + 2);
        out.add(Step.of("txn_begin", name));
        out.addAll(List.of(steps));
        out.add(Step.of("txn_end", name));
        return List.copyOf(out);
    }
}
