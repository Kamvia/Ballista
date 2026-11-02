// File: ballista/sdks/java/src/main/java/ballista/client/models/RegressionGate.java
package ballista.client.models;

import ballista.client.internal.Fields;
import ballista.client.internal.Json;
import ballista.client.internal.Rules;

import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * Fails a run that is worse than a baseline run beyond these limits. Null fields use the server defaults
 * (auto baseline, 10%, p95/p99/error_rate, 30 samples, 1 ms, 0.01).
 */
public record RegressionGate(String baseline, Double maxRegressionPct, List<CompareMetric> metrics, Long minSamples,
                             Double minChangeMs, Double maxErrorRateIncrease) implements Json.JsonValue {
    public RegressionGate {
        metrics = metrics == null ? null : List.copyOf(metrics);
    }

    public static Builder builder() {
        return new Builder();
    }

    public RegressionGate validate() {
        if (baseline != null && !baseline.equals("auto") && !Rules.RUN_ID.matcher(baseline).matches()) {
            throw Rules.fail("baseline must be 'auto' or a run id");
        }
        if (maxRegressionPct != null && !(Rules.finite(maxRegressionPct) && maxRegressionPct >= 0 && maxRegressionPct <= 10_000)) {
            throw Rules.fail("max_regression_pct must be within 0..10000");
        }
        if (minChangeMs != null && !(Rules.finite(minChangeMs) && minChangeMs >= 0)) {
            throw Rules.fail("min_change_ms must be non-negative");
        }
        if (maxErrorRateIncrease != null && !(Rules.finite(maxErrorRateIncrease) && maxErrorRateIncrease >= 0 && maxErrorRateIncrease <= 1)) {
            throw Rules.fail("max_error_rate_increase must be within 0..1");
        }
        if (minSamples != null && minSamples < 0) {
            throw Rules.fail("min_samples must be non-negative");
        }
        if (metrics != null && (metrics.isEmpty() || metrics.size() > 6)) {
            throw Rules.fail("metrics must list 1..6 metrics");
        }
        return this;
    }

    @Override
    public Map<String, Object> toJson() {
        Map<String, Object> m = new LinkedHashMap<>();
        if (baseline != null) m.put("baseline", baseline);
        if (maxRegressionPct != null) m.put("max_regression_pct", maxRegressionPct);
        if (metrics != null) m.put("metrics", metrics.stream().map(CompareMetric::wire).toList());
        if (minSamples != null) m.put("min_samples", minSamples);
        if (minChangeMs != null) m.put("min_change_ms", minChangeMs);
        if (maxErrorRateIncrease != null) m.put("max_error_rate_increase", maxErrorRateIncrease);
        return m;
    }

    public static RegressionGate from(Object v) {
        Fields f = Fields.of(v, "regression");
        return new RegressionGate(f.optStr("baseline"), f.optNum("max_regression_pct"),
                f.has("metrics") ? f.strList("metrics").stream().map(CompareMetric::fromWire).toList() : null,
                f.optLng("min_samples"), f.optNum("min_change_ms"), f.optNum("max_error_rate_increase"));
    }

    public static final class Builder {
        private String baseline;
        private Double maxRegressionPct;
        private List<CompareMetric> metrics;
        private Long minSamples;
        private Double minChangeMs;
        private Double maxErrorRateIncrease;

        /** "auto" (latest passed run of the same plan) or a run id. */
        public Builder baseline(String v) { baseline = v; return this; }
        public Builder maxRegressionPct(double v) { maxRegressionPct = v; return this; }
        public Builder metrics(CompareMetric... v) { metrics = List.of(v); return this; }
        public Builder minSamples(long v) { minSamples = v; return this; }
        public Builder minChangeMs(double v) { minChangeMs = v; return this; }
        public Builder maxErrorRateIncrease(double v) { maxErrorRateIncrease = v; return this; }

        public RegressionGate build() {
            return new RegressionGate(baseline, maxRegressionPct, metrics, minSamples, minChangeMs, maxErrorRateIncrease).validate();
        }
    }
}
