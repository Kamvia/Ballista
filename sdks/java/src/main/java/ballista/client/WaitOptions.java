// File: ballista/sdks/java/src/main/java/ballista/client/WaitOptions.java
package ballista.client;

import ballista.client.models.Run;
import ballista.client.models.Snapshot;

import java.time.Duration;
import java.util.function.Consumer;

/** Options for waiting on a run; a null timeout waits until the run finishes. */
public record WaitOptions(Duration timeout, Duration pollInterval, Consumer<Snapshot> onStats, Consumer<Run> onState) {
    public static final WaitOptions DEFAULT = new WaitOptions(null, null, null, null);

    public static WaitOptions timeout(Duration timeout) {
        return new WaitOptions(timeout, null, null, null);
    }

    public WaitOptions withTimeout(Duration d) {
        return new WaitOptions(d, pollInterval, onStats, onState);
    }

    public WaitOptions withPollInterval(Duration d) {
        return new WaitOptions(timeout, d, onStats, onState);
    }

    public WaitOptions withOnStats(Consumer<Snapshot> c) {
        return new WaitOptions(timeout, pollInterval, c, onState);
    }

    public WaitOptions withOnState(Consumer<Run> c) {
        return new WaitOptions(timeout, pollInterval, onStats, c);
    }
}
