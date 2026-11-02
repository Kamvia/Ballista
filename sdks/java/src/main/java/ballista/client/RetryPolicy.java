// File: ballista/sdks/java/src/main/java/ballista/client/RetryPolicy.java
package ballista.client;

import java.time.Duration;

/** Retries of requests on connection errors and 429/502/503/504, with jittered exponential backoff. */
public record RetryPolicy(int attempts, Duration baseDelay, Duration maxDelay) {
    public static final RetryPolicy DEFAULT = new RetryPolicy(4, Duration.ofMillis(500), Duration.ofSeconds(20));

    public RetryPolicy {
        attempts = Math.max(1, attempts);
        if (baseDelay == null || maxDelay == null || baseDelay.isNegative() || maxDelay.isNegative()) {
            throw new IllegalArgumentException("retry delays must be non-negative");
        }
    }
}
