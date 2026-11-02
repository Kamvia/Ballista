// File: ballista/sdks/java/src/main/java/ballista/client/exceptions/AuthenticationException.java
package ballista.client.exceptions;

import java.time.Duration;

/** Missing or invalid API token (401). */
public class AuthenticationException extends ApiException {
    private static final long serialVersionUID = 1L;

    public AuthenticationException(int status, String code, String detail, Duration retryAfter) {
        super(status, code, detail, retryAfter);
    }
}
