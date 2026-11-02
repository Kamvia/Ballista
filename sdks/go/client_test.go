// File: ballista/sdks/go/client_test.go
package ballista

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

const testToken = "bt_0123456789abcdef0123456789abcdef"

func testClient(t *testing.T, h http.HandlerFunc) *Client {
	t.Helper()
	srv := httptest.NewServer(h)
	t.Cleanup(srv.Close)
	c, err := New(Options{URL: srv.URL, Token: testToken, Retry: &RetryPolicy{Attempts: 3, BaseDelay: time.Millisecond, MaxDelay: 5 * time.Millisecond}})
	if err != nil {
		t.Fatal(err)
	}
	return c
}

const runJSON = `{"id":"r0123456789abcdef01234567","seq":1,"bundle":"0123456789abcdef0123456789abcdef","plan_name":"p","state":"%s","created_at":"t","created_by":"ci","labels":{},"overrides":{}}`

func TestNewRejectsUnsafeConfiguration(t *testing.T) {
	cases := []Options{
		{URL: "https://example.com", Token: "short"},
		{URL: "http://example.com", Token: testToken},
		{URL: "https://user:pw@example.com", Token: testToken},
		{URL: "ftp://example.com", Token: testToken},
		{URL: "https://example.com", Token: testToken + " x"},
	}
	for _, o := range cases {
		if _, err := New(o); err == nil {
			t.Errorf("accepted %+v", o.URL)
		}
	}
	c, err := New(Options{URL: "http://127.0.0.1:8070/ignored", Token: testToken})
	if err != nil || c.BaseURL() != "http://127.0.0.1:8070/api/v1" {
		t.Fatalf("loopback http: %v %v", c, err)
	}
	if s := fmt.Sprintf("%v %+v %#v", c, c, c); strings.Contains(s, testToken) {
		t.Fatal("token leaked through formatting")
	}
}

func TestRetriesAndErrors(t *testing.T) {
	var calls atomic.Int32
	c := testClient(t, func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("Authorization") != "Bearer "+testToken {
			t.Error("missing token")
		}
		switch r.URL.Path {
		case "/api/v1/health":
			if calls.Add(1) < 3 {
				w.WriteHeader(http.StatusServiceUnavailable)
				return
			}
			_, _ = io.WriteString(w, `{"status":"ok","version":"1"}`)
		case "/api/v1/whoami":
			w.WriteHeader(http.StatusForbidden)
			_, _ = io.WriteString(w, `{"error":{"code":"forbidden","message":"scope too low"}}`)
		case "/api/v1/status":
			http.Redirect(w, r, "https://evil.example/steal", http.StatusFound)
		}
	})
	ctx := context.Background()
	h, err := c.Health(ctx)
	if err != nil || h.Status != "ok" || calls.Load() != 3 {
		t.Fatalf("health after retries: %v %v %d", h, err, calls.Load())
	}
	_, err = c.WhoAmI(ctx)
	var api *APIError
	if !errors.As(err, &api) || !IsForbidden(err) || api.Code != "forbidden" || api.Detail != "scope too low" {
		t.Fatalf("forbidden: %v", err)
	}
	if _, err := c.Status(ctx); !errors.As(err, &api) || api.Status != http.StatusFound {
		t.Fatalf("redirect must not be followed: %v", err)
	}
}

func TestCreateRunSendsKeyAndGate(t *testing.T) {
	var keys []string
	c := testClient(t, func(w http.ResponseWriter, r *http.Request) {
		keys = append(keys, r.Header.Get("Idempotency-Key"))
		var body map[string]any
		_ = json.NewDecoder(r.Body).Decode(&body)
		gate, _ := body["regression"].(map[string]any)
		if gate["max_regression_pct"] != 15.0 || body["overrides"].(map[string]any)["workers"] != 2.0 {
			t.Errorf("body: %v", body)
		}
		w.WriteHeader(http.StatusAccepted)
		fmt.Fprintf(w, runJSON, "queued")
	})
	opts := CreateRunOptions{Overrides: &Overrides{Workers: Ptr(2)}, Regression: &RegressionGate{MaxRegressionPct: Ptr(15.0)}}
	r, err := c.CreateRun(context.Background(), strings.Repeat("0", 32), opts)
	if err != nil || r.State != StateQueued || len(keys[0]) != 36 {
		t.Fatalf("create: %v %v %v", r, err, keys)
	}
	bad := []CreateRunOptions{
		{Overrides: &Overrides{Users: Ptr(-1.0)}},
		{Overrides: &Overrides{Workers: Ptr(513)}},
		{Overrides: &Overrides{RunTime: "soon"}},
		{Overrides: &Overrides{Host: "http://u:p@x"}},
		{Labels: map[string]string{"bad key": "v"}},
		{Regression: &RegressionGate{Baseline: "../etc"}},
		{Regression: &RegressionGate{Metrics: []CompareMetric{"p42"}}},
		{IdempotencyKey: "has space"},
	}
	for i, o := range bad {
		var ce *ConfigurationError
		if _, err := c.CreateRun(context.Background(), strings.Repeat("0", 32), o); !errors.As(err, &ce) {
			t.Errorf("case %d accepted: %v", i, err)
		}
	}
	if _, err := c.GetRun(context.Background(), "../runs"); err == nil {
		t.Fatal("path traversal id accepted")
	}
}

func TestWaitFollowsEvents(t *testing.T) {
	c := testClient(t, func(w http.ResponseWriter, r *http.Request) {
		if !strings.HasSuffix(r.URL.Path, "/events") {
			fmt.Fprintf(w, runJSON, "passed")
			return
		}
		w.Header().Set("Content-Type", "text/event-stream")
		fmt.Fprintf(w, ": keepalive\n\nevent: run\ndata: "+runJSON+"\n\n", "running")
		_, _ = io.WriteString(w, "event: stats\ndata: {\"elapsed_secs\":1.5,\"users\":3,\n")
		_, _ = io.WriteString(w, "data: \"total\":{\"count\":9}}\r\n\r\n")
		fmt.Fprintf(w, "event: end\ndata: "+runJSON+"\n\n", "passed")
	})
	var states []RunState
	var stats []uint64
	r, err := c.Wait(context.Background(), "r0123456789abcdef01234567", WaitOptions{
		OnState: func(r *Run) { states = append(states, r.State) },
		OnStats: func(s *Snapshot) { stats = append(stats, s.Total.Count) },
	})
	if err != nil || r.State != StatePassed || len(states) != 2 || len(stats) != 1 || stats[0] != 9 {
		t.Fatalf("wait: %v %v %v %v", r, err, states, stats)
	}
}

func TestWaitTimesOut(t *testing.T) {
	c := testClient(t, func(w http.ResponseWriter, r *http.Request) {
		if strings.HasSuffix(r.URL.Path, "/events") {
			w.WriteHeader(http.StatusServiceUnavailable)
			return
		}
		fmt.Fprintf(w, runJSON, "running")
	})
	_, err := c.Wait(context.Background(), "r0123456789abcdef01234567", WaitOptions{Timeout: 150 * time.Millisecond, PollInterval: 20 * time.Millisecond})
	var te *RunTimeoutError
	if !errors.As(err, &te) {
		t.Fatalf("expected timeout, got %v", err)
	}
}

func TestBundleRules(t *testing.T) {
	b, err := NewBundle("name = \"x\"", nil)
	if err != nil {
		t.Fatal(err)
	}
	for _, rel := range []string{"../x", "/abs", ".hidden", "a/../b", "plan.toml", "bundle.json", "inline/x", "a//b"} {
		if b.AddText(rel, "x") == nil {
			t.Errorf("accepted %q", rel)
		}
	}
	if err := b.AddText("scenarios/s.rhai", "fn run(s, r) {}"); err != nil || b.Files()["scenarios/s.rhai"] != 15 {
		t.Fatalf("valid path: %v", err)
	}
	if b.AddFile("x", "/etc") == nil {
		t.Fatal("directory accepted")
	}
	if _, err := NewBundle("   ", nil); err == nil {
		t.Fatal("empty plan accepted")
	}
}
