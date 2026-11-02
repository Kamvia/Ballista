// File: ballista/sdks/java/src/main/java/ballista/client/exceptions/TransportException.java
package ballista.client.exceptions;

/** The server could not be reached or the connection failed. */
public class TransportException extends BallistaException {
    private static final long serialVersionUID = 1L;

    public TransportException(String message, Throwable cause) {
        super(message, cause);
    }
}
