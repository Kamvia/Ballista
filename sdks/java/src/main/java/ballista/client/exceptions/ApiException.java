// File: ballista/sdks/java/src/main/java/ballista/client/exceptions/ApiException.java
package ballista.client.exceptions;

import java.time.Duration;

/** The server answered with an error status. */
public class ApiException extends BallistaException {
    private static final long serialVersionUID = 1L;

    private final int status;
    private final String code;
    private final String detail;
    private final transient Duration retryAfter;

    public ApiException(int status, String code, String detail, Duration retryAfter) {
        super(status + " " + code + ": " + detail);
        this.status = status;
        this.code = code;
        this.detail = detail;
        this.retryAfter = retryAfter;
    }

    public int status() {
        return status;
    }

    public String code() {
        return code;
    }

    public String detail() {
        return detail;
    }

    /** Server-suggested delay before retrying, or null. */
    public Duration retryAfter() {
        return retryAfter;
    }

    /** Maps a status code to the most specific exception type. */
    public static ApiException of(int status, String code, String detail, Duration retryAfter) {
        return switch (status) {
            case 401 -> new AuthenticationException(status, code, detail, retryAfter);
            case 403 -> new PermissionDeniedException(status, code, detail, retryAfter);
            case 404 -> new NotFoundException(status, code, detail, retryAfter);
            case 409 -> new ConflictException(status, code, detail, retryAfter);
            case 422 -> new ValidationException(status, code, detail, retryAfter);
            case 429 -> new RateLimitException(status, code, detail, retryAfter);
            default -> status >= 500 ? new ServerException(status, code, detail, retryAfter) : new ApiException(status, code, detail, retryAfter);
        };
    }
}
