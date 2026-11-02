// File: ballista/sdks/java/src/main/java/ballista/client/internal/SseParser.java
package ballista.client.internal;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;

/** Incremental text/event-stream parser with a bounded line length. */
public final class SseParser {
    private static final int MAX_LINE = 16 << 20;

    /** One dispatched message. */
    public record Message(String event, String data, String id) {
    }

    private final InputStream in;
    private final Runnable onActivity;
    private final byte[] buf = new byte[64 << 10];
    private int len;
    private int off;
    private boolean pendingCr;

    public SseParser(InputStream in, Runnable onActivity) {
        this.in = in;
        this.onActivity = onActivity;
    }

    private int read() throws IOException {
        if (off == len) {
            len = in.read(buf);
            off = 0;
            if (len <= 0) {
                len = 0;
                return -1;
            }
            onActivity.run();
        }
        return buf[off++] & 0xff;
    }

    private String line() throws IOException {
        ByteArrayOutputStream out = new ByteArrayOutputStream(128);
        while (true) {
            int b = read();
            if (b == -1) {
                return out.size() > 0 ? out.toString(StandardCharsets.UTF_8) : null;
            }
            if (pendingCr) {
                pendingCr = false;
                if (b == '\n') {
                    continue;
                }
            }
            if (b == '\n') {
                return out.toString(StandardCharsets.UTF_8);
            }
            if (b == '\r') {
                pendingCr = true;
                return out.toString(StandardCharsets.UTF_8);
            }
            if (out.size() >= MAX_LINE) {
                throw new IOException("event stream line too long");
            }
            out.write(b);
        }
    }

    /** Returns the next message, or null when the stream ends. */
    public Message next() throws IOException {
        String event = "message";
        String id = null;
        List<String> data = new ArrayList<>();
        long size = 0;
        while (true) {
            String line = line();
            if (line == null) {
                return null;
            }
            if (line.isEmpty()) {
                if (!data.isEmpty()) {
                    return new Message(event, String.join("\n", data), id);
                }
                event = "message";
                continue;
            }
            if (line.startsWith(":")) {
                continue;
            }
            int colon = line.indexOf(':');
            String field = colon < 0 ? line : line.substring(0, colon);
            String value = colon < 0 ? "" : line.substring(colon + 1);
            if (value.startsWith(" ")) {
                value = value.substring(1);
            }
            switch (field) {
                case "event" -> event = value;
                case "data" -> {
                    size += value.length();
                    if (size > MAX_LINE) {
                        throw new IOException("event stream message too large");
                    }
                    data.add(value);
                }
                case "id" -> {
                    if (value.indexOf('\0') < 0) {
                        id = value;
                    }
                }
                default -> {
                }
            }
        }
    }
}
