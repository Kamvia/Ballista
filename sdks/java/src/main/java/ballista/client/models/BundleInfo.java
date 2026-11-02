// File: ballista/sdks/java/src/main/java/ballista/client/models/BundleInfo.java
package ballista.client.models;

import ballista.client.exceptions.ResponseException;
import ballista.client.internal.Fields;
import ballista.client.internal.Rules;

import java.util.List;
import java.util.Map;

/** A stored, content-addressed bundle. */
public record BundleInfo(String id, String planName, List<String> scenarios, List<BundleFile> files, long size,
                         Map<String, String> labels, String createdAt, String createdBy) {
    public static BundleInfo from(Object v) {
        Fields f = Fields.of(v, "bundle");
        String id = f.str("id");
        if (!Rules.BUNDLE_ID.matcher(id).matches()) {
            throw new ResponseException("bundle.id is malformed");
        }
        return new BundleInfo(id, f.str("plan_name"), f.strList("scenarios"), f.list("files", BundleFile::from), f.lng("size"),
                f.strMap("labels"), f.str("created_at"), f.str("created_by"));
    }
}
