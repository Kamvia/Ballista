// File: ballista/sdks/java/src/main/java/ballista/client/plan/Threshold.java
package ballista.client.plan;

import ballista.client.internal.Rules;

import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.Map;

/** Pass/fail criterion on a metric, for every request ("*") or a named request or transaction. */
public final class Threshold {
    private final Map<String, Object> json;

    private Threshold(Map<String, Object> json) {
        this.json = Collections.unmodifiableMap(new LinkedHashMap<>(json));
    }

    private static Threshold bound(Metric metric, String key, double value) {
        if (!Rules.finite(value)) {
            throw Rules.fail("threshold " + metric.wire() + " needs a finite bound");
        }
        Map<String, Object> m = new LinkedHashMap<>();
        m.put("metric", metric.wire());
        m.put("request", "*");
        m.put(key, value);
        return new Threshold(m);
    }

    public static Threshold max(Metric metric, double value) {
        return bound(metric, "max", value);
    }

    public static Threshold min(Metric metric, double value) {
        return bound(metric, "min", value);
    }

    private Threshold with(String k, Object v) {
        Map<String, Object> m = new LinkedHashMap<>(json);
        m.put(k, v);
        return new Threshold(m);
    }

    public Threshold on(String request) { return with("request", request); }
    public Threshold abortOnFail() { return with("abort_on_fail", true); }
    public Threshold abortDelay(String d) { return with("abort_delay", Rules.duration(d, "threshold.abort_delay")); }

    Map<String, Object> toJson() {
        return new LinkedHashMap<>(json);
    }
}
