// File: ballista/sdks/java/src/main/java/ballista/client/exceptions/ConfigurationException.java
package ballista.client.exceptions;

/** Invalid client configuration or input rejected before sending. */
public class ConfigurationException extends BallistaException {
    private static final long serialVersionUID = 1L;

    public ConfigurationException(String message) {
        super(message);
    }
}
