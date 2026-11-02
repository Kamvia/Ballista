// File: ballista/sdks/java/src/test/java/ballista/client/JsonTest.java
package ballista.client;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;

import ballista.client.internal.Json;

import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

import org.junit.jupiter.api.Test;

class JsonTest {
    @Test
    void roundTrips() {
        Object v = Json.parse("{\"a\":[1,2.5,-3e2,true,false,null],\"s\":\"q\\\"\\n\\u00e9\\ud83d\\ude00\",\"o\":{}}");
        Map<?, ?> m = (Map<?, ?>) v;
        assertEquals(List.of(1L, 2.5, -300.0, true, false), ((List<?>) m.get("a")).subList(0, 5));
        assertEquals("q\"\n\u00e9\ud83d\ude00", m.get("s"));
        assertEquals(Json.write(v), Json.write(Json.parse(Json.write(v))));
    }

    @Test
    void rejectsMalformedInput() {
        for (String bad : List.of("", "{", "[1,]", "{\"a\":1,}", "01", "1.", "\"\u0001\"", "{\"a\":1,\"a\":2}", "nul", "[1] x", "\"\\x\"")) {
            assertThrows(Json.JsonException.class, () -> Json.parse(bad), bad);
        }
        assertThrows(Json.JsonException.class, () -> Json.parse("[".repeat(200) + "]".repeat(200)));
    }

    @Test
    void encodesSafely() {
        Map<String, Object> m = new LinkedHashMap<>();
        m.put("x", "</script>\u2028");
        m.put("n", 3.0);
        assertEquals("{\"x\":\"</script>\\u2028\",\"n\":3}", Json.write(m));
        assertThrows(Json.JsonException.class, () -> Json.write(Map.of("x", Double.NaN)));
    }
}
