// File: ballista/sdks/java/src/main/java/ballista/client/models/Page.java
package ballista.client.models;

import ballista.client.internal.Fields;

import java.util.List;
import java.util.function.Function;

/** One page of a listing; nextCursor is null on the last page. */
public record Page<T>(List<T> items, String nextCursor) {
    public static <T> Page<T> from(Object v, Function<Object, T> item) {
        Fields f = Fields.of(v, "page");
        return new Page<>(f.list("items", item), f.optStr("next_cursor"));
    }
}
