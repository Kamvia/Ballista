// File: ballista/sdks/java/src/main/java/ballista/client/Bundle.java
package ballista.client;

import ballista.client.internal.Rules;
import ballista.client.plan.Plan;

import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Base64;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.TreeMap;

/**
 * A plan with optional assets (descriptor sets, certificates, upload files) sent as content.
 * Inline scripts and inline feeder rows need no files.
 */
public final class Bundle {
    public static final int MAX_FILES = 256;
    public static final int MAX_FILE_BYTES = 16 << 20;
    public static final long MAX_TOTAL_BYTES = 64L << 20;
    public static final int MAX_PLAN_BYTES = 1 << 20;

    private final Object plan;
    private final Map<String, String> labels;
    private final Map<String, byte[]> files = new TreeMap<>();

    public Bundle(Object plan) {
        this(plan, Map.of());
    }

    /** plan is a {@link Plan}, a plan document map or plan file text. */
    public Bundle(Object plan, Map<String, String> labels) {
        this.plan = payload(plan);
        this.labels = Rules.labels(labels);
    }

    static Object payload(Object plan) {
        return switch (plan) {
            case Plan p -> p.toDocument();
            case Map<?, ?> m when !m.isEmpty() -> new LinkedHashMap<>(m);
            case String text when !text.isBlank() && text.getBytes(StandardCharsets.UTF_8).length <= MAX_PLAN_BYTES -> text;
            case String text -> throw Rules.fail("plan text must be non-empty and at most 1 MiB");
            case null, default -> throw Rules.fail("plan must be a Plan, a non-empty document map or plan text");
        };
    }

    public synchronized Bundle addBytes(String rel, byte[] data) {
        if (!Rules.bundlePath(rel)) {
            throw Rules.fail("invalid bundle path '" + rel + "'");
        }
        if (data.length > MAX_FILE_BYTES) {
            throw Rules.fail("'" + rel + "' exceeds 16 MiB");
        }
        if (!files.containsKey(rel) && files.size() >= MAX_FILES) {
            throw Rules.fail("a bundle holds at most " + MAX_FILES + " files");
        }
        long total = data.length;
        for (Map.Entry<String, byte[]> e : files.entrySet()) {
            if (!e.getKey().equals(rel)) {
                total += e.getValue().length;
            }
        }
        if (total > MAX_TOTAL_BYTES) {
            throw Rules.fail("bundle exceeds 64 MiB");
        }
        files.put(rel, data.clone());
        return this;
    }

    public Bundle addText(String rel, String text) {
        return addBytes(rel, text.getBytes(StandardCharsets.UTF_8));
    }

    /** Adds the content of a local regular file under the bundle path rel. */
    public Bundle addFile(String rel, Path path) {
        if (!Files.isRegularFile(path)) {
            throw Rules.fail("'" + rel + "': " + path + " is not a regular file");
        }
        try (InputStream in = Files.newInputStream(path)) {
            byte[] data = in.readNBytes(MAX_FILE_BYTES + 1);
            if (data.length > MAX_FILE_BYTES) {
                throw Rules.fail("'" + rel + "' exceeds 16 MiB");
            }
            return addBytes(rel, data);
        } catch (IOException e) {
            throw Rules.fail("'" + rel + "': " + e.getMessage());
        }
    }

    /** Bundle path to size in bytes. */
    public synchronized Map<String, Integer> files() {
        Map<String, Integer> out = new TreeMap<>();
        files.forEach((k, v) -> out.put(k, v.length));
        return out;
    }

    synchronized Map<String, Object> toPayload() {
        Map<String, String> encoded = new LinkedHashMap<>();
        Base64.Encoder enc = Base64.getEncoder();
        files.forEach((k, v) -> encoded.put(k, enc.encodeToString(v)));
        Map<String, Object> m = new LinkedHashMap<>();
        m.put("plan", plan);
        m.put("files", encoded);
        m.put("labels", labels);
        return m;
    }

    @Override
    public synchronized String toString() {
        return "Bundle(plan=" + (plan instanceof String ? "text" : "document") + ", files=" + files.size() + ")";
    }
}
