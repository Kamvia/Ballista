// File: ballista/sdks/java/src/main/java/ballista/client/models/MetricChange.java
package ballista.client.models;

import ballista.client.internal.Fields;

/** One compared metric; changePct is null when the baseline value is zero. */
public record MetricChange(String kind, String method, String name, CompareMetric metric, double baseline, double current,
                           Double changePct, boolean regressed) {
    public static MetricChange from(Object v) {
        Fields f = Fields.of(v, "change");
        return new MetricChange(f.str("kind"), f.str("method"), f.str("name"), CompareMetric.fromWire(f.str("metric")),
                f.num("baseline"), f.num("current"), f.optNum("change_pct"), f.bool("regressed"));
    }
}
