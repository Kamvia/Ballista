// File: ballista/sdks/java/src/main/java/ballista/client/models/ThresholdOutcome.java
package ballista.client.models;

import ballista.client.internal.Fields;

public record ThresholdOutcome(String metric, String request, Double min, Double max, Double actual, boolean passed) {
    public static ThresholdOutcome from(Object v) {
        Fields f = Fields.of(v, "threshold");
        return new ThresholdOutcome(f.str("metric"), f.str("request"), f.optNum("min"), f.optNum("max"), f.optNum("actual"),
                f.bool("passed"));
    }
}
