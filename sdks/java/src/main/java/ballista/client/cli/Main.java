// File: ballista/sdks/java/src/main/java/ballista/client/cli/Main.java
package ballista.client.cli;

import ballista.client.Bundle;
import ballista.client.Client;
import ballista.client.CompareOptions;
import ballista.client.RunOptions;
import ballista.client.WaitOptions;
import ballista.client.exceptions.ApiException;
import ballista.client.exceptions.BallistaException;
import ballista.client.exceptions.RunTimeoutException;
import ballista.client.internal.Json;
import ballista.client.models.Comparison;
import ballista.client.models.MetricChange;
import ballista.client.models.Overrides;
import ballista.client.models.RegressionGate;
import ballista.client.models.Run;
import ballista.client.models.Snapshot;
import ballista.client.models.Summary;
import ballista.client.models.ThresholdOutcome;

import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Duration;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Set;
import java.util.function.Consumer;

/** Command line client: submit plans, follow runs, compare and download reports. */
public final class Main {
    private static final int EXIT_ERROR = 1;
    private static final int EXIT_FAILED = 2;
    private static final int EXIT_TIMEOUT = 3;

    private static final String USAGE = """
            usage: ballista-client [--url URL] <command>
              submit <plan.json|plan.toml> [--file REL=PATH]... [--users N] [--spawn-rate N] [--run-time D] [--iterations N]
                     [--tags a,b] [--host URL] [--workers N] [--label k=v]... [--gate PCT] [--baseline ID]
                     [--no-wait] [--timeout SECS] [--quiet] [--report DIR]
              status <run-id> | stop <run-id> | wait <run-id> | list
              compare <run-id> [--baseline ID] [--max-regression-pct PCT]
              report <run-id> [--name report.html] [--out DIR]""";

    private static final Set<String> SWITCHES = Set.of("no-wait", "quiet", "help");
    private static final Set<String> REPEATED = Set.of("file", "label");

    private Main() {
    }

    private record Args(List<String> positional, Map<String, List<String>> options) {
        String one(String k) {
            List<String> v = options.get(k);
            return v == null ? null : v.getLast();
        }

        List<String> all(String k) {
            return options.getOrDefault(k, List.of());
        }

        boolean flag(String k) {
            return options.containsKey(k);
        }
    }

    private static Args parse(String[] argv) {
        List<String> pos = new ArrayList<>();
        Map<String, List<String>> opts = new HashMap<>();
        for (int i = 0; i < argv.length; i++) {
            String a = argv[i];
            if (!a.startsWith("--") || a.length() == 2) {
                pos.add(a);
                continue;
            }
            String key = a.substring(2);
            String value = null;
            int eq = key.indexOf('=');
            if (eq > 0) {
                value = key.substring(eq + 1);
                key = key.substring(0, eq);
            }
            if (SWITCHES.contains(key)) {
                opts.computeIfAbsent(key, k -> new ArrayList<>()).add("true");
                continue;
            }
            if (value == null) {
                if (i + 1 >= argv.length) {
                    throw new BallistaException("--" + key + " requires a value");
                }
                value = argv[++i];
            }
            List<String> list = opts.computeIfAbsent(key, k -> new ArrayList<>());
            if (!REPEATED.contains(key)) {
                list.clear();
            }
            list.add(value);
        }
        return new Args(pos, opts);
    }

    private static double num(String v, String what) {
        try {
            double d = Double.parseDouble(v);
            if (!Double.isFinite(d)) {
                throw new NumberFormatException();
            }
            return d;
        } catch (NumberFormatException e) {
            throw new BallistaException(what + " must be a number");
        }
    }

    private static String fmt(double v, int places) {
        return String.format(Locale.ROOT, "%." + places + "f", v);
    }

    private static void summary(Run run) {
        System.out.println("run " + run.id() + " " + run.state());
        if (run.error() != null) {
            System.out.println("error: " + run.error());
        }
        Summary s = run.summary();
        if (s != null) {
            System.out.println("requests=" + s.requests() + " failures=" + s.failures() + " rps=" + fmt(s.rps(), 1) + " p50=" + fmt(s.p50Ms(), 1)
                    + "ms p95=" + fmt(s.p95Ms(), 1) + "ms p99=" + fmt(s.p99Ms(), 1) + "ms duration=" + fmt(s.durationSecs(), 0) + "s");
            for (ThresholdOutcome t : s.thresholds()) {
                System.out.println("threshold " + t.metric() + " " + t.request() + " actual=" + (t.actual() == null ? "n/a" : fmt(t.actual(), 3))
                        + " " + (t.passed() ? "pass" : "FAIL"));
            }
        }
        if (run.comparison() != null) {
            comparison(run.comparison());
        }
    }

    private static void comparison(Comparison c) {
        System.out.println("baseline=" + (c.baseline() == null ? "-" : c.baseline()) + " passed=" + c.passed() + " regressions=" + c.regressions());
        for (MetricChange ch : c.changes()) {
            String pct = ch.changePct() == null ? "n/a" : String.format(Locale.ROOT, "%+.1f%%", ch.changePct());
            System.out.println("  " + ch.kind() + " " + ch.method() + " " + ch.name() + " " + ch.metric() + " " + fmt(ch.baseline(), 3) + " -> "
                    + fmt(ch.current(), 3) + " " + pct + " " + (ch.regressed() ? "REGRESSED" : "ok"));
        }
        if (c.note() != null) {
            System.out.println("note: " + c.note());
        }
    }

    private static Consumer<Snapshot> progress() {
        long[] last = {-1};
        return s -> {
            long second = (long) s.elapsedSecs();
            if (second == last[0]) {
                return;
            }
            last[0] = second;
            System.err.println("  t=" + second + "s users=" + s.users() + " rps=" + fmt(s.total().rps(), 1) + " p95=" + fmt(s.total().p95Ms(), 1)
                    + "ms failures=" + s.total().failures());
        };
    }

    private static int exitCode(Run run) {
        return switch (run.state()) {
            case PASSED -> 0;
            case FAILED, STOPPED -> EXIT_FAILED;
            default -> EXIT_ERROR;
        };
    }

    private static String need(Args a, String cmd) {
        if (a.positional().size() != 2) {
            throw new BallistaException(cmd + " requires one argument");
        }
        return a.positional().get(1);
    }

    private static Object loadPlan(Path path) throws IOException {
        byte[] raw;
        try (InputStream in = Files.newInputStream(path)) {
            raw = in.readNBytes(Bundle.MAX_PLAN_BYTES + 1);
        }
        if (raw.length > Bundle.MAX_PLAN_BYTES) {
            throw new BallistaException("plan file exceeds 1 MiB");
        }
        String text = new String(raw, StandardCharsets.UTF_8);
        if (!path.toString().toLowerCase(Locale.ROOT).endsWith(".json")) {
            return text;
        }
        Object doc;
        try {
            doc = Json.parse(text);
        } catch (Json.JsonException e) {
            throw new BallistaException("plan document is not valid JSON: " + e.getMessage());
        }
        if (!(doc instanceof Map<?, ?>)) {
            throw new BallistaException("plan document must be a JSON object");
        }
        return doc;
    }

    private static int submit(Client c, Args a) throws IOException {
        Map<String, String> labels = new LinkedHashMap<>();
        for (String l : a.all("label")) {
            int i = l.indexOf('=');
            if (i < 1) {
                throw new BallistaException("label '" + l + "' must be key=value");
            }
            labels.put(l.substring(0, i), l.substring(i + 1));
        }
        Bundle bundle = new Bundle(loadPlan(Path.of(need(a, "submit"))), labels);
        for (String spec : a.all("file")) {
            int i = spec.indexOf('=');
            if (i < 1) {
                throw new BallistaException("--file '" + spec + "' must be REL=PATH");
            }
            bundle.addFile(spec.substring(0, i), Path.of(spec.substring(i + 1)));
        }
        Overrides.Builder o = Overrides.builder();
        if (a.one("users") != null) o.users(num(a.one("users"), "users"));
        if (a.one("spawn-rate") != null) o.spawnRate(num(a.one("spawn-rate"), "spawn-rate"));
        if (a.one("run-time") != null) o.runTime(a.one("run-time"));
        if (a.one("iterations") != null) o.iterations((long) num(a.one("iterations"), "iterations"));
        if (a.one("tags") != null) o.tags(a.one("tags").split(","));
        if (a.one("host") != null) o.host(a.one("host"));
        if (a.one("workers") != null) o.workers((int) num(a.one("workers"), "workers"));
        RunOptions opts = RunOptions.of(o.build()).withLabels(labels);
        if (a.one("gate") != null || a.one("baseline") != null) {
            RegressionGate.Builder g = RegressionGate.builder();
            if (a.one("gate") != null) g.maxRegressionPct(num(a.one("gate"), "gate"));
            if (a.one("baseline") != null) g.baseline(a.one("baseline"));
            opts = opts.withRegression(g.build());
        }
        var info = c.uploadBundle(bundle);
        Run run = c.createRun(info, opts);
        System.out.println("queued run " + run.id() + " (bundle " + info.id() + ")");
        if (a.flag("no-wait")) {
            return 0;
        }
        WaitOptions w = WaitOptions.DEFAULT;
        if (a.one("timeout") != null) {
            w = w.withTimeout(Duration.ofMillis((long) (num(a.one("timeout"), "timeout") * 1000)));
        }
        if (!a.flag("quiet")) {
            w = w.withOnStats(progress());
        }
        run = c.waitFor(run.id(), w);
        summary(run);
        if (a.one("report") != null && run.summary() != null) {
            Path dir = Files.createDirectories(Path.of(a.one("report")));
            c.download(run.id(), "report.json", dir);
            c.download(run.id(), "report.html", dir);
        }
        return exitCode(run);
    }

    static int run(String[] argv) throws IOException {
        return run(argv, System.getenv());
    }

    /** Runs a command; env supplies BALLISTA_URL and BALLISTA_TOKEN. */
    static int run(String[] argv, Map<String, String> env) throws IOException {
        Args a = parse(argv);
        if (a.flag("help") || a.positional().isEmpty()) {
            System.out.println(USAGE);
            return a.flag("help") ? 0 : EXIT_ERROR;
        }
        Client c = Client.builder().url(a.one("url") != null ? a.one("url") : env.get(Client.URL_ENV)).token(env.get(Client.TOKEN_ENV)).build();
        String cmd = a.positional().getFirst();
        return switch (cmd) {
            case "submit" -> submit(c, a);
            case "status" -> {
                summary(c.getRun(need(a, cmd)));
                yield 0;
            }
            case "stop" -> {
                summary(c.stopRun(need(a, cmd)));
                yield 0;
            }
            case "wait" -> {
                Run r = c.waitFor(need(a, cmd), WaitOptions.DEFAULT);
                summary(r);
                yield exitCode(r);
            }
            case "compare" -> {
                Double pct = a.one("max-regression-pct") == null ? null : num(a.one("max-regression-pct"), "max-regression-pct");
                Comparison cmp = c.compare(need(a, cmd), new CompareOptions(a.one("baseline"), pct, null));
                comparison(cmp);
                yield cmp.passed() ? 0 : EXIT_FAILED;
            }
            case "report" -> {
                String name = a.one("name") == null ? "report.html" : a.one("name");
                if (!Client.ARTIFACTS.contains(name)) {
                    throw new BallistaException("name must be one of " + String.join(", ", Client.ARTIFACTS));
                }
                System.out.println(c.download(need(a, cmd), name, Path.of(a.one("out") == null ? "." : a.one("out"))));
                yield 0;
            }
            case "list" -> {
                for (Run r : c.listRuns(null, 50, null).items()) {
                    Map<String, Object> row = new LinkedHashMap<>();
                    row.put("id", r.id());
                    row.put("state", r.state().wire());
                    row.put("plan", r.planName());
                    row.put("created_at", r.createdAt());
                    System.out.println(Json.write(row));
                }
                yield 0;
            }
            default -> {
                System.err.println(USAGE);
                yield EXIT_ERROR;
            }
        };
    }

    public static void main(String[] argv) {
        int code;
        try {
            code = run(argv);
        } catch (RunTimeoutException e) {
            System.err.println("error: " + e.getMessage());
            code = EXIT_TIMEOUT;
        } catch (ApiException e) {
            System.err.println("error: " + e.status() + " " + e.code() + ": " + e.detail());
            code = EXIT_ERROR;
        } catch (BallistaException | IOException e) {
            System.err.println("error: " + e.getMessage());
            code = EXIT_ERROR;
        }
        System.exit(code);
    }
}
