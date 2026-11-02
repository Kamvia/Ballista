// File: ballista/sdks/java/src/main/java/ballista/client/models/WhoAmI.java
package ballista.client.models;

import ballista.client.internal.Fields;

public record WhoAmI(String name, String scope) {
    public static WhoAmI from(Object v) {
        Fields f = Fields.of(v, "whoami");
        return new WhoAmI(f.str("name"), f.str("scope"));
    }
}
