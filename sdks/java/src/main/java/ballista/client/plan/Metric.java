// File: ballista/sdks/java/src/main/java/ballista/client/plan/Metric.java
package ballista.client.plan;

/** Threshold metric. */
public enum Metric {
    P50("p50"),
    P90("p90"),
    P95("p95"),
    P99("p99"),
    AVG("avg"),
    MAX("max"),
    ERROR_RATE("error_rate"),
    RPS("rps"),
    DROPPED_ITERATIONS("dropped_iterations");

    private final String wire;

    Metric(String wire) {
        this.wire = wire;
    }

    public String wire() {
        return wire;
    }
}
