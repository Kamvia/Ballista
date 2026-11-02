// File: ballista/sdks/java/src/main/java/ballista/client/exceptions/ServerException.java
package ballista.client.exceptions;

import java.time.Duration;

/** Server-side failure (5xx). */
public class ServerException extends ApiException {
    private static final long serialVersionUID = 1L;

    public ServerException(int status, String code, String detail, Duration retryAfter) {
        super(status, code, detail, retryAfter);
    }
}
