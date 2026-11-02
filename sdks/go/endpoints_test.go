// File: ballista/sdks/go/endpoints_test.go
package ballista

import (
	"context"
	"encoding/base64"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestServerInfoEndpoints(t *testing.T) {
	f := newFake(t)
	c := f.client(t)
	ctx := context.Background()
	if h, err := c.Health(ctx); err != nil || h.Version != "0.1.0" {
		t.Fatalf("health: %v %v", h, err)
	}
	if h := f.headers["health"]; h.Get("User-Agent") != userAgent || h.Get("Accept") != "application/json" {
		t.Fatalf("headers: %v", h)
	}
	if w, err := c.WhoAmI(ctx); err != nil || w.Scope != "run" {
		t.Fatalf("whoami: %v %v", w, err)
	}
	st, err := c.Status(ctx)
	if err != nil || st.Queued != 1 || st.Limits.MaxQueue != 50 || len(st.Workers) != 1 || st.Workers[0].CPUPct != nil || st.Workers[0].Cores != 8 {
		t.Fatalf("status: %+v %v", st, err)
	}
	bad, _ := New(Options{URL: f.srv.URL, Token: strings.Repeat("x", 40)})
	if _, err := bad.Health(ctx); !IsUnauthorized(err) {
		t.Fatalf("wrong token: %v", err)
	}
}

func TestValidateAndBundles(t *testing.T) {
	f := newFake(t)
	c := f.client(t)
	ctx := context.Background()
	plan := shopPlan("http://127.0.0.1:8080/")
	check, err := c.ValidatePlan(ctx, plan, &Overrides{RunTime: "5s", Workers: Ptr(0)})
	if err != nil || check.Name != "shop api" || check.Files == nil || len(check.Files) != 0 {
		t.Fatalf("validate: %+v %v", check, err)
	}
	ov := f.bodies["validate"]["overrides"].(map[string]any)
	if ov["run_time"] != "5s" || ov["workers"] != 0.0 {
		t.Fatalf("overrides sent: %v", ov)
	}
	if _, err := c.ValidatePlan(ctx, 42, nil); err == nil {
		t.Fatal("unsupported plan type accepted")
	}
	if _, err := c.ValidatePlan(ctx, plan, &Overrides{Users: Ptr(0.0)}); err == nil {
		t.Fatal("invalid overrides sent")
	}

	b, _ := NewBundle(plan, map[string]string{"suite": "unit"})
	if err := b.AddBytes("data/bin.dat", []byte{0, 1, 2, 255}); err != nil {
		t.Fatal(err)
	}
	info, err := c.UploadBundle(ctx, b)
	if err != nil {
		t.Fatal(err)
	}
	sent := f.bodies["upload"]
	files := sent["files"].(map[string]any)
	if raw, _ := base64.StdEncoding.DecodeString(files["data/bin.dat"].(string)); string(raw) != "\x00\x01\x02\xff" {
		t.Fatalf("file not base64 encoded: %v", files)
	}
	if sent["labels"].(map[string]any)["suite"] != "unit" || sent["plan"].(map[string]any)["name"] != "shop api" {
		t.Fatalf("payload: %v", sent)
	}
	again, err := c.UploadPlan(ctx, plan, nil)
	if err != nil || again.ID != info.ID {
		t.Fatalf("same plan, same id: %v %v", again, err)
	}
	if _, err := c.UploadBundle(ctx, nil); err == nil {
		t.Fatal("nil bundle accepted")
	}
	list, err := c.ListBundles(ctx)
	if err != nil || len(list) != 1 {
		t.Fatalf("list: %v %v", list, err)
	}
	if got, err := c.GetBundle(ctx, info.ID); err != nil || got.ID != info.ID {
		t.Fatalf("get: %v %v", got, err)
	}
	if err := c.DeleteBundle(ctx, info.ID); err != nil {
		t.Fatal(err)
	}
	if _, err := c.GetBundle(ctx, info.ID); !IsNotFound(err) {
		t.Fatalf("deleted bundle: %v", err)
	}
	if err := c.DeleteBundle(ctx, "ABC"); err == nil {
		t.Fatal("malformed bundle id accepted")
	}
}

func uploaded(t *testing.T, f *fakeAPI, c *Client) string {
	t.Helper()
	info, err := c.UploadPlan(context.Background(), shopPlan("http://127.0.0.1:8080/"), nil)
	if err != nil {
		t.Fatal(err)
	}
	return info.ID
}

func TestRunLifecycleEndpoints(t *testing.T) {
	f := newFake(t)
	c := f.client(t)
	ctx := context.Background()
	bundle := uploaded(t, f, c)
	opts := CreateRunOptions{Labels: map[string]string{"build": "7"}, IdempotencyKey: "build-7"}
	first, err := c.CreateRun(ctx, bundle, opts)
	if err != nil || first.Labels["build"] != "7" {
		t.Fatalf("create: %v %v", first, err)
	}
	dup, _ := c.CreateRun(ctx, bundle, opts)
	if dup.ID != first.ID || f.headers["create"].Get("Idempotency-Key") != "build-7" {
		t.Fatal("idempotency key not honored")
	}
	if _, err := c.CreateRun(ctx, strings.Repeat("a", 32), CreateRunOptions{}); !IsNotFound(err) {
		t.Fatalf("unknown bundle: %v", err)
	}
	if body := f.bodies["create"]; body["overrides"] == nil || body["labels"] == nil {
		t.Fatalf("empty overrides and labels must be objects: %v", body)
	}
	for i := 0; i < 4; i++ {
		if _, err := c.CreateRun(ctx, bundle, CreateRunOptions{}); err != nil {
			t.Fatal(err)
		}
	}
	page, err := c.ListRuns(ctx, ListOptions{State: StateQueued, Limit: 500, Cursor: "0"})
	if err != nil || len(page.Items) != 5 {
		t.Fatalf("list: %v %v", page, err)
	}
	if q := f.queries["list"]; q.Get("limit") != "200" || q.Get("state") != "queued" {
		t.Fatalf("list query: %v", q)
	}
	if _, err := c.ListRuns(ctx, ListOptions{State: "bogus"}); err == nil {
		t.Fatal("unknown state accepted")
	}
	var ids []string
	for r, err := range c.Runs(ctx, "", 2) {
		if err != nil {
			t.Fatal(err)
		}
		ids = append(ids, r.ID)
	}
	if len(ids) != 5 || f.calls["list"] != 4 {
		t.Fatalf("pagination: %d runs over %d calls", len(ids), f.calls["list"])
	}
	n := 0
	for range c.Runs(ctx, "", 2) {
		n++
		break
	}
	if n != 1 {
		t.Fatal("early break")
	}
	stopped, err := c.StopRun(ctx, first.ID)
	if err != nil || stopped.State != StateCancelled || stopped.StoppedBy != "ci" {
		t.Fatalf("stop: %v %v", stopped, err)
	}
	if err := c.DeleteRun(ctx, first.ID); err != nil || f.calls["delete-run"] != 1 {
		t.Fatalf("delete: %v", err)
	}
	if _, err := c.StopRun(ctx, "r000000000000000000000fff"); !IsNotFound(err) {
		t.Fatalf("unknown run: %v", err)
	}
}

func TestStatsCompareAndArtifacts(t *testing.T) {
	f := newFake(t)
	c := f.client(t)
	ctx := context.Background()
	id := "r000000000000000000000001"
	snap, err := c.GetStats(ctx, id)
	if err != nil || snap.Total.Count != 40 || snap.Users != 4 || len(snap.Distribution) != 2 || snap.Distribution[1].UpperMs != nil {
		t.Fatalf("stats: %+v %v", snap, err)
	}
	if r := snap.FindRow("item", "SSE"); r == nil || r.Count != 3 {
		t.Fatal("row by method")
	}
	if r := snap.FindRow("purchase", ""); r == nil || r.Method != "TXN" {
		t.Fatal("transaction row")
	}
	if snap.FindRow("missing", "") != nil {
		t.Fatal("missing row found")
	}

	cmp, err := c.Compare(ctx, id, CompareOptions{Baseline: "r000000000000000000000002", MaxRegressionPct: Ptr(12.5), Metrics: []CompareMetric{CompareP95, CompareRPS}})
	if err != nil || cmp.Passed || cmp.Regressions != 1 || !cmp.Changes[0].Regressed || *cmp.Changes[0].ChangePct != 80 || cmp.Unmatched[0] != "GET /gone" {
		t.Fatalf("compare: %+v %v", cmp, err)
	}
	q := f.queries["compare"]
	if q.Get("baseline") != "r000000000000000000000002" || q.Get("max_regression_pct") != "12.5" || q.Get("metrics") != "p95,rps" {
		t.Fatalf("compare query: %v", q)
	}
	if _, err := c.Compare(ctx, id, CompareOptions{}); err != nil || f.queries["compare"].Get("baseline") != "auto" {
		t.Fatalf("default baseline: %v", f.queries["compare"])
	}
	for _, o := range []CompareOptions{{Baseline: "latest"}, {MaxRegressionPct: Ptr(-1.0)}, {Metrics: []CompareMetric{"p42"}}} {
		if _, err := c.Compare(ctx, id, o); err == nil {
			t.Errorf("accepted %+v", o)
		}
	}

	rep, err := c.Report(ctx, id)
	if err != nil || rep["plan"] != "p" {
		t.Fatalf("report: %v %v", rep, err)
	}
	if _, err := c.Artifact(ctx, id, "../plan.toml"); err == nil {
		t.Fatal("artifact traversal accepted")
	}
	dir := t.TempDir()
	path, err := c.Download(ctx, id, "report.html", dir)
	if err != nil || path != filepath.Join(dir, id+"-report.html") {
		t.Fatalf("download to dir: %v %v", path, err)
	}
	st, _ := os.Stat(path)
	if st.Mode().Perm() != 0o644 {
		t.Fatalf("mode %v", st.Mode())
	}
	file := filepath.Join(dir, "custom.html")
	if p, err := c.Download(ctx, id, "report.html", file); err != nil || p != file {
		t.Fatalf("download to file: %v %v", p, err)
	}
	entries, _ := os.ReadDir(dir)
	for _, e := range entries {
		if strings.HasPrefix(e.Name(), ".download-") {
			t.Fatalf("temporary file left behind: %s", e.Name())
		}
	}
	if _, err := c.Download(ctx, id, "report.html", filepath.Join(dir, "missing", "x.html")); err == nil {
		t.Fatal("download into a missing directory succeeded")
	}
}

func TestRunConvenience(t *testing.T) {
	f := newFake(t)
	c := f.client(t)
	ctx := context.Background()
	plan := shopPlan("http://127.0.0.1:8080/")
	done, err := c.Run(ctx, plan, RunOptions{CreateRunOptions: CreateRunOptions{Regression: &RegressionGate{Baseline: "auto"}}})
	if err != nil || done.State != StatePassed || done.Summary == nil || done.Summary.Requests != 40 {
		t.Fatalf("run plan: %+v %v", done, err)
	}
	if f.bodies["create"]["regression"].(map[string]any)["baseline"] != "auto" {
		t.Fatal("regression gate not sent")
	}
	queued, err := c.Run(ctx, done.Bundle, RunOptions{NoWait: true})
	if err != nil || queued.State != StateQueued {
		t.Fatalf("run by id: %v %v", queued, err)
	}
	info, _ := c.GetBundle(ctx, done.Bundle)
	if r, err := c.Run(ctx, info, RunOptions{NoWait: true}); err != nil || r.Bundle != info.ID {
		t.Fatalf("run by info: %v %v", r, err)
	}
	b, _ := NewBundle(plan, nil)
	f.final = StateFailed
	if r, err := c.Run(ctx, b, RunOptions{}); err != nil || r.State != StateFailed {
		t.Fatalf("run bundle: %v %v", r, err)
	}
	if _, err := c.Run(ctx, "not-a-bundle", RunOptions{}); err == nil {
		t.Fatal("bad bundle id accepted")
	}
	var ce *ConfigurationError
	if _, err := c.Run(ctx, 3.14, RunOptions{}); !errors.As(err, &ce) {
		t.Fatalf("unsupported target: %v", err)
	}
}
