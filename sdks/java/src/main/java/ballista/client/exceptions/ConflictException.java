// File: ballista/sdks/java/src/main/java/ballista/client/exceptions/ConflictException.java
package ballista.client.exceptions;

import java.time.Duration;

/** The operation conflicts with the current state (409). */
public class ConflictException extends ApiException {
    private static final long serialVersionUID = 1L;

    public ConflictException(int status, String code, String detail, Duration retryAfter) {
        super(status, code, detail, retryAfter);
    }
}
