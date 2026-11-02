// File: ballista/sdks/java/src/main/java/ballista/client/models/WorkerInfo.java
package ballista.client.models;

import ballista.client.internal.Fields;

/** A pool worker connected to the server. cpuPct is null when unknown. */
public record WorkerInfo(long id, String name, String address, long cores, String state, long users, long iterations,
                         double target, Double cpuPct, double schedulerLagMs, boolean saturated, double lastReportSecs) {
    public static WorkerInfo from(Object v) {
        Fields f = Fields.of(v, "worker");
        return new WorkerInfo(f.lng("id"), f.str("name"), f.str("address"), f.lng("cores"), f.str("state"), f.lng("users"),
                f.lng("iterations"), f.num("target"), f.optNum("cpu_pct"), f.num("scheduler_lag_ms"), f.bool("saturated"),
                f.num("last_report_secs"));
    }
}
