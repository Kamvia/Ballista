// File: ballista/sdks/java/src/main/java/ballista/client/models/Run.java
package ballista.client.models;

import ballista.client.exceptions.ResponseException;
import ballista.client.internal.Fields;
import ballista.client.internal.Rules;

import java.util.Map;

/** A queued, running or finished run. Nullable fields are null until known. */
public record Run(String id, long seq, String bundle, String planName, RunState state, String createdAt, String createdBy,
                  String startedAt, String endedAt, Map<String, String> labels, Overrides overrides, Boolean passed, String error,
                  Summary summary, String stoppedBy, Integer workers, RegressionGate regression, Comparison comparison,
                  Long queuePosition) {
    public boolean isFinished() {
        return state.isTerminal();
    }

    public static Run from(Object v) {
        Fields f = Fields.of(v, "run");
        String id = f.str("id");
        if (!Rules.RUN_ID.matcher(id).matches()) {
            throw new ResponseException("run.id is malformed");
        }
        Long workers = f.optLng("workers");
        return new Run(id, f.lng("seq"), f.str("bundle"), f.str("plan_name"), RunState.fromWire(f.str("state")),
                f.str("created_at"), f.str("created_by"), f.optStr("started_at"), f.optStr("ended_at"), f.strMap("labels"),
                Overrides.from(f.raw().get("overrides")), f.optBool("passed"), f.optStr("error"),
                f.has("summary") ? Summary.from(f.raw().get("summary")) : null, f.optStr("stopped_by"),
                workers == null ? null : workers.intValue(),
                f.has("regression") ? RegressionGate.from(f.raw().get("regression")) : null,
                f.has("comparison") ? Comparison.from(f.raw().get("comparison")) : null, f.optLng("queue_position"));
    }
}
