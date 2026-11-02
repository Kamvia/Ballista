// File: ballista/sdks/java/src/main/java/ballista/client/plan/Wait.java
package ballista.client.plan;

import ballista.client.internal.Rules;

import java.util.List;
import java.util.Map;

/** Pause between iterations of a scenario. */
public final class Wait {
    private final Map<String, Object> json;

    private Wait(String kind, Object value) {
        this.json = Map.of(kind, value);
    }

    /** Uniform random pause between low and high. */
    public static Wait uniform(String low, String high) {
        return new Wait("between", List.of(Rules.duration(low, "wait"), Rules.duration(high, "wait")));
    }

    public static Wait fixed(String d) {
        return new Wait("constant", Rules.duration(d, "wait"));
    }

    /** Starts iterations at a fixed interval regardless of how long they take. */
    public static Wait pace(String d) {
        return new Wait("pacing", Rules.duration(d, "wait"));
    }

    /** Each user runs this many iterations per second. */
    public static Wait perSecond(double rate) {
        if (!Rules.finite(rate) || rate <= 0) {
            throw Rules.fail("wait throughput must be positive");
        }
        return new Wait("throughput", rate);
    }

    /** Exponentially distributed pause with this mean. */
    public static Wait exponential(String mean) {
        return new Wait("poisson", Rules.duration(mean, "wait"));
    }

    Map<String, Object> toJson() {
        return json;
    }
}
