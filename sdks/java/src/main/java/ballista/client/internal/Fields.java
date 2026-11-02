// File: ballista/sdks/java/src/main/java/ballista/client/internal/Fields.java
package ballista.client.internal;

import ballista.client.exceptions.ResponseException;

import java.util.ArrayList;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.function.Function;

/** Typed, checked access to a decoded JSON object from the server. */
public final class Fields {
    private final Map<String, Object> m;
    private final String what;

    private Fields(Map<String, Object> m, String what) {
        this.m = m;
        this.what = what;
    }

    @SuppressWarnings("unchecked")
    public static Fields of(Object v, String what) {
        if (!(v instanceof Map<?, ?> map)) {
            throw new ResponseException(what + " is not an object");
        }
        return new Fields((Map<String, Object>) map, what);
    }

    public Map<String, Object> raw() {
        return Collections.unmodifiableMap(m);
    }

    public boolean has(String k) {
        return m.get(k) != null;
    }

    private ResponseException bad(String k, String type) {
        return new ResponseException(what + "." + k + " is not " + type);
    }

    public String str(String k) {
        if (m.get(k) instanceof String s) {
            return s;
        }
        throw bad(k, "a string");
    }

    public String optStr(String k) {
        Object v = m.get(k);
        if (v == null) {
            return null;
        }
        if (v instanceof String s) {
            return s;
        }
        throw bad(k, "a string");
    }

    public double num(String k) {
        Object v = m.get(k);
        if (v == null) {
            return 0;
        }
        if (v instanceof Number n && Double.isFinite(n.doubleValue())) {
            return n.doubleValue();
        }
        throw bad(k, "a number");
    }

    public Double optNum(String k) {
        return m.get(k) == null ? null : num(k);
    }

    public long lng(String k) {
        Object v = m.get(k);
        if (v == null) {
            return 0;
        }
        if (v instanceof Long l) {
            return l;
        }
        if (v instanceof Double d && d == Math.rint(d) && Math.abs(d) < 9e15) {
            return d.longValue();
        }
        throw bad(k, "an integer");
    }

    public Long optLng(String k) {
        return m.get(k) == null ? null : lng(k);
    }

    public boolean bool(String k) {
        Object v = m.get(k);
        if (v == null) {
            return false;
        }
        if (v instanceof Boolean b) {
            return b;
        }
        throw bad(k, "a boolean");
    }

    public Boolean optBool(String k) {
        return m.get(k) == null ? null : bool(k);
    }

    public Fields obj(String k) {
        return of(m.get(k), what + "." + k);
    }

    public Map<String, Object> map(String k) {
        Object v = m.get(k);
        if (v == null) {
            return Map.of();
        }
        return obj(k).raw();
    }

    public Map<String, String> strMap(String k) {
        Map<String, String> out = new LinkedHashMap<>();
        for (Map.Entry<String, Object> e : map(k).entrySet()) {
            if (!(e.getValue() instanceof String s)) {
                throw bad(k, "a string map");
            }
            out.put(e.getKey(), s);
        }
        return Collections.unmodifiableMap(out);
    }

    public <T> List<T> list(String k, Function<Object, T> item) {
        Object v = m.get(k);
        if (v == null) {
            return List.of();
        }
        if (!(v instanceof List<?> l)) {
            throw bad(k, "an array");
        }
        List<T> out = new ArrayList<>(l.size());
        for (Object o : l) {
            out.add(item.apply(o));
        }
        return Collections.unmodifiableList(out);
    }

    public List<String> strList(String k) {
        return list(k, o -> {
            if (o instanceof String s) {
                return s;
            }
            throw bad(k, "a string array");
        });
    }

    public List<Double> numList(String k) {
        return list(k, o -> {
            if (o instanceof Number n) {
                return n.doubleValue();
            }
            throw bad(k, "a number array");
        });
    }
}
