// File: ballista/sdks/go/integration_test.go
package ballista

import (
	"context"
	"fmt"
	"os"
	"strings"
	"testing"
	"time"
)

// Requires BALLISTA_URL, BALLISTA_TOKEN (run scope), BALLISTA_READ_TOKEN and BALLISTA_DEMO_URL; skipped otherwise.
func live(t *testing.T) (*Client, string) {
	t.Helper()
	for _, k := range []string{"BALLISTA_URL", "BALLISTA_TOKEN", "BALLISTA_READ_TOKEN", "BALLISTA_DEMO_URL"} {
		if os.Getenv(k) == "" {
			t.Skip("integration environment not configured")
		}
	}
	c, err := New(Options{Timeout: time.Minute})
	if err != nil {
		t.Fatal(err)
	}
	return c, os.Getenv("BALLISTA_DEMO_URL")
}

func TestLiveLifecycle(t *testing.T) {
	c, demo := live(t)
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()
	if who, err := c.WhoAmI(ctx); err != nil || who.Scope != "run" {
		t.Fatalf("whoami: %v %v", who, err)
	}
	check, err := c.ValidatePlan(ctx, shopPlan(demo), nil)
	if err != nil || len(check.Files) != 0 || check.Name != "shop api" {
		t.Fatalf("validate: %v %v", check, err)
	}
	info, err := c.UploadPlan(ctx, shopPlan(demo), nil)
	if err != nil {
		t.Fatal(err)
	}
	again, err := c.UploadPlan(ctx, shopPlan(demo), nil)
	if err != nil || again.ID != info.ID {
		t.Fatalf("content addressing: %v %v", again, err)
	}
	key := fmt.Sprintf("go-%d", time.Now().UnixNano())
	opts := CreateRunOptions{Overrides: &Overrides{RunTime: "5s"}, Labels: map[string]string{"build": "7"}, IdempotencyKey: key}
	run, err := c.CreateRun(ctx, info.ID, opts)
	if err != nil {
		t.Fatal(err)
	}
	dup, err := c.CreateRun(ctx, info.ID, opts)
	if err != nil || dup.ID != run.ID {
		t.Fatalf("idempotency: %v %v", dup, err)
	}
	var stats int
	done, err := c.Wait(ctx, run.ID, WaitOptions{Timeout: 90 * time.Second, OnStats: func(*Snapshot) { stats++ }})
	if err != nil || done.State != StatePassed || done.Summary == nil || done.Summary.Requests < 10 || stats == 0 {
		t.Fatalf("wait: %+v %v stats=%d", done, err, stats)
	}
	snap, err := c.GetStats(ctx, run.ID)
	if err != nil || snap.FindRow("purchase", "") == nil {
		t.Fatalf("stats: %v", err)
	}
	path, err := c.Download(ctx, run.ID, "report.html", t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	if html, _ := os.ReadFile(path); !strings.HasPrefix(string(html), "<!DOCTYPE html>") {
		t.Fatal("report.html content")
	}
	found := false
	for r, err := range c.Runs(ctx, StatePassed, 50) {
		if err != nil {
			t.Fatal(err)
		}
		if r.ID == run.ID {
			found = true
			break
		}
	}
	if !found {
		t.Fatal("run not listed")
	}
	cmp, err := c.Compare(ctx, run.ID, CompareOptions{MaxRegressionPct: Ptr(10000.0), Metrics: []CompareMetric{CompareP95, CompareErrorRate}})
	if err != nil || len(cmp.Changes) == 0 || (cmp.Baseline != "" && !cmp.Passed) {
		t.Fatalf("compare: %+v %v", cmp, err)
	}
}

func TestLivePlanTextAndAssets(t *testing.T) {
	c, demo := live(t)
	ctx := context.Background()
	text := fmt.Sprintf("name = \"text\"\n[target]\nbase_url = %q\n[load]\nstages = [{ duration = \"1s\", target = 1 }]\n[[scenario]]\nname = \"s\"\nscript = \"scenarios/s.rhai\"\n", demo)
	check, err := c.ValidatePlan(ctx, text, nil)
	if err != nil || len(check.Files) != 1 || check.Files[0] != "scenarios/s.rhai" {
		t.Fatalf("missing assets: %v %v", check, err)
	}
	if _, err := c.UploadPlan(ctx, text, nil); !IsValidation(err) {
		t.Fatalf("upload without asset: %v", err)
	}
	b, _ := NewBundle(text, nil)
	if err := b.AddText("scenarios/s.rhai", `fn run(step, res) { if step == 0 { get("/items") } }`); err != nil {
		t.Fatal(err)
	}
	done, err := c.Run(ctx, b, RunOptions{CreateRunOptions: CreateRunOptions{Overrides: &Overrides{RunTime: "2s"}}, Wait: WaitOptions{Timeout: time.Minute}})
	if err != nil || done.State != StatePassed {
		t.Fatalf("run: %+v %v", done, err)
	}
}

func TestLiveStopPolicyAndScopes(t *testing.T) {
	c, demo := live(t)
	ctx := context.Background()
	info, err := c.UploadPlan(ctx, shopPlan(demo), nil)
	if err != nil {
		t.Fatal(err)
	}
	long := CreateRunOptions{Overrides: &Overrides{RunTime: "60s"}}
	first, err := c.CreateRun(ctx, info.ID, long)
	if err != nil {
		t.Fatal(err)
	}
	second, err := c.CreateRun(ctx, info.ID, long)
	if err != nil {
		t.Fatal(err)
	}
	if r, err := c.StopRun(ctx, second.ID); err != nil || r.State != StateCancelled {
		t.Fatalf("cancel: %v %v", r, err)
	}
	for i := 0; i < 100; i++ {
		if r, _ := c.GetRun(ctx, first.ID); r != nil && r.State != StateQueued {
			break
		}
		time.Sleep(200 * time.Millisecond)
	}
	if _, err := c.StopRun(ctx, first.ID); err != nil {
		t.Fatal(err)
	}
	if r, err := c.Wait(ctx, first.ID, WaitOptions{Timeout: time.Minute}); err != nil || r.State != StateStopped {
		t.Fatalf("stop: %v %v", r, err)
	}
	if _, err := c.ValidatePlan(ctx, shopPlan("http://10.0.0.1:8080/"), nil); !IsValidation(err) || !strings.Contains(err.Error(), "not permitted") {
		t.Fatalf("target policy: %v", err)
	}
	if _, err := c.CreateRun(ctx, info.ID, CreateRunOptions{Overrides: &Overrides{Users: Ptr(100000.0), SpawnRate: Ptr(100000.0), RunTime: "10s"}}); !IsValidation(err) {
		t.Fatalf("limits: %v", err)
	}
	ro, err := New(Options{Token: os.Getenv("BALLISTA_READ_TOKEN")})
	if err != nil {
		t.Fatal(err)
	}
	if st, err := ro.Status(ctx); err != nil || st.Limits.MaxQueue == 0 {
		t.Fatalf("status: %v %v", st, err)
	}
	if _, err := ro.CreateRun(ctx, info.ID, CreateRunOptions{}); !IsForbidden(err) {
		t.Fatalf("scope: %v", err)
	}
}
