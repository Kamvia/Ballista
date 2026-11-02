// File: ballista/sdks/java/src/main/java/ballista/client/exceptions/NotFoundException.java
package ballista.client.exceptions;

import java.time.Duration;

/** Unknown bundle, run or artifact (404). */
public class NotFoundException extends ApiException {
    private static final long serialVersionUID = 1L;

    public NotFoundException(int status, String code, String detail, Duration retryAfter) {
        super(status, code, detail, retryAfter);
    }
}
