// File: ballista/sdks/java/src/main/java/ballista/client/models/Summary.java
package ballista.client.models;

import ballista.client.internal.Fields;

import java.util.List;

public record Summary(long requests, long failures, double failRatio, double rps, double avgMs, double p50Ms, double p95Ms,
                      double p99Ms, double maxMs, long iterations, double durationSecs, boolean aborted,
                      double generatorSaturatedSecs, List<ThresholdOutcome> thresholds) {
    public static Summary from(Object v) {
        Fields f = Fields.of(v, "summary");
        return new Summary(f.lng("requests"), f.lng("failures"), f.num("fail_ratio"), f.num("rps"), f.num("avg_ms"),
                f.num("p50_ms"), f.num("p95_ms"), f.num("p99_ms"), f.num("max_ms"), f.lng("iterations"), f.num("duration_secs"),
                f.bool("aborted"), f.num("generator_saturated_secs"), f.list("thresholds", ThresholdOutcome::from));
    }
}
