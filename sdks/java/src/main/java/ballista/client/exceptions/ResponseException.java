// File: ballista/sdks/java/src/main/java/ballista/client/exceptions/ResponseException.java
package ballista.client.exceptions;

/** A response body does not match the API contract. */
public class ResponseException extends BallistaException {
    private static final long serialVersionUID = 1L;

    public ResponseException(String what) {
        super("unexpected response: " + what);
    }
}
