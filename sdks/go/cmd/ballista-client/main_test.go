// File: ballista/sdks/go/cmd/ballista-client/main_test.go
package main

import (
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
)

const token = "bt_0123456789abcdef0123456789abcdef"

const runBody = `{"id":"r0123456789abcdef01234567","seq":1,"bundle":"0123456789abcdef0123456789abcdef","plan_name":"p","state":"%s",` +
	`"created_at":"t","created_by":"ci","labels":{},"overrides":{},` +
	`"summary":{"requests":10,"failures":0,"rps":2,"thresholds":[{"metric":"p95","request":"*","max":100,"actual":5,"passed":true}]},` +
	`"comparison":{"baseline":null,"passed":true,"regressions":0,"changes":[],"unmatched":[],"note":"first run"}}`

type server struct {
	mu      sync.Mutex
	state   string
	created map[string]any
	passed  bool
}

func start(t *testing.T, state string) *server {
	t.Helper()
	s := &server{state: state, passed: true}
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("Authorization") != "Bearer "+token {
			w.WriteHeader(401)
			return
		}
		s.mu.Lock()
		defer s.mu.Unlock()
		p := r.URL.Path
		switch {
		case p == "/api/v1/bundles":
			_, _ = io.WriteString(w, `{"id":"0123456789abcdef0123456789abcdef","plan_name":"p","scenarios":[],"files":[],"labels":{},"created_at":"t","created_by":"ci"}`)
		case p == "/api/v1/runs" && r.Method == "POST":
			_ = json.NewDecoder(r.Body).Decode(&s.created)
			w.WriteHeader(202)
			fmt.Fprintf(w, runBody, "queued")
		case p == "/api/v1/runs":
			_, _ = io.WriteString(w, `{"items":[`+fmt.Sprintf(runBody, s.state)+`],"next_cursor":null}`)
		case strings.HasSuffix(p, "/events"):
			fmt.Fprintf(w, "event: end\ndata: "+runBody+"\n\n", s.state)
		case strings.HasSuffix(p, "/compare"):
			fmt.Fprintf(w, `{"baseline":"r000000000000000000000001","passed":%t,"regressions":%d,"changes":[{"kind":"total","method":"","name":"Aggregated","metric":"p95","baseline":1,"current":2,"change_pct":100,"regressed":%t}],"unmatched":[]}`,
				s.passed, map[bool]int{true: 0, false: 1}[s.passed], !s.passed)
		case strings.Contains(p, "/artifacts/"):
			_, _ = io.WriteString(w, "artifact")
		case strings.HasSuffix(p, "/stop"):
			fmt.Fprintf(w, runBody, "cancelled")
		default:
			fmt.Fprintf(w, runBody, s.state)
		}
	}))
	t.Cleanup(srv.Close)
	t.Setenv("BALLISTA_URL", srv.URL)
	t.Setenv("BALLISTA_TOKEN", token)
	return s
}

// capture runs the CLI with stdout and stderr redirected.
func capture(t *testing.T, args ...string) (int, string, error) {
	t.Helper()
	r, w, _ := os.Pipe()
	oldOut, oldErr := os.Stdout, os.Stderr
	os.Stdout, os.Stderr = w, w
	code, err := run(args)
	os.Stdout, os.Stderr = oldOut, oldErr
	_ = w.Close()
	out, _ := io.ReadAll(r)
	return code, string(out), err
}

func writePlan(t *testing.T, name, content string) string {
	t.Helper()
	p := filepath.Join(t.TempDir(), name)
	if err := os.WriteFile(p, []byte(content), 0o600); err != nil {
		t.Fatal(err)
	}
	return p
}

func TestSubmitSendsOverridesAndGate(t *testing.T) {
	s := start(t, "passed")
	plan := writePlan(t, "plan.json", `{"name":"p","target":{"base_url":"http://127.0.0.1:8080/"},"load":{"stages":[{"duration":"1s","target":1}]},"scenario":[{"name":"s","step":[{"get":"/"}]}]}`)
	asset := writePlan(t, "s.rhai", "fn run(s, r) {}")
	reports := t.TempDir()
	code, out, err := capture(t, "submit", plan, "--users", "5", "--run-time=10s", "--workers", "0", "--tags", "a,b",
		"--label", "build=7", "--label", "branch=main", "--file", "scenarios/s.rhai="+asset, "--gate", "15", "--baseline", "auto", "--quiet", "--report", reports)
	if err != nil || code != 0 {
		t.Fatalf("submit: %d %v\n%s", code, err, out)
	}
	ov := s.created["overrides"].(map[string]any)
	if ov["users"] != 5.0 || ov["run_time"] != "10s" || ov["workers"] != 0.0 || len(ov["tags"].([]any)) != 2 {
		t.Fatalf("overrides: %v", ov)
	}
	if g := s.created["regression"].(map[string]any); g["max_regression_pct"] != 15.0 || g["baseline"] != "auto" {
		t.Fatalf("gate: %v", g)
	}
	if l := s.created["labels"].(map[string]any); l["build"] != "7" || l["branch"] != "main" {
		t.Fatalf("labels: %v", l)
	}
	for _, want := range []string{"queued run r0123456789abcdef01234567", "requests=10", "threshold p95 * actual=5.000 pass", "note: first run"} {
		if !strings.Contains(out, want) {
			t.Errorf("output lacks %q:\n%s", want, out)
		}
	}
	entries, _ := os.ReadDir(reports)
	if len(entries) != 2 {
		t.Fatalf("reports: %v", entries)
	}
}

func TestSubmitWithoutGateOrWorkersOmitsThem(t *testing.T) {
	s := start(t, "failed")
	plan := writePlan(t, "plan.toml", "name = \"p\"\n")
	code, _, err := capture(t, "submit", plan, "--no-wait")
	if err != nil || code != 0 {
		t.Fatalf("%d %v", code, err)
	}
	if _, ok := s.created["regression"]; ok {
		t.Fatal("gate sent without --gate")
	}
	if _, ok := s.created["overrides"].(map[string]any)["workers"]; ok {
		t.Fatal("workers sent without --workers")
	}
	if code, _, _ := capture(t, "submit", plan, "--quiet"); code != exitFailed {
		t.Fatalf("failed run exit code %d", code)
	}
}

func TestSubmitInputErrors(t *testing.T) {
	start(t, "passed")
	good := writePlan(t, "p.toml", "name = \"p\"\n")
	for name, args := range map[string][]string{
		"no plan":      {"submit"},
		"missing file": {"submit", filepath.Join(t.TempDir(), "nope.toml")},
		"bad json":     {"submit", writePlan(t, "p.json", "[1,2]")},
		"label":        {"submit", good, "--label", "novalue"},
		"file spec":    {"submit", good, "--file", "noequals"},
		"asset path":   {"submit", good, "--file", "../x=" + good},
		"users":        {"submit", good, "--users", "many"},
		"run time":     {"submit", good, "--run-time", "forever"},
		"oversized":    {"submit", writePlan(t, "big.toml", strings.Repeat("#", 1<<20+1))},
	} {
		if code, _, err := capture(t, args...); code != exitError || err == nil {
			t.Errorf("%s: code=%d err=%v", name, code, err)
		}
	}
}

func TestOtherCommands(t *testing.T) {
	s := start(t, "stopped")
	id := "r0123456789abcdef01234567"
	if code, out, _ := capture(t, "status", id); code != 0 || !strings.Contains(out, "run "+id+" stopped") {
		t.Fatalf("status: %d %s", code, out)
	}
	if code, out, _ := capture(t, "stop", id); code != 0 || !strings.Contains(out, "cancelled") {
		t.Fatalf("stop: %d %s", code, out)
	}
	if code, _, _ := capture(t, "wait", id); code != exitFailed {
		t.Fatalf("wait on stopped run: %d", code)
	}
	if code, out, _ := capture(t, "list"); code != 0 || !strings.Contains(out, `"state":"stopped"`) {
		t.Fatalf("list: %d %s", code, out)
	}
	if code, out, _ := capture(t, "compare", id, "--max-regression-pct", "5"); code != 0 || !strings.Contains(out, "passed=true") {
		t.Fatalf("compare: %d %s", code, out)
	}
	s.passed = false
	if code, out, _ := capture(t, "compare", id); code != exitFailed || !strings.Contains(out, "REGRESSED") {
		t.Fatalf("compare regressed: %d %s", code, out)
	}
	dir := t.TempDir()
	if code, out, _ := capture(t, "report", id, "--name", "stats.csv", "--out", dir); code != 0 || !strings.Contains(out, id+"-stats.csv") {
		t.Fatalf("report: %d %s", code, out)
	}
	if code, _, err := capture(t, "report", id, "--name", "../../etc/passwd"); code != exitError || err == nil {
		t.Fatal("artifact name not checked")
	}
	if code, _, err := capture(t, "status"); code != exitError || err == nil {
		t.Fatal("missing id")
	}
	if code, _, err := capture(t, "status", "../x"); code != exitError || err == nil {
		t.Fatal("bad id")
	}
	if code, _, _ := capture(t, "dance"); code != exitError {
		t.Fatal("unknown command")
	}
	if code, _, _ := capture(t); code != exitError {
		t.Fatal("no command")
	}
	if code, _, err := capture(t, "--help"); code != 0 || err != nil {
		t.Fatal("help")
	}
}

func TestConfigurationErrors(t *testing.T) {
	start(t, "passed")
	t.Setenv("BALLISTA_TOKEN", "short")
	if code, _, err := capture(t, "list"); code != exitError || err == nil || strings.Contains(err.Error(), "short") {
		t.Fatalf("bad token: %d %v", code, err)
	}
	t.Setenv("BALLISTA_TOKEN", token)
	if code, _, err := capture(t, "--url", "http://203.0.113.9:8070", "list"); code != exitError || err == nil {
		t.Fatal("plain http to a remote host")
	}
}

func TestParseAcceptsInterspersedFlags(t *testing.T) {
	fs := flag.NewFlagSet("x", flag.ContinueOnError)
	n := fs.Int("n", 0, "")
	var m multi
	fs.Var(&m, "m", "")
	pos, err := parse(fs, []string{"a", "--n", "3", "b", "--m", "x", "--m=y", "c"})
	if err != nil || strings.Join(pos, ",") != "a,b,c" || *n != 3 || m.String() != "x,y" {
		t.Fatalf("%v %v %d %v", pos, err, *n, m)
	}
	if _, err := parse(fs, []string{"--unknown"}); err == nil {
		t.Fatal("unknown flag accepted")
	}
}
