// File: ballista/sdks/java/src/main/java/ballista/client/plan/Scenario.java
package ballista.client.plan;

import ballista.client.internal.Rules;

import java.util.ArrayList;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/** One user behaviour: declarative steps, an inline script source, or a script file in the bundle. */
public final class Scenario {
    private final String name;
    private final Map<String, Object> json;
    private final List<Step> onStart;
    private final List<Step> steps;

    private Scenario(Builder b) {
        this.name = b.name;
        this.json = Collections.unmodifiableMap(new LinkedHashMap<>(b.m));
        this.onStart = List.copyOf(b.onStart);
        this.steps = List.copyOf(b.steps);
    }

    public static Builder builder(String name) {
        return new Builder(name);
    }

    public String name() {
        return name;
    }

    void check() {
        long kinds = (json.containsKey("source") ? 1 : 0) + (json.containsKey("script") ? 1 : 0) + (steps.isEmpty() ? 0 : 1);
        if (kinds != 1) {
            throw Rules.fail("scenario '" + name + "' needs exactly one of source (inline script), script (bundle asset) or steps");
        }
        for (int i = 0; i < onStart.size(); i++) {
            onStart.get(i).check("scenario '" + name + "' on_start step " + (i + 1));
        }
        for (int i = 0; i < steps.size(); i++) {
            steps.get(i).check("scenario '" + name + "' step " + (i + 1));
        }
    }

    Map<String, Object> toJson() {
        Map<String, Object> m = new LinkedHashMap<>();
        m.put("name", name);
        m.putAll(json);
        if (!onStart.isEmpty()) {
            m.put("on_start", onStart.stream().map(Step::toJson).toList());
        }
        if (!steps.isEmpty()) {
            m.put("step", steps.stream().map(Step::toJson).toList());
        }
        return m;
    }

    public static final class Builder {
        private final String name;
        private final Map<String, Object> m = new LinkedHashMap<>();
        private final List<Step> onStart = new ArrayList<>();
        private final List<Step> steps = new ArrayList<>();

        private Builder(String name) {
            this.name = Rules.name(name, "scenario");
        }

        public Builder weight(int w) {
            if (w < 1 || w > 10_000) {
                throw Rules.fail("scenario '" + name + "': weight must be 1..10000");
            }
            m.put("weight", (long) w);
            return this;
        }

        /** Inline script source. */
        public Builder source(String src) { m.put("source", src); return this; }
        /** Script file inside the bundle (sent as an asset). */
        public Builder script(String path) { m.put("script", path); return this; }
        public Builder waitBetween(Wait w) { m.put("wait", w.toJson()); return this; }
        public Builder fixedCount(int n) { m.put("fixed_count", (long) n); return this; }
        public Builder tags(String... t) { Rules.names(List.of(t), 64, "scenario '" + name + "' tags"); m.put("tags", List.of(t)); return this; }
        public Builder onStart(Step... s) { onStart.addAll(List.of(s)); return this; }
        public Builder onStart(List<Step> s) { onStart.addAll(s); return this; }
        public Builder step(Step... s) { steps.addAll(List.of(s)); return this; }
        /** Appends a step group, such as a transaction. */
        public Builder steps(List<Step> s) { steps.addAll(s); return this; }

        public Scenario build() {
            Scenario s = new Scenario(this);
            s.check();
            return s;
        }
    }
}
