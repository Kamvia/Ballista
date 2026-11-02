// File: ballista/sdks/java/src/main/java/ballista/client/RunOptions.java
package ballista.client;

import ballista.client.models.Overrides;
import ballista.client.models.RegressionGate;

import java.util.Map;

/** Options for a queued run and, for {@link Client#run}, whether and how to wait for it. */
public record RunOptions(Overrides overrides, Map<String, String> labels, String idempotencyKey, RegressionGate regression,
                         boolean noWait, WaitOptions waitOptions) {
    public static final RunOptions DEFAULT = new RunOptions(null, Map.of(), null, null, false, WaitOptions.DEFAULT);

    public RunOptions {
        labels = labels == null ? Map.of() : Map.copyOf(labels);
        waitOptions = waitOptions == null ? WaitOptions.DEFAULT : waitOptions;
    }

    public static RunOptions of(Overrides overrides) {
        return DEFAULT.withOverrides(overrides);
    }

    public RunOptions withOverrides(Overrides o) {
        return new RunOptions(o, labels, idempotencyKey, regression, noWait, waitOptions);
    }

    public RunOptions withLabels(Map<String, String> l) {
        return new RunOptions(overrides, l, idempotencyKey, regression, noWait, waitOptions);
    }

    /** Makes retries safe against duplicate runs; one is generated when null. */
    public RunOptions withIdempotencyKey(String k) {
        return new RunOptions(overrides, labels, k, regression, noWait, waitOptions);
    }

    /** Fails the run when it is worse than its baseline. */
    public RunOptions withRegression(RegressionGate g) {
        return new RunOptions(overrides, labels, idempotencyKey, g, noWait, waitOptions);
    }

    public RunOptions withNoWait() {
        return new RunOptions(overrides, labels, idempotencyKey, regression, true, waitOptions);
    }

    public RunOptions withWait(WaitOptions w) {
        return new RunOptions(overrides, labels, idempotencyKey, regression, noWait, w);
    }
}
