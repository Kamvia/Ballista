// File: ballista/sdks/java/src/test/java/ballista/client/SseParserTest.java
package ballista.client;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import ballista.client.internal.SseParser;

import java.io.ByteArrayInputStream;
import java.io.FilterInputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.atomic.AtomicInteger;

import org.junit.jupiter.api.Test;

class SseParserTest {
    /** Delivers one byte per read, like a slow network. */
    private static InputStream trickle(String s) {
        return new FilterInputStream(new ByteArrayInputStream(s.getBytes(StandardCharsets.UTF_8))) {
            @Override
            public int read(byte[] b, int off, int len) throws IOException {
                return super.read(b, off, Math.min(len, 1));
            }
        };
    }

    private static List<SseParser.Message> all(InputStream in, Runnable activity) throws IOException {
        SseParser p = new SseParser(in, activity);
        List<SseParser.Message> out = new ArrayList<>();
        for (SseParser.Message m; (m = p.next()) != null; ) {
            out.add(m);
        }
        return out;
    }

    @Test
    void parsesFieldsAcrossFragmentsAndLineEndings() throws IOException {
        String stream = ": comment\n"
                + "event: run\r\ndata: {\"a\":1}\r\n\r\n"
                + "data: line one\ndata:line two\nid: 7\n\n"
                + "event: dropped-without-data\n\n"
                + "id: bad\u0000id\ndata: x\n\n"
                + "data\rdata: cr\r\r"
                + "data: café €\n\n"
                + "data: trailing message without a blank line";
        AtomicInteger reads = new AtomicInteger();
        List<SseParser.Message> msgs = all(trickle(stream), reads::incrementAndGet);
        assertEquals(List.of(
                new SseParser.Message("run", "{\"a\":1}", null),
                new SseParser.Message("message", "line one\nline two", "7"),
                new SseParser.Message("message", "x", null),
                new SseParser.Message("message", "\ncr", null),
                new SseParser.Message("message", "café €", null)), msgs);
        assertTrue(reads.get() > 100, "activity reported for every read");
    }

    @Test
    void eventNameResetsBetweenMessages() throws IOException {
        List<SseParser.Message> msgs = all(new ByteArrayInputStream("event: stats\ndata: 1\n\ndata: 2\n\n".getBytes(StandardCharsets.UTF_8)), () -> { });
        assertEquals("message", msgs.get(1).event());
    }

    @Test
    void enforcesLimits() {
        String longLine = "data: " + "x".repeat((16 << 20) + 1) + "\n\n";
        IOException e = assertThrows(IOException.class, () -> all(new ByteArrayInputStream(longLine.getBytes(StandardCharsets.US_ASCII)), () -> { }));
        assertTrue(e.getMessage().contains("too long"));
        StringBuilder big = new StringBuilder();
        String chunk = "data: " + "y".repeat(1 << 20) + "\n";
        for (int i = 0; i < 17; i++) {
            big.append(chunk);
        }
        big.append('\n');
        IOException e2 = assertThrows(IOException.class, () -> all(new ByteArrayInputStream(big.toString().getBytes(StandardCharsets.US_ASCII)), () -> { }));
        assertTrue(e2.getMessage().contains("too large"));
    }
}
