// File: ballista/sdks/java/src/main/java/ballista/client/exceptions/RateLimitException.java
package ballista.client.exceptions;

import java.time.Duration;

/** Request rejected by the server rate limit (429). */
public class RateLimitException extends ApiException {
    private static final long serialVersionUID = 1L;

    public RateLimitException(int status, String code, String detail, Duration retryAfter) {
        super(status, code, detail, retryAfter);
    }
}
