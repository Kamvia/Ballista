// File: ballista/sdks/java/src/main/java/ballista/client/plan/Feeder.java
package ballista.client.plan;

import ballista.client.internal.Rules;

import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/** Test data: inline rows, an inline table with columns, or a CSV asset in the bundle. */
public final class Feeder {
    private final Map<String, Object> json;

    private Feeder(Map<String, Object> json) {
        this.json = Collections.unmodifiableMap(new LinkedHashMap<>(json));
    }

    private static Map<String, Object> base(String name) {
        Map<String, Object> m = new LinkedHashMap<>();
        m.put("name", Rules.name(name, "feeder"));
        m.put("mode", "sequential");
        return m;
    }

    /** Rows as objects keyed by column. */
    public static Feeder rows(String name, List<Map<String, Object>> rows) {
        if (rows.isEmpty()) {
            throw Rules.fail("feeder '" + name + "' has no rows");
        }
        Map<String, Object> m = base(name);
        m.put("rows", List.copyOf(rows));
        return new Feeder(m);
    }

    /** Rows as arrays aligned with columns. */
    public static Feeder table(String name, List<String> columns, List<List<Object>> rows) {
        if (rows.isEmpty() || columns.isEmpty()) {
            throw Rules.fail("feeder '" + name + "' needs columns and rows");
        }
        for (List<Object> r : rows) {
            if (r.size() != columns.size()) {
                throw Rules.fail("feeder '" + name + "': every row needs " + columns.size() + " values");
            }
        }
        Map<String, Object> m = base(name);
        m.put("columns", List.copyOf(columns));
        m.put("rows", List.copyOf(rows));
        return new Feeder(m);
    }

    /** CSV file inside the bundle. */
    public static Feeder file(String name, String path) {
        Map<String, Object> m = base(name);
        m.put("path", path);
        return new Feeder(m);
    }

    public Feeder random() {
        Map<String, Object> m = new LinkedHashMap<>(json);
        m.put("mode", "random");
        return new Feeder(m);
    }

    Map<String, Object> toJson() {
        return new LinkedHashMap<>(json);
    }
}
