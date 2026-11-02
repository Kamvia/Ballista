// File: ballista/sdks/java/src/main/java/ballista/client/models/CompareMetric.java
package ballista.client.models;

import ballista.client.exceptions.ResponseException;

/** Metric the regression gate compares. */
public enum CompareMetric {
    P50("p50"),
    P95("p95"),
    P99("p99"),
    AVG("avg"),
    ERROR_RATE("error_rate"),
    RPS("rps");

    private final String wire;

    CompareMetric(String wire) {
        this.wire = wire;
    }

    public String wire() {
        return wire;
    }

    public static CompareMetric fromWire(String v) {
        for (CompareMetric m : values()) {
            if (m.wire.equals(v)) {
                return m;
            }
        }
        throw new ResponseException("unknown metric '" + v + "'");
    }

    @Override
    public String toString() {
        return wire;
    }
}
