// File: ballista/sdks/java/src/main/java/ballista/client/exceptions/PermissionDeniedException.java
package ballista.client.exceptions;

import java.time.Duration;

/** The token's scope does not allow the operation (403). */
public class PermissionDeniedException extends ApiException {
    private static final long serialVersionUID = 1L;

    public PermissionDeniedException(int status, String code, String detail, Duration retryAfter) {
        super(status, code, detail, retryAfter);
    }
}
