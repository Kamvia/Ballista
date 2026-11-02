// File: ballista/sdks/java/src/main/java/ballista/client/models/BundleFile.java
package ballista.client.models;

import ballista.client.internal.Fields;

public record BundleFile(String path, long size, String sha256) {
    public static BundleFile from(Object v) {
        Fields f = Fields.of(v, "bundle file");
        return new BundleFile(f.str("path"), f.lng("size"), f.str("sha256"));
    }
}
