// File: ballista/sdks/java/src/test/java/ballista/client/EventsTest.java
package ballista.client;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import ballista.client.exceptions.ConfigurationException;
import ballista.client.exceptions.NotFoundException;
import ballista.client.exceptions.PermissionDeniedException;
import ballista.client.exceptions.ResponseException;
import ballista.client.exceptions.RunTimeoutException;
import ballista.client.exceptions.TransportException;
import ballista.client.models.Run;
import ballista.client.models.RunEvent;
import ballista.client.models.RunState;

import com.sun.net.httpserver.HttpExchange;

import java.io.IOException;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.atomic.AtomicInteger;

import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;

class EventsTest {
    private static final String ID = "r0123456789abcdef01234567";
    private FakeApi api;
    private Client c;

    @BeforeEach
    void start() throws Exception {
        api = new FakeApi();
        c = api.client();
    }

    @AfterEach
    void stop() {
        api.close();
    }

    private static String run(String state) {
        return String.format(ClientTest.RUN, state);
    }

    private static void stream(HttpExchange ex, String body) {
        try {
            ex.getResponseHeaders().set("Content-Type", "text/event-stream");
            byte[] b = body.getBytes(StandardCharsets.UTF_8);
            ex.sendResponseHeaders(200, b.length == 0 ? 0 : b.length);
            try (OutputStream o = ex.getResponseBody()) {
                o.write(b);
            }
        } catch (IOException ignored) {
            // client closed
        }
    }

    private List<RunEvent> collect() {
        List<RunEvent> out = new ArrayList<>();
        c.events(ID, Duration.ofSeconds(2), out::add);
        return out;
    }

    @Test
    void idleStreamTimesOut() {
        api.on("GET .*/events", (ex, m) -> {
            try {
                ex.getResponseHeaders().set("Content-Type", "text/event-stream");
                ex.sendResponseHeaders(200, 0);
                ex.getResponseBody().flush();
                Thread.sleep(3000);
            } catch (IOException | InterruptedException ignored) {
                // closed by the client
            }
        });
        long start = System.nanoTime();
        TransportException e = assertThrows(TransportException.class, () -> c.events(ID, Duration.ofMillis(200), ev -> true));
        assertTrue(e.getMessage().contains("idle timeout"), e.getMessage());
        assertTrue(Duration.ofNanos(System.nanoTime() - start).toMillis() < 2500);
    }

    @Test
    void errorsAndFiltering() {
        String[] mode = {"404"};
        api.on("GET .*/events", (ex, m) -> {
            switch (mode[0]) {
                case "404" -> FakeApi.reply(ex, 404, "{\"error\":{\"code\":\"not_found\",\"message\":\"no run\"}}");
                case "badjson" -> stream(ex, "event: stats\ndata: {broken\n\n");
                case "badrun" -> stream(ex, "event: run\ndata: {\"id\":\"x\",\"state\":\"queued\"}\n\n");
                case "mixed" -> stream(ex, "event: heartbeat\ndata: {}\n\nretry: 100\n\nevent: stats\ndata: {\"users\":1}\n\n"
                        + "event: stats\ndata: {\"users\":2}\n\nevent: end\ndata: " + run("passed") + "\n\n");
                default -> stream(ex, "event: stats\ndata: {\"users\":1}\n\n");
            }
        });
        assertThrows(NotFoundException.class, this::collect);
        mode[0] = "badjson";
        assertThrows(ResponseException.class, this::collect);
        mode[0] = "badrun";
        assertThrows(ResponseException.class, this::collect);
        mode[0] = "mixed";
        List<RunEvent> evs = collect();
        assertEquals(3, evs.size());
        assertEquals(1, evs.getFirst().stats().users());
        assertEquals(RunEvent.Type.END, evs.get(2).type());
        assertEquals(RunState.PASSED, evs.get(2).run().state());
        AtomicInteger seen = new AtomicInteger();
        c.events(ID, Duration.ofSeconds(2), ev -> seen.incrementAndGet() < 1);
        assertEquals(1, seen.get(), "handler returning false stops the stream");
        mode[0] = "closed";
        TransportException e = assertThrows(TransportException.class, this::collect);
        assertTrue(e.getMessage().contains("closed"), e.getMessage());
        assertThrows(ConfigurationException.class, () -> c.events("bad", Duration.ofSeconds(1), ev -> true));
    }

    @Test
    void waitFallsBackToPolling() {
        AtomicInteger polls = new AtomicInteger();
        api.on("GET .*/events", (ex, m) -> stream(ex, ": closes without an end event\n\n"));
        api.on("GET /api/v1/runs/r\\w+", (ex, m) -> FakeApi.reply(ex, 200, run(polls.incrementAndGet() >= 3 ? "failed" : "running")));
        Run r = c.waitFor(ID, WaitOptions.DEFAULT.withPollInterval(Duration.ofMillis(10)));
        assertEquals(RunState.FAILED, r.state());
        assertEquals(3, polls.get());
    }

    @Test
    void waitStopsOnPermanentErrors() {
        int[] status = {404};
        api.on("GET /api/v1/runs/.*", (ex, m) -> FakeApi.reply(ex, status[0], ""));
        assertThrows(NotFoundException.class, () -> c.waitFor(ID, null));
        status[0] = 403;
        assertThrows(PermissionDeniedException.class, () -> c.waitFor(ID, null));
        assertThrows(ConfigurationException.class, () -> c.waitFor("nope", null));
    }

    @Test
    void waitTimeoutBoundsSlowStreams() {
        api.on("GET .*/events", (ex, m) -> {
            try {
                ex.sendResponseHeaders(200, 0);
                Thread.sleep(5000);
            } catch (IOException | InterruptedException ignored) {
                // closed by the client
            }
        });
        api.on("GET /api/v1/runs/r\\w+", (ex, m) -> FakeApi.reply(ex, 200, run("running")));
        long start = System.nanoTime();
        RunTimeoutException e = assertThrows(RunTimeoutException.class,
                () -> c.waitFor(ID, WaitOptions.timeout(Duration.ofMillis(400)).withPollInterval(Duration.ofMillis(20))));
        assertTrue(e.getMessage().contains(ID));
        assertTrue(Duration.ofNanos(System.nanoTime() - start).toMillis() < 3000, "deadline closes a silent stream");
    }
}
