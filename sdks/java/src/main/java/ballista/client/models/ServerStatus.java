// File: ballista/sdks/java/src/main/java/ballista/client/models/ServerStatus.java
package ballista.client.models;

import ballista.client.internal.Fields;

import java.util.List;

public record ServerStatus(String version, String activeRun, long queued, Limits limits, List<WorkerInfo> workers) {
    public static ServerStatus from(Object v) {
        Fields f = Fields.of(v, "status");
        return new ServerStatus(f.str("version"), f.optStr("active_run"), f.lng("queued"), Limits.from(f.raw().get("limits")),
                f.list("workers", WorkerInfo::from));
    }
}
