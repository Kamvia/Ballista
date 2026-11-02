// File: ballista/sdks/go/fake_test.go
package ballista

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strconv"
	"sync"
	"testing"
	"time"
)

// fakeAPI is an in-memory run manager used by the unit tests.
type fakeAPI struct {
	t      *testing.T
	srv    *httptest.Server
	mux    *http.ServeMux
	mu     sync.Mutex
	runs   []map[string]any
	bundle map[string]map[string]any
	keys   map[string]string
	// last request bodies, headers and queries by route label
	bodies  map[string]map[string]any
	headers map[string]http.Header
	queries map[string]url.Values
	calls   map[string]int
	final   RunState
}

func newFake(t *testing.T) *fakeAPI {
	t.Helper()
	f := &fakeAPI{
		t: t, mux: http.NewServeMux(), bundle: map[string]map[string]any{}, keys: map[string]string{},
		bodies: map[string]map[string]any{}, headers: map[string]http.Header{}, queries: map[string]url.Values{},
		calls: map[string]int{}, final: StatePassed,
	}
	f.routes()
	f.srv = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("Authorization") != "Bearer "+testToken {
			writeJSON(w, http.StatusUnauthorized, map[string]any{"error": map[string]string{"code": "unauthorized", "message": "invalid token"}})
			return
		}
		f.mux.ServeHTTP(w, r)
	}))
	t.Cleanup(f.srv.Close)
	return f
}

func (f *fakeAPI) client(t *testing.T) *Client {
	t.Helper()
	c, err := New(Options{URL: f.srv.URL, Token: testToken, Retry: &RetryPolicy{Attempts: 3, BaseDelay: time.Millisecond, MaxDelay: 5 * time.Millisecond}})
	if err != nil {
		t.Fatal(err)
	}
	return c
}

// handle replaces or adds a route.
func (f *fakeAPI) handle(pattern string, h http.HandlerFunc) {
	mux := http.NewServeMux()
	mux.HandleFunc(pattern, h)
	prev := f.mux
	f.mux = http.NewServeMux()
	f.mux.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		if _, p := mux.Handler(r); p != "" {
			mux.ServeHTTP(w, r)
			return
		}
		prev.ServeHTTP(w, r)
	})
}

func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(v)
}

func (f *fakeAPI) record(label string, r *http.Request) map[string]any {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.calls[label]++
	f.headers[label] = r.Header.Clone()
	f.queries[label] = r.URL.Query()
	var body map[string]any
	if raw, _ := io.ReadAll(r.Body); len(raw) > 0 {
		if err := json.Unmarshal(raw, &body); err != nil {
			f.t.Errorf("%s: body is not JSON: %v", label, err)
		}
	}
	f.bodies[label] = body
	return body
}

func (f *fakeAPI) newRun(bundle string, body map[string]any) map[string]any {
	run := map[string]any{
		"id": fmt.Sprintf("r%024x", len(f.runs)+1), "seq": len(f.runs) + 1, "bundle": bundle, "plan_name": "p",
		"state": "queued", "created_at": time.Now().UTC().Format(time.RFC3339), "created_by": "ci",
		"labels": body["labels"], "overrides": body["overrides"], "queue_position": 0,
	}
	if g, ok := body["regression"]; ok {
		run["regression"] = g
	}
	f.runs = append(f.runs, run)
	return run
}

func (f *fakeAPI) findRun(id string) map[string]any {
	for _, r := range f.runs {
		if r["id"] == id {
			return r
		}
	}
	return nil
}

func notFound(w http.ResponseWriter) {
	writeJSON(w, http.StatusNotFound, map[string]any{"error": map[string]string{"code": "not_found", "message": "no such resource"}})
}

func (f *fakeAPI) routes() {
	f.mux.HandleFunc("GET /api/v1/health", func(w http.ResponseWriter, r *http.Request) {
		f.record("health", r)
		writeJSON(w, 200, map[string]string{"status": "ok", "version": "0.1.0"})
	})
	f.mux.HandleFunc("GET /api/v1/whoami", func(w http.ResponseWriter, r *http.Request) {
		writeJSON(w, 200, map[string]string{"name": "ci", "scope": "run"})
	})
	f.mux.HandleFunc("GET /api/v1/status", func(w http.ResponseWriter, r *http.Request) {
		writeJSON(w, 200, map[string]any{"version": "0.1.0", "active_run": nil, "queued": 1,
			"limits":  map[string]any{"max_users": 5000, "max_run_time_secs": 3600, "max_queue": 50},
			"workers": []map[string]any{{"id": 1, "name": "pool-a", "address": "10.0.0.2", "cores": 8, "state": "ready", "cpu_pct": nil}}})
	})
	f.mux.HandleFunc("POST /api/v1/plans/validate", func(w http.ResponseWriter, r *http.Request) {
		body := f.record("validate", r)
		name := "text"
		if doc, ok := body["plan"].(map[string]any); ok {
			name, _ = doc["name"].(string)
		}
		writeJSON(w, 200, map[string]any{"name": name, "scenarios": []string{"s"}, "files": nil, "document": map[string]any{"name": name}})
	})
	f.mux.HandleFunc("POST /api/v1/bundles", func(w http.ResponseWriter, r *http.Request) {
		body := f.record("upload", r)
		raw, _ := json.Marshal(body["plan"])
		sum := sha256.Sum256(raw)
		id := hex.EncodeToString(sum[:16])
		f.mu.Lock()
		f.bundle[id] = body
		f.mu.Unlock()
		writeJSON(w, 201, bundleJSON(id))
	})
	f.mux.HandleFunc("GET /api/v1/bundles", func(w http.ResponseWriter, r *http.Request) {
		f.mu.Lock()
		defer f.mu.Unlock()
		items := []any{}
		for id := range f.bundle {
			items = append(items, bundleJSON(id))
		}
		writeJSON(w, 200, map[string]any{"items": items, "next_cursor": nil})
	})
	f.mux.HandleFunc("GET /api/v1/bundles/{id}", func(w http.ResponseWriter, r *http.Request) {
		f.mu.Lock()
		_, ok := f.bundle[r.PathValue("id")]
		f.mu.Unlock()
		if !ok {
			notFound(w)
			return
		}
		writeJSON(w, 200, bundleJSON(r.PathValue("id")))
	})
	f.mux.HandleFunc("DELETE /api/v1/bundles/{id}", func(w http.ResponseWriter, r *http.Request) {
		f.mu.Lock()
		defer f.mu.Unlock()
		if _, ok := f.bundle[r.PathValue("id")]; !ok {
			notFound(w)
			return
		}
		delete(f.bundle, r.PathValue("id"))
		w.WriteHeader(http.StatusNoContent)
	})
	f.mux.HandleFunc("POST /api/v1/runs", func(w http.ResponseWriter, r *http.Request) {
		body := f.record("create", r)
		f.mu.Lock()
		defer f.mu.Unlock()
		key := r.Header.Get("Idempotency-Key")
		if id, ok := f.keys[key]; ok {
			writeJSON(w, 200, f.findRun(id))
			return
		}
		bundle, _ := body["bundle"].(string)
		if _, ok := f.bundle[bundle]; !ok {
			notFound(w)
			return
		}
		run := f.newRun(bundle, body)
		f.keys[key] = run["id"].(string)
		writeJSON(w, 202, run)
	})
	f.mux.HandleFunc("GET /api/v1/runs", func(w http.ResponseWriter, r *http.Request) {
		f.record("list", r)
		f.mu.Lock()
		defer f.mu.Unlock()
		limit, _ := strconv.Atoi(r.URL.Query().Get("limit"))
		start, _ := strconv.Atoi(r.URL.Query().Get("cursor"))
		var match []map[string]any
		for i := len(f.runs) - 1; i >= 0; i-- {
			if s := r.URL.Query().Get("state"); s == "" || f.runs[i]["state"] == s {
				match = append(match, f.runs[i])
			}
		}
		end := min(start+limit, len(match))
		var next any
		if end < len(match) {
			next = strconv.Itoa(end)
		}
		writeJSON(w, 200, map[string]any{"items": match[min(start, len(match)):end], "next_cursor": next})
	})
	f.mux.HandleFunc("GET /api/v1/runs/{id}", func(w http.ResponseWriter, r *http.Request) {
		f.mu.Lock()
		defer f.mu.Unlock()
		run := f.findRun(r.PathValue("id"))
		if run == nil {
			notFound(w)
			return
		}
		writeJSON(w, 200, run)
	})
	f.mux.HandleFunc("POST /api/v1/runs/{id}/stop", func(w http.ResponseWriter, r *http.Request) {
		f.mu.Lock()
		defer f.mu.Unlock()
		run := f.findRun(r.PathValue("id"))
		if run == nil {
			notFound(w)
			return
		}
		run["state"] = "cancelled"
		run["stopped_by"] = "ci"
		writeJSON(w, 200, run)
	})
	f.mux.HandleFunc("DELETE /api/v1/runs/{id}", func(w http.ResponseWriter, r *http.Request) {
		f.record("delete-run", r)
		w.WriteHeader(http.StatusNoContent)
	})
	f.mux.HandleFunc("GET /api/v1/runs/{id}/stats", func(w http.ResponseWriter, r *http.Request) {
		writeJSON(w, 200, map[string]any{
			"plan": "p", "state": "finished", "elapsed_secs": 5.2, "users": 4, "total": map[string]any{"name": "Aggregated", "count": 40, "p95_ms": 9.5},
			"rows":         []map[string]any{{"method": "GET", "name": "item", "count": 30}, {"method": "SSE", "name": "item", "count": 3}},
			"transactions": []map[string]any{{"method": "TXN", "name": "purchase", "count": 10}},
			"distribution": []map[string]any{{"upper_ms": 10, "count": 30}, {"upper_ms": nil, "count": 1}},
		})
	})
	f.mux.HandleFunc("GET /api/v1/runs/{id}/compare", func(w http.ResponseWriter, r *http.Request) {
		f.record("compare", r)
		writeJSON(w, 200, map[string]any{"baseline": "r000000000000000000000001", "passed": false, "regressions": 1, "unmatched": []string{"GET /gone"}, "note": nil,
			"changes": []map[string]any{{"kind": "request", "method": "GET", "name": "item", "metric": "p95", "baseline": 5.0, "current": 9.0, "change_pct": 80.0, "regressed": true}}})
	})
	f.mux.HandleFunc("GET /api/v1/runs/{id}/artifacts/{name}", func(w http.ResponseWriter, r *http.Request) {
		switch r.PathValue("name") {
		case "report.json":
			writeJSON(w, 200, map[string]any{"plan": "p", "total": map[string]any{"count": 40}})
		default:
			_, _ = io.WriteString(w, "<!DOCTYPE html><title>report</title>")
		}
	})
	f.mux.HandleFunc("GET /api/v1/runs/{id}/events", func(w http.ResponseWriter, r *http.Request) {
		f.mu.Lock()
		run := f.findRun(r.PathValue("id"))
		if run != nil {
			run["state"] = string(f.final)
			run["summary"] = map[string]any{"requests": 40, "failures": 0, "thresholds": []any{}}
		}
		f.mu.Unlock()
		if run == nil {
			notFound(w)
			return
		}
		w.Header().Set("Content-Type", "text/event-stream")
		data, _ := json.Marshal(run)
		fmt.Fprintf(w, "event: stats\ndata: {\"elapsed_secs\":1,\"users\":2,\"total\":{\"count\":5}}\n\n")
		fmt.Fprintf(w, "event: end\ndata: %s\n\n", data)
	})
}

func bundleJSON(id string) map[string]any {
	return map[string]any{"id": id, "plan_name": "p", "scenarios": []string{"s"}, "files": []any{}, "size": 10,
		"labels": map[string]string{}, "created_at": "2026-11-02T00:00:00Z", "created_by": "ci"}
}
