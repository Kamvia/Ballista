// File: ballista/sdks/java/src/main/java/ballista/client/models/Comparison.java
package ballista.client.models;

import ballista.client.internal.Fields;

import java.util.List;

/** Result of comparing a run with a baseline; baseline is null when none was found. */
public record Comparison(String baseline, boolean passed, long regressions, List<MetricChange> changes, List<String> unmatched,
                         String note) {
    public static Comparison from(Object v) {
        Fields f = Fields.of(v, "comparison");
        return new Comparison(f.optStr("baseline"), f.bool("passed"), f.lng("regressions"), f.list("changes", MetricChange::from),
                f.strList("unmatched"), f.optStr("note"));
    }
}
