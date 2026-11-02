// File: ballista/sdks/java/src/main/java/ballista/client/models/Bucket.java
package ballista.client.models;

import ballista.client.internal.Fields;

/** Latency histogram bucket; upperMs is null for the overflow bucket. */
public record Bucket(Double upperMs, long count) {
    public static Bucket from(Object v) {
        Fields f = Fields.of(v, "bucket");
        return new Bucket(f.optNum("upper_ms"), f.lng("count"));
    }
}
