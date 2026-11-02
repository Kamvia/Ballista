// File: ballista/sdks/java/src/main/java/ballista/client/Client.java
package ballista.client;

import ballista.client.exceptions.ApiException;
import ballista.client.exceptions.BallistaException;
import ballista.client.exceptions.ConfigurationException;
import ballista.client.exceptions.ResponseException;
import ballista.client.exceptions.RunTimeoutException;
import ballista.client.exceptions.TransportException;
import ballista.client.internal.Fields;
import ballista.client.internal.Json;
import ballista.client.internal.Rules;
import ballista.client.internal.SseParser;
import ballista.client.internal.Tls;
import ballista.client.models.BundleInfo;
import ballista.client.models.CompareMetric;
import ballista.client.models.Comparison;
import ballista.client.models.Health;
import ballista.client.models.Overrides;
import ballista.client.models.Page;
import ballista.client.models.PlanCheck;
import ballista.client.models.Run;
import ballista.client.models.RunEvent;
import ballista.client.models.RunState;
import ballista.client.models.ServerStatus;
import ballista.client.models.Snapshot;
import ballista.client.models.WhoAmI;
import ballista.client.plan.Plan;

import java.io.IOException;
import java.io.InputStream;
import java.net.InetAddress;
import java.net.ProxySelector;
import java.net.URI;
import java.net.URISyntaxException;
import java.net.URLEncoder;
import java.net.UnknownHostException;
import java.net.http.HttpClient;
import java.net.http.HttpHeaders;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.nio.file.attribute.PosixFilePermissions;
import java.time.Duration;
import java.util.Iterator;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.NoSuchElementException;
import java.util.Spliterator;
import java.util.Spliterators;
import java.util.UUID;
import java.util.concurrent.ThreadLocalRandom;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicLong;
import java.util.function.Function;
import java.util.function.Predicate;
import java.util.stream.Collectors;
import java.util.stream.Stream;
import java.util.stream.StreamSupport;

import javax.net.ssl.SSLContext;

/** Client for the run manager API. Thread-safe; every method blocks and suits virtual threads. */
public final class Client {
    public static final String TOKEN_ENV = "BALLISTA_TOKEN";
    public static final String URL_ENV = "BALLISTA_URL";
    public static final List<String> ARTIFACTS = List.of("report.json", "report.html", "stats.csv", "history.csv", "failures.csv");

    private static final String USER_AGENT = "ballista-client-java/0.1.0";
    private static final int MAX_JSON = 32 << 20;
    private static final int MAX_ARTIFACT = 512 << 20;
    private static final int MAX_ERROR = 64 << 10;

    private final String baseUrl;
    private final String token;
    private final Duration timeout;
    private final RetryPolicy retry;
    private final HttpClient http;

    private Client(Builder b) {
        this.baseUrl = baseUrl(b.url, b.allowInsecureHttp);
        String t = b.token != null ? b.token.strip() : env(TOKEN_ENV);
        if (t.length() < 32 || t.length() > 4096 || t.chars().anyMatch(c -> c < 0x21 || c > 0x7e)) {
            throw new ConfigurationException("api token required (option or " + TOKEN_ENV + ")");
        }
        this.token = t;
        this.timeout = b.timeout;
        this.retry = b.retry;
        HttpClient.Builder hb = HttpClient.newBuilder()
                .followRedirects(HttpClient.Redirect.NEVER)
                .connectTimeout(Duration.ofSeconds(10));
        if (b.sslContext != null) {
            hb.sslContext(b.sslContext);
        } else if (b.caCertFile != null) {
            hb.sslContext(Tls.withExtraCa(b.caCertFile));
        }
        if (b.proxy != null) {
            hb.proxy(b.proxy);
        }
        this.http = hb.build();
    }

    public static Builder builder() {
        return new Builder();
    }

    /** A client configured from BALLISTA_URL and BALLISTA_TOKEN. */
    public static Client fromEnv() {
        return builder().build();
    }

    public String baseUrl() {
        return baseUrl;
    }

    @Override
    public String toString() {
        return "Client(" + baseUrl + ")";
    }

    private static String env(String key) {
        String v = System.getenv(key);
        return v == null ? "" : v.strip();
    }

    private static boolean loopback(String host) {
        if (host.equals("localhost")) {
            return true;
        }
        String h = host.startsWith("[") && host.endsWith("]") ? host.substring(1, host.length() - 1) : host;
        if (!h.matches("[0-9.]+") && !h.contains(":")) {
            return false;
        }
        try {
            return InetAddress.getByName(h).isLoopbackAddress();
        } catch (UnknownHostException e) {
            return false;
        }
    }

    private static String baseUrl(String raw, boolean allowInsecure) {
        String value = raw != null && !raw.isBlank() ? raw.strip() : env(URL_ENV);
        if (value.isEmpty()) {
            throw new ConfigurationException("server url required (option or " + URL_ENV + ")");
        }
        URI u;
        try {
            u = new URI(value);
        } catch (URISyntaxException e) {
            throw new ConfigurationException("server url is not a valid URL");
        }
        String scheme = u.getScheme();
        if (!("http".equals(scheme) || "https".equals(scheme)) || u.getRawUserInfo() != null || u.getHost() == null) {
            throw new ConfigurationException("server url must be http(s)://host[:port] without credentials");
        }
        if (scheme.equals("http") && !loopback(u.getHost()) && !allowInsecure) {
            throw new ConfigurationException("refusing to send the token over plain http to a remote host; use https");
        }
        return scheme + "://" + u.getRawAuthority() + "/api/v1";
    }

    // ------------------------------------------------------------------ transport

    private record Response(int status, HttpHeaders headers, byte[] body) {
    }

    private static boolean retryable(int status) {
        return status == 429 || status == 502 || status == 503 || status == 504;
    }

    private static Duration retryAfter(HttpHeaders h) {
        return h.firstValue("retry-after").map(v -> {
            try {
                long s = Long.parseLong(v.strip());
                return s >= 0 ? Duration.ofSeconds(s) : null;
            } catch (NumberFormatException e) {
                return null;
            }
        }).orElse(null);
    }

    private Duration delay(int attempt, Duration after) {
        long max = retry.maxDelay().toMillis();
        if (after != null) {
            return Duration.ofMillis(Math.min(after.toMillis(), max));
        }
        long backoff = Math.min(retry.baseDelay().toMillis() << Math.min(attempt, 20), max);
        return Duration.ofMillis((long) (backoff * (0.5 + ThreadLocalRandom.current().nextDouble() / 2)));
    }

    private static void sleep(Duration d) {
        try {
            Thread.sleep(d);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            throw new TransportException("interrupted", e);
        }
    }

    private static ApiException apiError(int status, HttpHeaders h, byte[] body) {
        String code = "error";
        String detail = "request failed";
        try {
            Fields err = Fields.of(Json.parse(new String(body, StandardCharsets.UTF_8)), "error").obj("error");
            if (err.optStr("code") != null) {
                code = err.optStr("code");
            }
            if (err.optStr("message") != null) {
                detail = err.optStr("message");
            }
        } catch (RuntimeException ignored) {
            // non-JSON error body
        }
        if (detail.length() > 1000) {
            detail = detail.substring(0, 1000);
        }
        return ApiException.of(status, code, detail, retryAfter(h));
    }

    private static String query(Map<String, String> q) {
        if (q == null || q.isEmpty()) {
            return "";
        }
        return "?" + q.entrySet().stream()
                .map(e -> URLEncoder.encode(e.getKey(), StandardCharsets.UTF_8) + "=" + URLEncoder.encode(e.getValue(), StandardCharsets.UTF_8))
                .collect(Collectors.joining("&"));
    }

    private HttpRequest.Builder request(String path, String accept) {
        return HttpRequest.newBuilder(URI.create(baseUrl + path))
                .header("Authorization", "Bearer " + token)
                .header("User-Agent", USER_AGENT)
                .header("Accept", accept);
    }

    private byte[] send(String method, String path, Map<String, String> q, Object body, Map<String, String> headers, int limit) {
        String payload = null;
        if (body != null) {
            try {
                payload = Json.write(body);
            } catch (Json.JsonException e) {
                throw new ConfigurationException("request body cannot be encoded: " + e.getMessage());
            }
        }
        String op = method + " " + path;
        for (int attempt = 0; ; attempt++) {
            boolean last = attempt + 1 >= retry.attempts();
            HttpRequest.Builder rb = request(path + query(q), "application/json").timeout(timeout);
            headers.forEach(rb::header);
            if (payload != null) {
                rb.header("Content-Type", "application/json").method(method, HttpRequest.BodyPublishers.ofString(payload));
            } else {
                rb.method(method, HttpRequest.BodyPublishers.noBody());
            }
            Response res;
            try {
                HttpResponse<InputStream> r = http.send(rb.build(), HttpResponse.BodyHandlers.ofInputStream());
                boolean ok = r.statusCode() >= 200 && r.statusCode() < 300;
                int cap = ok ? limit : MAX_ERROR;
                byte[] data;
                try (InputStream in = r.body()) {
                    data = in.readNBytes(cap + 1);
                }
                if (data.length > cap) {
                    if (ok) {
                        throw new TransportException(op + ": response exceeds " + cap + " bytes", null);
                    }
                    data = java.util.Arrays.copyOf(data, cap);
                }
                res = new Response(r.statusCode(), r.headers(), data);
            } catch (IOException e) {
                if (last) {
                    throw new TransportException(op + ": " + e.getClass().getSimpleName(), e);
                }
                sleep(delay(attempt, null));
                continue;
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                throw new TransportException(op + ": interrupted", e);
            }
            if (retryable(res.status()) && !last) {
                sleep(delay(attempt, retryAfter(res.headers())));
                continue;
            }
            if (res.status() < 200 || res.status() > 299) {
                throw apiError(res.status(), res.headers(), res.body());
            }
            return res.body();
        }
    }

    private Object json(String method, String path, Map<String, String> q, Object body, Map<String, String> headers) {
        byte[] data = send(method, path, q, body, headers, MAX_JSON);
        if (data.length == 0) {
            return null;
        }
        try {
            return Json.parse(new String(data, StandardCharsets.UTF_8));
        } catch (Json.JsonException e) {
            throw new ResponseException("body of " + path + " is not valid JSON");
        }
    }

    private Object get(String path) {
        return json("GET", path, null, null, Map.of());
    }

    // ------------------------------------------------------------------ endpoints

    public Health health() {
        return Health.from(get("/health"));
    }

    public WhoAmI whoami() {
        return WhoAmI.from(get("/whoami"));
    }

    public ServerStatus status() {
        return ServerStatus.from(get("/status"));
    }

    /** Validates a plan without storing it and lists files a bundle must still carry. */
    public PlanCheck validatePlan(Object plan) {
        return validatePlan(plan, null);
    }

    /** Validates a plan as it would run with overrides; plan is a {@link Plan}, a document map or plan text. */
    public PlanCheck validatePlan(Object plan, Overrides overrides) {
        Map<String, Object> body = new LinkedHashMap<>();
        body.put("plan", Bundle.payload(plan));
        if (overrides != null) {
            body.put("overrides", overrides.validate());
        }
        return PlanCheck.from(json("POST", "/plans/validate", null, body, Map.of()));
    }

    /** Uploads a plan with assets; identical content returns the stored bundle, so it is safe to retry. */
    public BundleInfo uploadBundle(Bundle bundle) {
        return BundleInfo.from(json("POST", "/bundles", null, bundle.toPayload(), Map.of()));
    }

    /** Uploads a plan without assets. */
    public BundleInfo uploadPlan(Object plan) {
        return uploadBundle(plan instanceof Bundle b ? b : new Bundle(plan));
    }

    public List<BundleInfo> listBundles() {
        return Page.from(get("/bundles"), BundleInfo::from).items();
    }

    public BundleInfo getBundle(String id) {
        return BundleInfo.from(get("/bundles/" + Rules.bundleId(id)));
    }

    public void deleteBundle(String id) {
        send("DELETE", "/bundles/" + Rules.bundleId(id), null, null, Map.of(), MAX_ERROR);
    }

    /** Queues a run of a stored bundle. A generated idempotency key makes retries safe against duplicates. */
    public Run createRun(String bundleId, RunOptions opts) {
        RunOptions o = opts == null ? RunOptions.DEFAULT : opts;
        String key = o.idempotencyKey() == null ? UUID.randomUUID().toString() : o.idempotencyKey();
        if (!Rules.KEY.matcher(key).matches()) {
            throw new ConfigurationException("idempotency key must be 1..128 printable characters");
        }
        Map<String, Object> body = new LinkedHashMap<>();
        body.put("bundle", Rules.bundleId(bundleId));
        body.put("overrides", o.overrides() == null ? Map.of() : o.overrides().validate());
        body.put("labels", Rules.labels(o.labels()));
        if (o.regression() != null) {
            body.put("regression", o.regression().validate());
        }
        return Run.from(json("POST", "/runs", null, body, Map.of("Idempotency-Key", key)));
    }

    public Run createRun(BundleInfo bundle, RunOptions opts) {
        return createRun(bundle.id(), opts);
    }

    /** One page of runs, newest first; state may be null and cursor comes from the previous page. */
    public Page<Run> listRuns(RunState state, int limit, String cursor) {
        Map<String, String> q = new LinkedHashMap<>();
        q.put("limit", Integer.toString(Math.max(1, Math.min(limit <= 0 ? 50 : limit, 200))));
        if (state != null) {
            q.put("state", state.wire());
        }
        if (cursor != null && !cursor.isEmpty()) {
            q.put("cursor", cursor);
        }
        return Page.from(json("GET", "/runs", q, null, Map.of()), Run::from);
    }

    /** Every run (newest first), fetching pages lazily. */
    public Stream<Run> runs(RunState state, int pageSize) {
        Iterator<Run> it = new Iterator<>() {
            private Iterator<Run> page = List.<Run>of().iterator();
            private String cursor;
            private boolean done;

            @Override
            public boolean hasNext() {
                while (!page.hasNext() && !done) {
                    Page<Run> p = listRuns(state, pageSize <= 0 ? 100 : pageSize, cursor);
                    page = p.items().iterator();
                    cursor = p.nextCursor();
                    done = cursor == null || cursor.isEmpty();
                }
                return page.hasNext();
            }

            @Override
            public Run next() {
                if (!hasNext()) {
                    throw new NoSuchElementException();
                }
                return page.next();
            }
        };
        return StreamSupport.stream(Spliterators.spliteratorUnknownSize(it, Spliterator.ORDERED | Spliterator.NONNULL), false);
    }

    public Run getRun(String id) {
        return Run.from(get("/runs/" + Rules.runId(id)));
    }

    /** Stops a running run or cancels a queued one. */
    public Run stopRun(String id) {
        return Run.from(json("POST", "/runs/" + Rules.runId(id) + "/stop", null, null, Map.of()));
    }

    public void deleteRun(String id) {
        send("DELETE", "/runs/" + Rules.runId(id), null, null, Map.of(), MAX_ERROR);
    }

    /** Latest statistics of a running or finished run. */
    public Snapshot getStats(String id) {
        return Snapshot.from(get("/runs/" + Rules.runId(id) + "/stats"));
    }

    /** Compares a finished run with a baseline (default: the latest earlier passed run of the same plan). */
    public Comparison compare(String id, CompareOptions opts) {
        CompareOptions o = opts == null ? CompareOptions.DEFAULT : opts;
        new ballista.client.models.RegressionGate(o.baseline(), o.maxRegressionPct(), o.metrics(), null, null, null).validate();
        Map<String, String> q = new LinkedHashMap<>();
        q.put("baseline", o.baseline() == null ? "auto" : o.baseline());
        if (o.maxRegressionPct() != null) {
            q.put("max_regression_pct", Double.toString(o.maxRegressionPct()));
        }
        if (o.metrics() != null) {
            q.put("metrics", o.metrics().stream().map(CompareMetric::wire).collect(Collectors.joining(",")));
        }
        return Comparison.from(json("GET", "/runs/" + Rules.runId(id) + "/compare", q, null, Map.of()));
    }

    /** Downloads one of {@link #ARTIFACTS}. */
    public byte[] artifact(String id, String name) {
        if (!ARTIFACTS.contains(name)) {
            throw new ConfigurationException("unknown artifact '" + name + "'");
        }
        return send("GET", "/runs/" + Rules.runId(id) + "/artifacts/" + name, null, null, Map.of(), MAX_ARTIFACT);
    }

    /** The parsed report.json of a finished run. */
    public Map<String, Object> report(String id) {
        try {
            return Fields.of(Json.parse(new String(artifact(id, "report.json"), StandardCharsets.UTF_8)), "report").raw();
        } catch (Json.JsonException e) {
            throw new ResponseException("report.json is not valid JSON");
        }
    }

    /** Writes an artifact to dest (a file path, or an existing directory to place it in) atomically. */
    public Path download(String id, String name, Path dest) {
        byte[] data = artifact(id, name);
        Path target = Files.isDirectory(dest) ? dest.resolve(id + "-" + name) : dest;
        Path dir = target.toAbsolutePath().getParent();
        try {
            Path tmp = Files.createTempFile(dir, ".download-", ".part");
            try {
                Files.write(tmp, data);
                try {
                    Files.setPosixFilePermissions(tmp, PosixFilePermissions.fromString("rw-r--r--"));
                } catch (UnsupportedOperationException ignored) {
                    // non-POSIX file system
                }
                Files.move(tmp, target, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE);
            } finally {
                Files.deleteIfExists(tmp);
            }
            return target;
        } catch (IOException e) {
            throw new BallistaException("cannot write " + target + ": " + e.getMessage(), e);
        }
    }

    // ------------------------------------------------------------------ events and waiting

    /**
     * Streams run events to handler until the run finishes (end is the last event) or handler returns false.
     * Fails with a TransportException when no data arrives for idleTimeout.
     */
    public void events(String id, Duration idleTimeout, Predicate<RunEvent> handler) {
        events(id, idleTimeout, Long.MAX_VALUE, handler);
    }

    @SuppressWarnings("try")
    private void events(String id, Duration idleTimeout, long deadlineNanos, Predicate<RunEvent> handler) {
        String path = "/runs/" + Rules.runId(id) + "/events";
        long idle = (idleTimeout == null ? Duration.ofSeconds(60) : idleTimeout).toNanos();
        HttpResponse<InputStream> res;
        try {
            res = http.send(request(path, "text/event-stream").GET().build(), HttpResponse.BodyHandlers.ofInputStream());
        } catch (IOException e) {
            throw new TransportException("GET " + path + ": " + e.getClass().getSimpleName(), e);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            throw new TransportException("GET " + path + ": interrupted", e);
        }
        try (InputStream in = res.body()) {
            if (res.statusCode() < 200 || res.statusCode() > 299) {
                throw apiError(res.statusCode(), res.headers(), in.readNBytes(MAX_ERROR));
            }
            AtomicLong lastActivity = new AtomicLong(System.nanoTime());
            AtomicBoolean finished = new AtomicBoolean();
            AtomicBoolean idled = new AtomicBoolean();
            Thread watchdog = Thread.ofVirtual().name("event-stream-watchdog").start(() -> {
                while (!finished.get()) {
                    long now = System.nanoTime();
                    if (now - lastActivity.get() > idle || now - deadlineNanos > 0) {
                        idled.set(true);
                        try {
                            in.close();
                        } catch (IOException ignored) {
                            // closing aborts the blocked read
                        }
                        return;
                    }
                    try {
                        Thread.sleep(Duration.ofMillis(100));
                    } catch (InterruptedException e) {
                        return;
                    }
                }
            });
            try {
                SseParser parser = new SseParser(in, () -> lastActivity.set(System.nanoTime()));
                while (true) {
                    SseParser.Message msg;
                    try {
                        msg = parser.next();
                    } catch (IOException e) {
                        throw new TransportException("event stream interrupted: " + (idled.get() ? "idle timeout" : e.getClass().getSimpleName()), e);
                    }
                    if (msg == null) {
                        throw new TransportException("event stream interrupted: " + (idled.get() ? "idle timeout" : "closed"), null);
                    }
                    RunEvent ev = switch (msg.event()) {
                        case "stats" -> new RunEvent(RunEvent.Type.STATS, null, Snapshot.from(parse(msg.data())));
                        case "run" -> new RunEvent(RunEvent.Type.RUN, Run.from(parse(msg.data())), null);
                        case "end" -> new RunEvent(RunEvent.Type.END, Run.from(parse(msg.data())), null);
                        default -> null;
                    };
                    if (ev == null) {
                        continue;
                    }
                    if (!handler.test(ev) || ev.type() == RunEvent.Type.END) {
                        return;
                    }
                }
            } finally {
                finished.set(true);
                watchdog.interrupt();
            }
        } catch (IOException e) {
            throw new TransportException("GET " + path + ": " + e.getClass().getSimpleName(), e);
        }
    }

    private static Object parse(String data) {
        try {
            return Json.parse(data);
        } catch (Json.JsonException e) {
            throw new ResponseException("event is not valid JSON");
        }
    }

    /** Returns when the run finishes. Follows the event stream and falls back to polling. */
    public Run waitFor(String id, WaitOptions opts) {
        Rules.runId(id);
        WaitOptions o = opts == null ? WaitOptions.DEFAULT : opts;
        long deadline = o.timeout() == null ? Long.MAX_VALUE : System.nanoTime() + o.timeout().toNanos();
        Duration poll = o.pollInterval() == null ? Duration.ofSeconds(2) : o.pollInterval();
        Duration idle = poll.multipliedBy(10).compareTo(Duration.ofSeconds(30)) > 0 ? poll.multipliedBy(10) : Duration.ofSeconds(30);
        Run[] result = new Run[1];
        while (true) {
            try {
                events(id, idle, deadline, ev -> {
                    switch (ev.type()) {
                        case STATS -> {
                            if (o.onStats() != null) {
                                o.onStats().accept(ev.stats());
                            }
                        }
                        case RUN, END -> {
                            if (o.onState() != null) {
                                o.onState().accept(ev.run());
                            }
                            if (ev.type() == RunEvent.Type.END) {
                                result[0] = ev.run();
                            }
                        }
                    }
                    return true;
                });
                if (result[0] != null) {
                    return result[0];
                }
            } catch (TransportException e) {
                if (System.nanoTime() - deadline > 0) {
                    throw new RunTimeoutException(id, o.timeout());
                }
            } catch (ApiException e) {
                if (!retryable(e.status())) {
                    throw e;
                }
            }
            if (System.nanoTime() - deadline > 0) {
                throw new RunTimeoutException(id, o.timeout());
            }
            Run run = getRun(id);
            if (run.isFinished()) {
                return run;
            }
            long left = deadline - System.nanoTime();
            if (left <= 0) {
                throw new RunTimeoutException(id, o.timeout());
            }
            sleep(Duration.ofNanos(Math.min(left, poll.toNanos())));
        }
    }

    /**
     * Uploads (a {@link Plan}, {@link Bundle} or document map), queues and, unless noWait is set, waits for a run.
     * A String or {@link BundleInfo} refers to a stored bundle.
     */
    public Run run(Object target, RunOptions opts) {
        RunOptions o = opts == null ? RunOptions.DEFAULT : opts;
        Function<Object, String> resolve = t -> switch (t) {
            case String s -> s;
            case BundleInfo b -> b.id();
            case Bundle b -> uploadBundle(b).id();
            default -> uploadPlan(t).id();
        };
        Run run = createRun(resolve.apply(target), o);
        return o.noWait() ? run : waitFor(run.id(), o.waitOptions());
    }

    /** Configures a {@link Client}; URL and token fall back to BALLISTA_URL and BALLISTA_TOKEN. */
    public static final class Builder {
        private String url;
        private String token;
        private Duration timeout = Duration.ofSeconds(30);
        private RetryPolicy retry = RetryPolicy.DEFAULT;
        private boolean allowInsecureHttp;
        private Path caCertFile;
        private SSLContext sslContext;
        private ProxySelector proxy;

        private Builder() {
        }

        public Builder url(String v) { url = v; return this; }
        public Builder token(String v) { token = v; return this; }
        /** Bounds each request until response headers arrive (default 30s). */
        public Builder timeout(Duration v) { timeout = v; return this; }
        public Builder retry(RetryPolicy v) { retry = v; return this; }
        /** Permits plain http to a non-loopback host (the token is sent in clear). */
        public Builder allowInsecureHttp(boolean v) { allowInsecureHttp = v; return this; }
        /** PEM CA bundle trusted in addition to the platform roots. */
        public Builder caCertFile(Path v) { caCertFile = v; return this; }
        public Builder sslContext(SSLContext v) { sslContext = v; return this; }
        public Builder proxy(ProxySelector v) { proxy = v; return this; }

        public Client build() {
            return new Client(this);
        }
    }
}
