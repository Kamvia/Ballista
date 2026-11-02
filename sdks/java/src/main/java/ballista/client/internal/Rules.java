// File: ballista/sdks/java/src/main/java/ballista/client/internal/Rules.java
package ballista.client.internal;

import ballista.client.exceptions.ConfigurationException;

import java.net.URI;
import java.net.URISyntaxException;
import java.util.Collection;
import java.util.Map;
import java.util.regex.Pattern;

/** Validation rules shared with the server, applied before anything is sent. */
public final class Rules {
    public static final Pattern NAME = Pattern.compile("^[A-Za-z0-9_.-]{1,64}$");
    public static final Pattern IDENT = Pattern.compile("^[A-Za-z_][A-Za-z0-9_]{0,63}$");
    public static final Pattern BUNDLE_ID = Pattern.compile("^[0-9a-f]{32}$");
    public static final Pattern RUN_ID = Pattern.compile("^r[0-9a-f]{24}$");
    public static final Pattern KEY = Pattern.compile("^[\\x21-\\x7e]{1,128}$");
    private static final Pattern DURATION = Pattern.compile(
            "^\\s*\\d+(\\.\\d+)?\\s*(ns|us|ms|s|sec|m|min|h|hr|d)(\\s*\\d+(\\.\\d+)?\\s*(ns|us|ms|s|sec|m|min|h|hr|d))*\\s*$");
    private static final Pattern SEGMENT = Pattern.compile("^[A-Za-z0-9_-][A-Za-z0-9._-]{0,99}$");

    private Rules() {
    }

    public static ConfigurationException fail(String message) {
        return new ConfigurationException(message);
    }

    public static String duration(String d, String what) {
        if (d == null || !DURATION.matcher(d).matches()) {
            throw fail(what + ": '" + d + "' is not a duration such as '500ms' or '2m'");
        }
        return d;
    }

    public static String name(String n, String what) {
        if (n == null || !NAME.matcher(n).matches()) {
            throw fail(what + ": '" + n + "' must match [A-Za-z0-9_.-]{1,64}");
        }
        return n;
    }

    public static void names(Collection<String> list, int max, String what) {
        if (list.size() > max) {
            throw fail(what + ": at most " + max + " names");
        }
        for (String t : list) {
            name(t, what);
        }
    }

    public static String ident(String v, String what) {
        if (v == null || !IDENT.matcher(v).matches()) {
            throw fail(what + ": variable '" + v + "' must be an identifier");
        }
        return v;
    }

    public static String runId(String id) {
        if (id == null || !RUN_ID.matcher(id).matches()) {
            throw fail("invalid run id '" + id + "'");
        }
        return id;
    }

    public static String bundleId(String id) {
        if (id == null || !BUNDLE_ID.matcher(id).matches()) {
            throw fail("invalid bundle id '" + id + "'");
        }
        return id;
    }

    public static boolean finite(double v) {
        return !Double.isNaN(v) && !Double.isInfinite(v);
    }

    /** An http(s) URL with a host and no credentials or whitespace. */
    public static boolean baseUrl(String raw) {
        if (raw == null || raw.chars().anyMatch(Character::isWhitespace)) {
            return false;
        }
        try {
            URI u = new URI(raw);
            String scheme = u.getScheme();
            return ("http".equals(scheme) || "https".equals(scheme)) && u.getHost() != null && u.getRawUserInfo() == null;
        } catch (URISyntaxException e) {
            return false;
        }
    }

    public static Map<String, String> labels(Map<String, String> labels) {
        if (labels.size() > 32) {
            throw fail("at most 32 labels");
        }
        for (Map.Entry<String, String> e : labels.entrySet()) {
            if (e.getKey() == null || !NAME.matcher(e.getKey()).matches()) {
                throw fail("label key '" + e.getKey() + "' must match [A-Za-z0-9_.-]{1,64}");
            }
            String v = e.getValue();
            if (v == null || v.codePointCount(0, v.length()) > 256 || v.chars().anyMatch(c -> c < 0x20 || c == 0x7f)) {
                throw fail("label '" + e.getKey() + "' must be up to 256 printable characters");
            }
        }
        return Map.copyOf(labels);
    }

    /** Mirrors the server rule: relative, at most 8 segments, no hidden or parent segments. */
    public static boolean bundlePath(String rel) {
        if (rel == null || rel.isEmpty() || rel.length() > 200 || rel.startsWith("/")) {
            return false;
        }
        String[] parts = rel.split("/", -1);
        if (parts.length > 8 || parts[0].equals("inline") || rel.equals("plan.toml") || rel.equals("bundle.json")) {
            return false;
        }
        for (String p : parts) {
            if (!SEGMENT.matcher(p).matches()) {
                return false;
            }
        }
        return true;
    }
}
