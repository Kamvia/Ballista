// File: ballista/sdks/java/src/main/java/ballista/client/exceptions/RunTimeoutException.java
package ballista.client.exceptions;

import java.time.Duration;

/** A wait for run completion exceeded its deadline. */
public class RunTimeoutException extends BallistaException {
    private static final long serialVersionUID = 1L;

    public RunTimeoutException(String runId, Duration timeout) {
        super("run " + runId + " did not finish within " + timeout);
    }
}
