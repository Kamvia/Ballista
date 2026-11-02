// File: ballista/sdks/java/src/main/java/ballista/client/exceptions/BallistaException.java
package ballista.client.exceptions;

/** Base class for every error raised by this package. Messages never include the API token. */
public class BallistaException extends RuntimeException {
    private static final long serialVersionUID = 1L;

    public BallistaException(String message) {
        super(message);
    }

    public BallistaException(String message, Throwable cause) {
        super(message, cause);
    }
}
