// File: ballista/sdks/java/src/main/java/ballista/client/models/Limits.java
package ballista.client.models;

import ballista.client.internal.Fields;

public record Limits(double maxUsers, long maxRunTimeSecs, long maxQueue) {
    public static Limits from(Object v) {
        Fields f = Fields.of(v, "limits");
        return new Limits(f.num("max_users"), f.lng("max_run_time_secs"), f.lng("max_queue"));
    }
}
