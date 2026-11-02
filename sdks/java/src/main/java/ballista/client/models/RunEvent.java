// File: ballista/sdks/java/src/main/java/ballista/client/models/RunEvent.java
package ballista.client.models;

/** One message of a run's event stream: run and end carry a Run, stats a Snapshot. */
public record RunEvent(Type type, Run run, Snapshot stats) {
    public enum Type {
        RUN,
        STATS,
        END
    }
}
