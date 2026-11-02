// File: ballista/sdks/java/src/main/java/ballista/client/models/RunState.java
package ballista.client.models;

import ballista.client.exceptions.ResponseException;

/** Lifecycle state of a run. */
public enum RunState {
    QUEUED("queued"),
    RUNNING("running"),
    STOPPING("stopping"),
    PASSED("passed"),
    FAILED("failed"),
    STOPPED("stopped"),
    CANCELLED("cancelled"),
    ERROR("error");

    private final String wire;

    RunState(String wire) {
        this.wire = wire;
    }

    public String wire() {
        return wire;
    }

    public boolean isTerminal() {
        return switch (this) {
            case PASSED, FAILED, STOPPED, CANCELLED, ERROR -> true;
            default -> false;
        };
    }

    public static RunState fromWire(String v) {
        for (RunState s : values()) {
            if (s.wire.equals(v)) {
                return s;
            }
        }
        throw new ResponseException("unknown run state '" + v + "'");
    }

    @Override
    public String toString() {
        return wire;
    }
}
