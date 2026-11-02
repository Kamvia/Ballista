// File: ballista/sdks/java/src/main/java/ballista/client/models/PlanCheck.java
package ballista.client.models;

import ballista.client.internal.Fields;

import java.util.List;
import java.util.Map;

/** Server-side validation result: files a bundle must still carry and the normalized plan. */
public record PlanCheck(String name, List<String> scenarios, List<String> files, Map<String, Object> document) {
    public static PlanCheck from(Object v) {
        Fields f = Fields.of(v, "plan check");
        return new PlanCheck(f.str("name"), f.strList("scenarios"), f.strList("files"), f.map("document"));
    }
}
