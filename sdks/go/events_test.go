// File: ballista/sdks/go/events_test.go
package ballista

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

const testRunID = "r0123456789abcdef01234567"

func TestEventsIdleTimeout(t *testing.T) {
	c := testClient(t, func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		w.WriteHeader(200)
		w.(http.Flusher).Flush()
		select {
		case <-r.Context().Done():
		case <-time.After(3 * time.Second):
		}
	})
	start := time.Now()
	var got error
	for _, err := range c.Events(context.Background(), testRunID, 100*time.Millisecond) {
		got = err
	}
	var te *TransportError
	if !errors.As(got, &te) || !strings.Contains(got.Error(), "idle timeout") || time.Since(start) > 2*time.Second {
		t.Fatalf("idle: %v after %v", got, time.Since(start))
	}
}

func TestEventsErrorsAndFiltering(t *testing.T) {
	var mode atomic.Value
	mode.Store("404")
	c := testClient(t, func(w http.ResponseWriter, r *http.Request) {
		switch mode.Load() {
		case "404":
			w.WriteHeader(404)
			_, _ = io.WriteString(w, `{"error":{"code":"not_found","message":"no run"}}`)
		case "badjson":
			_, _ = io.WriteString(w, "event: stats\ndata: {broken\n\n")
		case "badrun":
			_, _ = io.WriteString(w, "event: run\ndata: {\"id\":\"x\",\"state\":\"queued\"}\n\n")
		case "mixed":
			_, _ = io.WriteString(w, "event: heartbeat\ndata: {}\n\nretry: 100\n\n")
			fmt.Fprintf(w, "event: stats\ndata: {\"users\":1}\n\nevent: stats\ndata: {\"users\":2}\n\nevent: end\ndata: "+runJSON+"\n\n", "passed")
		case "closed":
			_, _ = io.WriteString(w, "event: stats\ndata: {\"users\":1}\n\n")
		}
	})
	collect := func() ([]Event, error) {
		var evs []Event
		for ev, err := range c.Events(context.Background(), testRunID, time.Second) {
			if err != nil {
				return evs, err
			}
			evs = append(evs, ev)
		}
		return evs, nil
	}
	if _, err := collect(); !IsNotFound(err) {
		t.Fatalf("404: %v", err)
	}
	var re *ResponseError
	for _, m := range []string{"badjson", "badrun"} {
		mode.Store(m)
		if _, err := collect(); !errors.As(err, &re) {
			t.Fatalf("%s: %v", m, err)
		}
	}
	mode.Store("mixed")
	evs, err := collect()
	if err != nil || len(evs) != 3 || evs[0].Stats.Users != 1 || evs[2].Type != EventEnd || evs[2].Run.State != StatePassed {
		t.Fatalf("mixed: %v %v", evs, err)
	}
	n := 0
	for range c.Events(context.Background(), testRunID, time.Second) {
		n++
		break
	}
	if n != 1 {
		t.Fatal("consumer break")
	}
	mode.Store("closed")
	if evs, err := collect(); len(evs) != 1 || err == nil || !strings.Contains(err.Error(), "closed") {
		t.Fatalf("closed early: %v %v", evs, err)
	}
	for _, err := range c.Events(context.Background(), "bad", time.Second) {
		var ce *ConfigurationError
		if !errors.As(err, &ce) {
			t.Fatalf("bad id: %v", err)
		}
	}
}

func TestWaitFallsBackToPolling(t *testing.T) {
	var polls atomic.Int32
	c := testClient(t, func(w http.ResponseWriter, r *http.Request) {
		if strings.HasSuffix(r.URL.Path, "/events") {
			_, _ = io.WriteString(w, ": stream closes without an end event\n\n")
			return
		}
		state := "running"
		if polls.Add(1) >= 3 {
			state = "failed"
		}
		fmt.Fprintf(w, runJSON, state)
	})
	r, err := c.Wait(context.Background(), testRunID, WaitOptions{PollInterval: 10 * time.Millisecond})
	if err != nil || r.State != StateFailed || polls.Load() != 3 {
		t.Fatalf("polling: %v %v polls=%d", r, err, polls.Load())
	}
}

func TestWaitStopsOnPermanentErrorsAndCancellation(t *testing.T) {
	status := http.StatusNotFound
	c := testClient(t, func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(status)
	})
	if _, err := c.Wait(context.Background(), testRunID, WaitOptions{}); !IsNotFound(err) {
		t.Fatalf("404: %v", err)
	}
	status = http.StatusForbidden
	if _, err := c.Wait(context.Background(), testRunID, WaitOptions{}); !IsForbidden(err) {
		t.Fatalf("403: %v", err)
	}
	if _, err := c.Wait(context.Background(), "nope", WaitOptions{}); err == nil {
		t.Fatal("bad id")
	}
	status = http.StatusServiceUnavailable
	ctx, cancel := context.WithTimeout(context.Background(), 100*time.Millisecond)
	defer cancel()
	_, err := c.Wait(ctx, testRunID, WaitOptions{PollInterval: 10 * time.Millisecond})
	var rt *RunTimeoutError
	if err == nil || errors.As(err, &rt) {
		t.Fatalf("caller cancellation must not look like a wait timeout: %v", err)
	}
}

func TestRunTimeoutErrorMessage(t *testing.T) {
	e := &RunTimeoutError{RunID: testRunID, Timeout: 90 * time.Second}
	if e.Error() != "run "+testRunID+" did not finish within 1m30s" {
		t.Fatal(e.Error())
	}
}
