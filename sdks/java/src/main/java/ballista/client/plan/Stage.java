// File: ballista/sdks/java/src/main/java/ballista/client/plan/Stage.java
package ballista.client.plan;

import ballista.client.internal.Rules;

import java.util.LinkedHashMap;
import java.util.Map;

/** Ramps to target (users, or iterations per second) over duration. */
public record Stage(String duration, double target) {
    public Stage {
        Rules.duration(duration, "load.stages");
        if (!Rules.finite(target) || target < 0) {
            throw Rules.fail("stage targets must be non-negative");
        }
    }

    public static Stage of(String duration, double target) {
        return new Stage(duration, target);
    }

    Map<String, Object> toJson() {
        Map<String, Object> m = new LinkedHashMap<>();
        m.put("duration", duration);
        m.put("target", target);
        return m;
    }
}
