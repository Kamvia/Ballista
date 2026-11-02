// File: ballista/sdks/java/src/main/java/ballista/client/exceptions/ValidationException.java
package ballista.client.exceptions;

import java.time.Duration;

/** Plan or request rejected by server-side validation or policy (422). */
public class ValidationException extends ApiException {
    private static final long serialVersionUID = 1L;

    public ValidationException(int status, String code, String detail, Duration retryAfter) {
        super(status, code, detail, retryAfter);
    }
}
