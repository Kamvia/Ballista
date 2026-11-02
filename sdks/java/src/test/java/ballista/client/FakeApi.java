// File: ballista/sdks/java/src/test/java/ballista/client/FakeApi.java
package ballista.client;

import ballista.client.internal.Json;

import com.sun.net.httpserver.HttpExchange;
import com.sun.net.httpserver.HttpServer;

import java.io.IOException;
import java.io.OutputStream;
import java.net.InetSocketAddress;
import java.net.URLDecoder;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.time.Duration;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.HexFormat;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;
import java.util.function.BiConsumer;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

/** In-memory run manager used by the unit tests. */
final class FakeApi implements AutoCloseable {
    static final String TOKEN = "bt_0123456789abcdef0123456789abcdef";

    private final HttpServer server;
    private final List<Map<String, Object>> runs = new ArrayList<>();
    private final Map<String, Map<String, Object>> bundles = new LinkedHashMap<>();
    private final Map<String, String> keys = new HashMap<>();
    private final Map<String, BiConsumer<HttpExchange, Matcher>> overrides = new ConcurrentHashMap<>();

    final Map<String, Object> bodies = new ConcurrentHashMap<>();
    final Map<String, Map<String, String>> queries = new ConcurrentHashMap<>();
    final Map<String, Map<String, String>> headers = new ConcurrentHashMap<>();
    final Map<String, Integer> calls = new ConcurrentHashMap<>();
    volatile String finalState = "passed";

    FakeApi() throws IOException {
        server = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        server.createContext("/", ex -> {
            try {
                handle(ex);
            } catch (RuntimeException e) {
                reply(ex, 500, "{\"error\":{\"code\":\"fake\",\"message\":\"" + e.getClass().getSimpleName() + "\"}}");
            } finally {
                ex.close();
            }
        });
        server.start();
    }

    String url() {
        return "http://127.0.0.1:" + server.getAddress().getPort();
    }

    Client client() {
        return Client.builder().url(url()).token(TOKEN).retry(new RetryPolicy(3, Duration.ofMillis(1), Duration.ofMillis(5))).build();
    }

    /** Replaces the handler of routes matching regex (matched against "METHOD /path"). */
    void on(String regex, BiConsumer<HttpExchange, Matcher> handler) {
        overrides.put(regex, handler);
    }

    @Override
    public void close() {
        server.stop(0);
    }

    static void reply(HttpExchange ex, int status, String body) {
        try {
            byte[] b = body.getBytes(StandardCharsets.UTF_8);
            ex.getResponseHeaders().set("Content-Type", "application/json");
            ex.sendResponseHeaders(status, b.length == 0 ? -1 : b.length);
            if (b.length > 0) {
                try (OutputStream o = ex.getResponseBody()) {
                    o.write(b);
                }
            }
        } catch (IOException e) {
            // client went away
        }
    }

    static void json(HttpExchange ex, int status, Object v) {
        reply(ex, status, Json.write(v));
    }

    private static void notFound(HttpExchange ex) {
        reply(ex, 404, "{\"error\":{\"code\":\"not_found\",\"message\":\"no such resource\"}}");
    }

    private static Map<String, String> query(HttpExchange ex) {
        Map<String, String> q = new LinkedHashMap<>();
        String raw = ex.getRequestURI().getRawQuery();
        if (raw != null) {
            for (String part : raw.split("&")) {
                int i = part.indexOf('=');
                q.put(URLDecoder.decode(part.substring(0, i), StandardCharsets.UTF_8), URLDecoder.decode(part.substring(i + 1), StandardCharsets.UTF_8));
            }
        }
        return q;
    }

    @SuppressWarnings("unchecked")
    private Map<String, Object> record(String label, HttpExchange ex) throws IOException {
        calls.merge(label, 1, Integer::sum);
        queries.put(label, query(ex));
        Map<String, String> h = new HashMap<>();
        ex.getRequestHeaders().forEach((k, v) -> h.put(k.toLowerCase(), v.getFirst()));
        headers.put(label, h);
        String body = new String(ex.getRequestBody().readAllBytes(), StandardCharsets.UTF_8);
        Map<String, Object> parsed = body.isEmpty() ? Map.of() : (Map<String, Object>) Json.parse(body);
        bodies.put(label, parsed);
        return parsed;
    }

    static Map<String, Object> bundleJson(String id) {
        Map<String, Object> m = new LinkedHashMap<>();
        m.put("id", id);
        m.put("plan_name", "p");
        m.put("scenarios", List.of("s"));
        m.put("files", List.of());
        m.put("size", 10);
        m.put("labels", Map.of());
        m.put("created_at", "2026-11-02T00:00:00Z");
        m.put("created_by", "ci");
        return m;
    }

    private Map<String, Object> run(String id) {
        return runs.stream().filter(r -> r.get("id").equals(id)).findFirst().orElse(null);
    }

    private static String sha(String s) {
        try {
            return HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256").digest(s.getBytes(StandardCharsets.UTF_8))).substring(0, 32);
        } catch (NoSuchAlgorithmException e) {
            throw new IllegalStateException(e);
        }
    }

    @SuppressWarnings("unchecked")
    private synchronized void handle(HttpExchange ex) throws IOException {
        if (!("Bearer " + TOKEN).equals(ex.getRequestHeaders().getFirst("Authorization"))) {
            reply(ex, 401, "{\"error\":{\"code\":\"unauthorized\",\"message\":\"invalid token\"}}");
            return;
        }
        String route = ex.getRequestMethod() + " " + ex.getRequestURI().getPath();
        for (Map.Entry<String, BiConsumer<HttpExchange, Matcher>> o : overrides.entrySet()) {
            Matcher m = Pattern.compile(o.getKey()).matcher(route);
            if (m.matches()) {
                o.getValue().accept(ex, m);
                return;
            }
        }
        Matcher m;
        if (route.equals("GET /api/v1/health")) {
            record("health", ex);
            json(ex, 200, Map.of("status", "ok", "version", "0.1.0"));
        } else if (route.equals("GET /api/v1/whoami")) {
            json(ex, 200, Map.of("name", "ci", "scope", "run"));
        } else if (route.equals("GET /api/v1/status")) {
            Map<String, Object> worker = new LinkedHashMap<>(Map.of("id", 1, "name", "pool-a", "address", "10.0.0.2", "cores", 8, "state", "ready"));
            worker.put("cpu_pct", null);
            Map<String, Object> st = new LinkedHashMap<>();
            st.put("version", "0.1.0");
            st.put("active_run", null);
            st.put("queued", 1);
            st.put("limits", Map.of("max_users", 5000, "max_run_time_secs", 3600, "max_queue", 50));
            st.put("workers", List.of(worker));
            json(ex, 200, st);
        } else if (route.equals("POST /api/v1/plans/validate")) {
            Map<String, Object> body = record("validate", ex);
            String name = body.get("plan") instanceof Map<?, ?> doc ? (String) doc.get("name") : "text";
            json(ex, 200, Map.of("name", name, "scenarios", List.of("s"), "files", List.of(), "document", Map.of("name", name)));
        } else if (route.equals("POST /api/v1/bundles")) {
            Map<String, Object> body = record("upload", ex);
            String id = sha(Json.write(body.get("plan")));
            bundles.put(id, body);
            json(ex, 201, bundleJson(id));
        } else if (route.equals("GET /api/v1/bundles")) {
            json(ex, 200, Map.of("items", bundles.keySet().stream().map(FakeApi::bundleJson).toList()));
        } else if ((m = Pattern.compile("(GET|DELETE) /api/v1/bundles/(\\w+)").matcher(route)).matches()) {
            if (!bundles.containsKey(m.group(2))) {
                notFound(ex);
            } else if (m.group(1).equals("DELETE")) {
                bundles.remove(m.group(2));
                reply(ex, 204, "");
            } else {
                json(ex, 200, bundleJson(m.group(2)));
            }
        } else if (route.equals("POST /api/v1/runs")) {
            Map<String, Object> body = record("create", ex);
            String key = ex.getRequestHeaders().getFirst("Idempotency-Key");
            if (keys.containsKey(key)) {
                json(ex, 200, run(keys.get(key)));
                return;
            }
            if (!bundles.containsKey((String) body.get("bundle"))) {
                notFound(ex);
                return;
            }
            Map<String, Object> r = new LinkedHashMap<>();
            r.put("id", String.format("r%024x", runs.size() + 1));
            r.put("seq", runs.size() + 1);
            r.put("bundle", body.get("bundle"));
            r.put("plan_name", "p");
            r.put("state", "queued");
            r.put("created_at", "2026-11-02T00:00:00Z");
            r.put("created_by", "ci");
            r.put("labels", body.get("labels"));
            r.put("overrides", body.get("overrides"));
            r.put("queue_position", 0);
            if (body.containsKey("regression")) {
                r.put("regression", body.get("regression"));
            }
            runs.add(r);
            keys.put(key, (String) r.get("id"));
            json(ex, 202, r);
        } else if (route.equals("GET /api/v1/runs")) {
            Map<String, String> q = query(ex);
            record("list", ex);
            int limit = Integer.parseInt(q.get("limit"));
            int start = Integer.parseInt(q.getOrDefault("cursor", "0"));
            List<Map<String, Object>> match = new ArrayList<>();
            for (int i = runs.size() - 1; i >= 0; i--) {
                if (!q.containsKey("state") || runs.get(i).get("state").equals(q.get("state"))) {
                    match.add(runs.get(i));
                }
            }
            int end = Math.min(start + limit, match.size());
            Map<String, Object> page = new LinkedHashMap<>();
            page.put("items", match.subList(Math.min(start, match.size()), end));
            page.put("next_cursor", end < match.size() ? Integer.toString(end) : null);
            json(ex, 200, page);
        } else if ((m = Pattern.compile("(GET|POST|DELETE) /api/v1/runs/(r\\w+)(/\\w+)?(/[\\w.]+)?").matcher(route)).matches()) {
            String sub = m.group(3) == null ? "" : m.group(3);
            Map<String, Object> r = run(m.group(2));
            switch (m.group(1) + sub) {
                case "DELETE" -> {
                    record("delete-run", ex);
                    reply(ex, 204, "");
                }
                case "GET/stats" -> {
                    Map<String, Object> s = new LinkedHashMap<>();
                    s.put("plan", "p");
                    s.put("elapsed_secs", 5.2);
                    s.put("users", 4);
                    s.put("total", Map.of("name", "Aggregated", "count", 40, "p95_ms", 9.5));
                    s.put("rows", List.of(Map.of("method", "GET", "name", "item", "count", 30), Map.of("method", "SSE", "name", "item", "count", 3)));
                    s.put("transactions", List.of(Map.of("method", "TXN", "name", "purchase", "count", 10)));
                    Map<String, Object> overflow = new LinkedHashMap<>();
                    overflow.put("upper_ms", null);
                    overflow.put("count", 1);
                    s.put("distribution", List.of(Map.of("upper_ms", 10, "count", 30), overflow));
                    json(ex, 200, s);
                }
                case "GET/compare" -> {
                    record("compare", ex);
                    Map<String, Object> c = new LinkedHashMap<>();
                    c.put("baseline", "r000000000000000000000001");
                    c.put("passed", false);
                    c.put("regressions", 1);
                    c.put("changes", List.of(Map.of("kind", "request", "method", "GET", "name", "item", "metric", "p95", "baseline", 5.0,
                            "current", 9.0, "change_pct", 80.0, "regressed", true)));
                    c.put("unmatched", List.of("GET /gone"));
                    c.put("note", null);
                    json(ex, 200, c);
                }
                case "GET/artifacts" -> {
                    if (m.group(4).equals("/report.json")) {
                        json(ex, 200, Map.of("plan", "p"));
                    } else {
                        reply(ex, 200, "<!DOCTYPE html><title>report</title>");
                    }
                }
                default -> {
                    if (r == null) {
                        notFound(ex);
                    } else if (sub.equals("/stop")) {
                        r.put("state", "cancelled");
                        r.put("stopped_by", "ci");
                        json(ex, 200, r);
                    } else if (sub.equals("/events")) {
                        r.put("state", finalState);
                        r.put("summary", Map.of("requests", 40, "failures", 0, "thresholds", List.of()));
                        ex.getResponseHeaders().set("Content-Type", "text/event-stream");
                        String body = "event: stats\ndata: {\"elapsed_secs\":1,\"users\":2,\"total\":{\"count\":5}}\n\nevent: end\ndata: " + Json.write(r) + "\n\n";
                        byte[] b = body.getBytes(StandardCharsets.UTF_8);
                        ex.sendResponseHeaders(200, b.length);
                        try (OutputStream o = ex.getResponseBody()) {
                            o.write(b);
                        }
                    } else {
                        json(ex, 200, r);
                    }
                }
            }
        } else {
            notFound(ex);
        }
    }
}
