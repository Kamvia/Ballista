// File: ballista/sdks/java/src/main/java/ballista/client/plan/Step.java
package ballista.client.plan;

import ballista.client.internal.Rules;

import java.util.ArrayList;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * One declarative action. Steps are immutable: every setter returns a new step. Build them with {@link Steps}.
 */
public final class Step {
    private static final List<String> METHODS = List.of("get", "post", "put", "patch", "delete", "head");
    private static final List<String> ACTIONS = List.of("think", "feed", "set", "txn_begin", "txn_end", "batch");

    private final Map<String, Object> fields;

    Step(Map<String, Object> fields) {
        this.fields = Collections.unmodifiableMap(new LinkedHashMap<>(fields));
    }

    static Step of(String key, Object value) {
        if (value == null) {
            throw Rules.fail("step " + key + " needs a value");
        }
        return new Step(Map.of(key, value));
    }

    private Step with(String key, Object value) {
        if (value == null) {
            throw Rules.fail("step " + key + " needs a value");
        }
        Map<String, Object> m = new LinkedHashMap<>(fields);
        m.put(key, value);
        return new Step(m);
    }

    @SuppressWarnings("unchecked")
    private Step entry(String key, String k, String v) {
        if (k == null || k.isEmpty() || v == null) {
            throw Rules.fail("step " + key + " entries need a non-empty name and a value");
        }
        Map<String, String> m = new LinkedHashMap<>((Map<String, String>) fields.getOrDefault(key, Map.of()));
        m.put(k, v);
        return with(key, Collections.unmodifiableMap(m));
    }

    public Step name(String n) { return with("name", n); }
    public Step header(String k, String v) { return entry("headers", k, v); }
    public Step query(String k, String v) { return entry("query", k, v); }
    /** JSON body: maps, lists, strings, numbers, booleans and null. */
    public Step json(Object body) { return with("json", body); }
    public Step body(String raw) { return with("body", raw); }
    public Step form(String k, String v) { return entry("form", k, v); }
    public Step expect(int... codes) {
        List<Long> out = new ArrayList<>();
        for (int c : codes) {
            out.add((long) c);
        }
        return with("expect", List.copyOf(out));
    }
    public Step contains(String text) { return with("contains", text); }
    /** Stores a value from the response (e.g. json.items.0.id) in a user variable. */
    public Step capture(String variable, String expr) { return entry("capture", variable, expr); }
    public Step timeout(String d) { return with("timeout", d); }
    public Step retries(int n) { return with("retries", (long) n); }
    public Step continueOnFailure() { return with("continue_on_failure", true); }
    /** Reads this many server-sent events from the response. */
    public Step sse(int events) { return with("sse", (long) events); }
    public Step reconnects(int n) { return with("reconnects", (long) n); }
    public Step context(String k, String v) { return entry("context", k, v); }

    @SuppressWarnings("unchecked")
    void check(String where) {
        long methods = METHODS.stream().filter(fields::containsKey).count();
        long actions = Math.min(methods, 1) + ACTIONS.stream().filter(fields::containsKey).count();
        if (methods > 1 || actions != 1) {
            throw Rules.fail(where + ": a step does exactly one thing (request, think, feed, set, transaction boundary or batch)");
        }
        for (String key : List.of("capture", "set")) {
            for (String k : ((Map<String, String>) fields.getOrDefault(key, Map.of())).keySet()) {
                Rules.ident(k, where);
            }
        }
        if (fields.get("timeout") instanceof String t) {
            Rules.duration(t, where);
        }
        switch (fields.get("think")) {
            case null -> { }
            case String d -> Rules.duration(d, where);
            case List<?> l -> l.forEach(d -> Rules.duration((String) d, where));
            default -> throw Rules.fail(where + ": think takes one or two durations");
        }
        for (String key : List.of("txn_begin", "txn_end")) {
            if (fields.get(key) instanceof String n) {
                Rules.name(n, where);
            }
        }
        if (fields.get("retries") instanceof Long r && (r < 0 || r > 10)) {
            throw Rules.fail(where + ": retries must be 0..10");
        }
        if (fields.get("batch") instanceof List<?> batch) {
            if (!(fields.get("concurrency") instanceof Long c) || c < 1 || c > 64) {
                throw Rules.fail(where + ": concurrency must be 1..64");
            }
            for (int i = 0; i < batch.size(); i++) {
                ((Step) batch.get(i)).check(where + " batch[" + i + "]");
            }
        }
    }

    Map<String, Object> toJson() {
        Map<String, Object> m = new LinkedHashMap<>(fields);
        if (m.get("batch") instanceof List<?> batch) {
            m.put("batch", batch.stream().map(s -> ((Step) s).toJson()).toList());
        }
        return m;
    }

    @Override
    public String toString() {
        return "Step" + fields.keySet();
    }
}
