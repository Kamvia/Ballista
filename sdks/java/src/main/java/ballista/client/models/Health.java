// File: ballista/sdks/java/src/main/java/ballista/client/models/Health.java
package ballista.client.models;

import ballista.client.internal.Fields;

public record Health(String status, String version) {
    public static Health from(Object v) {
        Fields f = Fields.of(v, "health");
        return new Health(f.str("status"), f.str("version"));
    }
}
