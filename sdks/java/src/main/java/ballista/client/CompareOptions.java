// File: ballista/sdks/java/src/main/java/ballista/client/CompareOptions.java
package ballista.client;

import ballista.client.models.CompareMetric;

import java.util.List;

/** Baseline and rule for {@link Client#compare}; null fields use the server defaults. */
public record CompareOptions(String baseline, Double maxRegressionPct, List<CompareMetric> metrics) {
    public static final CompareOptions DEFAULT = new CompareOptions(null, null, null);

    public CompareOptions {
        metrics = metrics == null ? null : List.copyOf(metrics);
    }
}
