// File: ballista/sdks/java/src/main/java/ballista/client/plan/Plan.java
package ballista.client.plan;

import ballista.client.internal.Rules;

import java.util.ArrayList;
import java.util.HashSet;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;

/** A validated, structured test plan; {@link #toDocument()} is the form the server accepts. */
public final class Plan {
    private final Map<String, Object> document;
    private final String name;

    private Plan(Builder b) {
        if (b.name == null || b.name.isEmpty() || b.name.length() > 128) {
            throw Rules.fail("plan name must be 1..128 characters");
        }
        if (b.target == null || b.load == null) {
            throw Rules.fail("plan requires a target and a load");
        }
        if (b.scenarios.isEmpty()) {
            throw Rules.fail("plan requires at least one scenario");
        }
        Set<String> seen = new HashSet<>();
        for (Scenario s : b.scenarios) {
            if (!seen.add(s.name())) {
                throw Rules.fail("duplicate scenario '" + s.name() + "'");
            }
        }
        if (b.percentiles.size() > 12 || b.percentiles.stream().anyMatch(p -> !(p > 0 && p <= 100))) {
            throw Rules.fail("percentiles: up to 12 values in (0, 100]");
        }
        Map<String, Object> d = new LinkedHashMap<>();
        d.put("name", b.name);
        d.put("target", b.target.toJson());
        d.put("load", b.load.toJson());
        d.put("scenario", b.scenarios.stream().map(Scenario::toJson).toList());
        if (!b.feeders.isEmpty()) {
            d.put("feeder", b.feeders.stream().map(Feeder::toJson).toList());
        }
        if (!b.thresholds.isEmpty()) {
            d.put("threshold", b.thresholds.stream().map(Threshold::toJson).toList());
        }
        if (!b.percentiles.isEmpty()) {
            d.put("report", Map.of("percentiles", List.copyOf(b.percentiles)));
        }
        this.name = b.name;
        this.document = d;
    }

    public static Builder builder(String name) {
        return new Builder(name);
    }

    public String name() {
        return name;
    }

    /** The structured document sent to the server (a fresh mutable copy). */
    public Map<String, Object> toDocument() {
        return new LinkedHashMap<>(document);
    }

    public static final class Builder {
        private final String name;
        private Target target;
        private Load load;
        private final List<Scenario> scenarios = new ArrayList<>();
        private final List<Threshold> thresholds = new ArrayList<>();
        private final List<Feeder> feeders = new ArrayList<>();
        private final List<Double> percentiles = new ArrayList<>();

        private Builder(String name) {
            this.name = name;
        }

        public Builder target(Target t) { target = t; return this; }
        public Builder target(String baseUrl) { target = Target.of(baseUrl); return this; }
        public Builder load(Load l) { load = l; return this; }
        public Builder scenario(Scenario... s) { scenarios.addAll(List.of(s)); return this; }
        public Builder threshold(Threshold... t) { thresholds.addAll(List.of(t)); return this; }
        public Builder feeder(Feeder... f) { feeders.addAll(List.of(f)); return this; }
        public Builder percentiles(double... p) {
            for (double v : p) {
                percentiles.add(v);
            }
            return this;
        }

        public Plan build() {
            return new Plan(this);
        }
    }
}
