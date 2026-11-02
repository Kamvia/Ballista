// File: ballista/sdks/java/src/main/java/ballista/client/plan/Load.java
package ballista.client.plan;

import ballista.client.internal.Rules;

import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/** Load profile: stages, or an inline shape script. Immutable; with-methods return a copy. */
public final class Load {
    public enum Mode {
        USERS("users"),
        ARRIVAL_RATE("arrival_rate");

        private final String wire;

        Mode(String wire) {
            this.wire = wire;
        }
    }

    private final Map<String, Object> json;

    private Load(Map<String, Object> json) {
        this.json = Collections.unmodifiableMap(new LinkedHashMap<>(json));
    }

    private static Load stages(Mode mode, Stage... stages) {
        if (stages.length == 0) {
            throw Rules.fail("load needs at least one stage");
        }
        Map<String, Object> m = new LinkedHashMap<>();
        m.put("mode", mode.wire);
        m.put("stages", java.util.Arrays.stream(stages).map(Stage::toJson).toList());
        return new Load(m);
    }

    /** Closed model: each stage ramps to that many concurrent users. */
    public static Load users(Stage... stages) {
        return stages(Mode.USERS, stages);
    }

    /** Open model: each stage ramps to that many iterations per second. */
    public static Load arrivalRate(Stage... stages) {
        return stages(Mode.ARRIVAL_RATE, stages);
    }

    /** Custom profile from a script defining fn shape(t); the source travels inside the plan. */
    public static Load shape(String source, Mode mode) {
        if (source == null || source.isBlank()) {
            throw Rules.fail("shape source is empty");
        }
        Map<String, Object> m = new LinkedHashMap<>();
        m.put("mode", mode.wire);
        m.put("shape_source", source);
        return new Load(m);
    }

    private Load with(String k, Object v) {
        Map<String, Object> m = new LinkedHashMap<>(json);
        m.put(k, v);
        return new Load(m);
    }

    public Load gracefulStop(String d) { return with("graceful_stop", Rules.duration(d, "load.graceful_stop")); }
    public Load maxUsers(double n) { return with("max_users", n); }
    public Load iterations(long n) { return with("iterations", n); }
    public Load tags(String... t) { Rules.names(List.of(t), 64, "load.tags"); return with("tags", List.of(t)); }
    public Load excludeTags(String... t) { Rules.names(List.of(t), 64, "load.exclude_tags"); return with("exclude_tags", List.of(t)); }

    Map<String, Object> toJson() {
        return new LinkedHashMap<>(json);
    }
}
