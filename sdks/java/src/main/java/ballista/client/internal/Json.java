// File: ballista/sdks/java/src/main/java/ballista/client/internal/Json.java
package ballista.client.internal;

import java.math.BigDecimal;
import java.util.ArrayList;
import java.util.Collection;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * Strict RFC 8259 JSON codec. Objects decode to {@link LinkedHashMap}, arrays to {@link ArrayList},
 * integral numbers to {@link Long}, other numbers to {@link Double}. Nesting is limited to 128 levels.
 */
public final class Json {
    private static final int MAX_DEPTH = 128;

    private Json() {
    }

    /** Thrown for malformed input or values that cannot be encoded. */
    public static final class JsonException extends RuntimeException {
        private static final long serialVersionUID = 1L;

        JsonException(String message) {
            super(message);
        }
    }

    public static Object parse(String text) {
        Parser p = new Parser(text);
        p.ws();
        Object v = p.value(0);
        p.ws();
        if (p.pos != text.length()) {
            throw p.fail("trailing data");
        }
        return v;
    }

    public static String write(Object value) {
        StringBuilder sb = new StringBuilder(256);
        write(sb, value, 0);
        return sb.toString();
    }

    private static void write(StringBuilder sb, Object v, int depth) {
        if (depth > MAX_DEPTH) {
            throw new JsonException("value nested too deeply");
        }
        switch (v) {
            case null -> sb.append("null");
            case String s -> string(sb, s);
            case Boolean b -> sb.append(b.booleanValue());
            case Double d -> number(sb, d);
            case Float f -> number(sb, f.doubleValue());
            case BigDecimal bd -> sb.append(bd.toPlainString());
            case Number n -> sb.append(n.longValue());
            case Map<?, ?> m -> {
                sb.append('{');
                boolean first = true;
                for (Map.Entry<?, ?> e : m.entrySet()) {
                    if (!(e.getKey() instanceof String k)) {
                        throw new JsonException("object keys must be strings");
                    }
                    if (!first) {
                        sb.append(',');
                    }
                    first = false;
                    string(sb, k);
                    sb.append(':');
                    write(sb, e.getValue(), depth + 1);
                }
                sb.append('}');
            }
            case Collection<?> c -> {
                sb.append('[');
                boolean first = true;
                for (Object o : c) {
                    if (!first) {
                        sb.append(',');
                    }
                    first = false;
                    write(sb, o, depth + 1);
                }
                sb.append(']');
            }
            case Object[] a -> write(sb, List.of(a), depth);
            case JsonValue j -> write(sb, j.toJson(), depth);
            default -> throw new JsonException("cannot encode " + v.getClass().getSimpleName());
        }
    }

    private static void number(StringBuilder sb, double d) {
        if (Double.isNaN(d) || Double.isInfinite(d)) {
            throw new JsonException("numbers must be finite");
        }
        if (d == Math.rint(d) && Math.abs(d) < 1e15) {
            sb.append((long) d);
        } else {
            sb.append(d);
        }
    }

    private static void string(StringBuilder sb, String s) {
        sb.append('"');
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            switch (c) {
                case '"' -> sb.append("\\\"");
                case '\\' -> sb.append("\\\\");
                case '\n' -> sb.append("\\n");
                case '\r' -> sb.append("\\r");
                case '\t' -> sb.append("\\t");
                case '\b' -> sb.append("\\b");
                case '\f' -> sb.append("\\f");
                default -> {
                    if (c < 0x20 || c == 0x2028 || c == 0x2029) {
                        sb.append(String.format("\\u%04x", (int) c));
                    } else {
                        sb.append(c);
                    }
                }
            }
        }
        sb.append('"');
    }

    /** Implemented by request types that encode themselves. */
    public interface JsonValue {
        Object toJson();
    }

    private static final class Parser {
        private final String s;
        private int pos;

        Parser(String s) {
            this.s = s;
        }

        JsonException fail(String what) {
            return new JsonException(what + " at offset " + pos);
        }

        void ws() {
            while (pos < s.length()) {
                char c = s.charAt(pos);
                if (c != ' ' && c != '\t' && c != '\n' && c != '\r') {
                    return;
                }
                pos++;
            }
        }

        char peek() {
            if (pos >= s.length()) {
                throw fail("unexpected end");
            }
            return s.charAt(pos);
        }

        Object value(int depth) {
            if (depth > MAX_DEPTH) {
                throw fail("nested too deeply");
            }
            char c = peek();
            return switch (c) {
                case '{' -> object(depth);
                case '[' -> array(depth);
                case '"' -> str();
                case 't' -> literal("true", Boolean.TRUE);
                case 'f' -> literal("false", Boolean.FALSE);
                case 'n' -> literal("null", null);
                default -> {
                    if (c == '-' || (c >= '0' && c <= '9')) {
                        yield num();
                    }
                    throw fail("unexpected character");
                }
            };
        }

        Object literal(String word, Object v) {
            if (!s.startsWith(word, pos)) {
                throw fail("invalid literal");
            }
            pos += word.length();
            return v;
        }

        Map<String, Object> object(int depth) {
            pos++;
            Map<String, Object> out = new LinkedHashMap<>();
            ws();
            if (peek() == '}') {
                pos++;
                return out;
            }
            while (true) {
                ws();
                if (peek() != '"') {
                    throw fail("expected key");
                }
                String k = str();
                ws();
                if (peek() != ':') {
                    throw fail("expected ':'");
                }
                pos++;
                ws();
                if (out.put(k, value(depth + 1)) != null) {
                    throw fail("duplicate key");
                }
                ws();
                char c = peek();
                pos++;
                if (c == '}') {
                    return out;
                }
                if (c != ',') {
                    throw fail("expected ',' or '}'");
                }
            }
        }

        List<Object> array(int depth) {
            pos++;
            List<Object> out = new ArrayList<>();
            ws();
            if (peek() == ']') {
                pos++;
                return out;
            }
            while (true) {
                ws();
                out.add(value(depth + 1));
                ws();
                char c = peek();
                pos++;
                if (c == ']') {
                    return out;
                }
                if (c != ',') {
                    throw fail("expected ',' or ']'");
                }
            }
        }

        String str() {
            pos++;
            StringBuilder sb = new StringBuilder();
            while (true) {
                char c = peek();
                pos++;
                if (c == '"') {
                    return sb.toString();
                }
                if (c < 0x20) {
                    throw fail("control character in string");
                }
                if (c != '\\') {
                    sb.append(c);
                    continue;
                }
                char e = peek();
                pos++;
                switch (e) {
                    case '"', '\\', '/' -> sb.append(e);
                    case 'b' -> sb.append('\b');
                    case 'f' -> sb.append('\f');
                    case 'n' -> sb.append('\n');
                    case 'r' -> sb.append('\r');
                    case 't' -> sb.append('\t');
                    case 'u' -> {
                        if (pos + 4 > s.length()) {
                            throw fail("short unicode escape");
                        }
                        try {
                            sb.append((char) Integer.parseInt(s.substring(pos, pos + 4), 16));
                        } catch (NumberFormatException ex) {
                            throw fail("invalid unicode escape");
                        }
                        pos += 4;
                    }
                    default -> throw fail("invalid escape");
                }
            }
        }

        Object num() {
            int start = pos;
            if (peek() == '-') {
                pos++;
            }
            if (peek() == '0') {
                pos++;
            } else {
                digits();
            }
            boolean integral = true;
            if (pos < s.length() && s.charAt(pos) == '.') {
                integral = false;
                pos++;
                digits();
            }
            if (pos < s.length() && (s.charAt(pos) == 'e' || s.charAt(pos) == 'E')) {
                integral = false;
                pos++;
                if (pos < s.length() && (s.charAt(pos) == '+' || s.charAt(pos) == '-')) {
                    pos++;
                }
                digits();
            }
            String t = s.substring(start, pos);
            if (integral && t.length() <= 18) {
                return Long.parseLong(t);
            }
            double d = Double.parseDouble(t);
            if (Double.isInfinite(d)) {
                throw fail("number out of range");
            }
            return d;
        }

        void digits() {
            int start = pos;
            while (pos < s.length() && s.charAt(pos) >= '0' && s.charAt(pos) <= '9') {
                pos++;
            }
            if (pos == start) {
                throw fail("expected digit");
            }
        }
    }
}
