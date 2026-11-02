// File: ballista/sdks/java/src/main/java/ballista/client/models/StatsRow.java
package ballista.client.models;

import ballista.client.internal.Fields;

import java.util.List;

public record StatsRow(String scenario, String method, String name, long count, long failures, double failRatio, double rps,
                       double failRps, double rpsAvg, double avgMs, double minMs, double maxMs, double p50Ms, double p90Ms,
                       double p95Ms, double p99Ms, double p999Ms, double avgBytes, List<Double> percentiles) {
    public static final StatsRow EMPTY = new StatsRow("", "", "", 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, List.of());

    public static StatsRow from(Object v) {
        Fields f = Fields.of(v, "row");
        String scenario = f.optStr("scenario");
        String method = f.optStr("method");
        String name = f.optStr("name");
        return new StatsRow(scenario == null ? "" : scenario, method == null ? "" : method, name == null ? "" : name,
                f.lng("count"), f.lng("failures"), f.num("fail_ratio"), f.num("rps"), f.num("fail_rps"), f.num("rps_avg"),
                f.num("avg_ms"), f.num("min_ms"), f.num("max_ms"), f.num("p50_ms"), f.num("p90_ms"), f.num("p95_ms"),
                f.num("p99_ms"), f.num("p999_ms"), f.num("avg_bytes"), f.numList("percentiles"));
    }
}
