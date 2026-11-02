// File: ballista/sdks/java/src/main/java/ballista/client/models/Snapshot.java
package ballista.client.models;

import ballista.client.internal.Fields;

import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.stream.Stream;

/** Live or final statistics of a run. */
public record Snapshot(String plan, String state, double elapsedSecs, long users, double target, StatsRow total, List<StatsRow> rows,
                       List<StatsRow> transactions, List<StatsRow> iterations, List<Map<String, Object>> errors,
                       List<Map<String, Object>> thresholds, List<Bucket> distribution, long iterationsStarted,
                       long droppedIterations, boolean aborted) {
    /** Finds a request or transaction row by name, and by method when method is not null. */
    public Optional<StatsRow> findRow(String name, String method) {
        return Stream.concat(rows.stream(), transactions.stream())
                .filter(r -> r.name().equals(name) && (method == null || r.method().equals(method)))
                .findFirst();
    }

    public static Snapshot from(Object v) {
        Fields f = Fields.of(v, "snapshot");
        String plan = f.optStr("plan");
        String state = f.optStr("state");
        return new Snapshot(plan == null ? "" : plan, state == null ? "" : state, f.num("elapsed_secs"), f.lng("users"),
                f.num("target"), f.has("total") ? StatsRow.from(f.raw().get("total")) : StatsRow.EMPTY,
                f.list("rows", StatsRow::from), f.list("transactions", StatsRow::from), f.list("iterations", StatsRow::from),
                f.list("errors", o -> Fields.of(o, "error").raw()), f.list("thresholds", o -> Fields.of(o, "threshold").raw()),
                f.list("distribution", Bucket::from), f.lng("iterations_started"), f.lng("dropped_iterations"), f.bool("aborted"));
    }
}
