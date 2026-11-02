// File: ballista/sdks/go/integration_gate_test.go
package ballista

import (
	"context"
	"fmt"
	"os"
	"strings"
	"testing"
	"time"
)

// gatePlan uses a unique plan name so the automatic baseline is only ever a run from this test.
func gatePlan(demo, name, path string) *Plan {
	return &Plan{
		Name:      name,
		Target:    Target{BaseURL: demo, Timeout: "5s"},
		Load:      Users(S("1s", 10), S("4s", 10)),
		Scenarios: []Scenario{{Name: "detail", Wait: WaitFixed("20ms"), Steps: []Step{Get(path).Named("item detail").Expect(200)}}},
	}
}

func TestLiveRegressionGate(t *testing.T) {
	c, demo := live(t)
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Minute)
	defer cancel()
	name := fmt.Sprintf("go gate %d", time.Now().UnixNano())
	gate := &RegressionGate{MaxRegressionPct: Ptr(50.0), MinChangeMs: Ptr(5.0), Metrics: []CompareMetric{CompareP95, CompareErrorRate}}
	opts := func(workers *int) RunOptions {
		return RunOptions{CreateRunOptions: CreateRunOptions{Regression: gate, Overrides: &Overrides{Workers: workers}}, Wait: WaitOptions{Timeout: time.Minute}}
	}

	first, err := c.Run(ctx, gatePlan(demo, name, "/items/1"), opts(Ptr(0)))
	if err != nil || first.State != StatePassed || first.Comparison == nil || first.Comparison.Baseline != "" || first.Comparison.Note == "" {
		t.Fatalf("first run becomes the baseline: %+v %v", first, err)
	}
	if first.Regression == nil || *first.Regression.MaxRegressionPct != 50 {
		t.Fatalf("gate not stored on the run: %+v", first.Regression)
	}

	st, err := c.Status(ctx)
	if err != nil {
		t.Fatal(err)
	}
	var workers *int
	if len(st.Workers) > 0 {
		workers = Ptr(len(st.Workers))
	}
	second, err := c.Run(ctx, gatePlan(demo, name, "/items/1"), opts(workers))
	if err != nil || second.State != StatePassed || second.Comparison == nil || second.Comparison.Baseline != first.ID || !second.Comparison.Passed {
		t.Fatalf("second run compares with the first: %+v %v", second.Comparison, err)
	}
	if workers != nil && (second.Workers == nil || *second.Workers != *workers) {
		t.Fatalf("run not spread across %d workers: %v", *workers, second.Workers)
	}

	slow, err := c.Run(ctx, gatePlan(demo, name, "/slow"), opts(Ptr(0)))
	if err != nil || slow.State != StateFailed || slow.Comparison == nil || slow.Comparison.Passed || slow.Comparison.Regressions == 0 {
		t.Fatalf("slow run must fail the gate: %+v %v", slow, err)
	}
	if slow.Comparison.Baseline != second.ID || !strings.Contains(slow.Error, "regression") {
		t.Fatalf("baseline %s, error %q", slow.Comparison.Baseline, slow.Error)
	}
	if !slow.Comparison.Changes[0].Regressed || slow.Comparison.Changes[0].Metric != CompareP95 {
		t.Fatalf("regressions sort first: %+v", slow.Comparison.Changes[0])
	}

	explicit, err := c.Compare(ctx, slow.ID, CompareOptions{Baseline: first.ID, MaxRegressionPct: Ptr(10000.0)})
	if err != nil || explicit.Baseline != first.ID || !explicit.Passed {
		t.Fatalf("explicit baseline with a wide limit: %+v %v", explicit, err)
	}
}

func TestLiveCompareRejectsUnfinishedRun(t *testing.T) {
	c, demo := live(t)
	ctx := context.Background()
	run, err := c.Run(ctx, gatePlan(demo, "go unfinished", "/items/1"), RunOptions{CreateRunOptions: CreateRunOptions{Overrides: &Overrides{RunTime: "60s"}}, NoWait: true})
	if err != nil {
		t.Fatal(err)
	}
	defer func() {
		_, _ = c.StopRun(ctx, run.ID)
		_, _ = c.Wait(ctx, run.ID, WaitOptions{Timeout: time.Minute})
	}()
	if _, err := c.Compare(ctx, run.ID, CompareOptions{}); !IsConflict(err) {
		t.Fatalf("compare of an unfinished run: %v", err)
	}
	if _, err := c.Compare(ctx, "r000000000000000000000000", CompareOptions{}); !IsNotFound(err) {
		t.Fatalf("compare of an unknown run: %v", err)
	}
}

func TestLiveAdminDeletion(t *testing.T) {
	c, demo := live(t)
	admin := os.Getenv("BALLISTA_ADMIN_TOKEN")
	if admin == "" {
		t.Skip("BALLISTA_ADMIN_TOKEN not set")
	}
	ctx := context.Background()
	plan := gatePlan(demo, fmt.Sprintf("go delete %d", time.Now().UnixNano()), "/items/1")
	info, err := c.UploadPlan(ctx, plan, map[string]string{"purpose": "deletion"})
	if err != nil {
		t.Fatal(err)
	}
	listed := false
	bundles, err := c.ListBundles(ctx)
	if err != nil {
		t.Fatal(err)
	}
	for _, b := range bundles {
		listed = listed || b.ID == info.ID
	}
	if !listed {
		t.Fatal("uploaded bundle not listed")
	}
	if got, err := c.GetBundle(ctx, info.ID); err != nil || got.Labels["purpose"] != "deletion" || got.PlanName != plan.Name {
		t.Fatalf("get bundle: %+v %v", got, err)
	}
	run, err := c.Run(ctx, info, RunOptions{CreateRunOptions: CreateRunOptions{Overrides: &Overrides{RunTime: "1s"}}, Wait: WaitOptions{Timeout: time.Minute}})
	if err != nil || !run.State.Terminal() {
		t.Fatalf("run: %v %v", run, err)
	}
	if err := c.DeleteRun(ctx, run.ID); !IsForbidden(err) {
		t.Fatalf("run scope must not delete: %v", err)
	}
	ac, err := New(Options{Token: admin})
	if err != nil {
		t.Fatal(err)
	}
	if err := ac.DeleteRun(ctx, run.ID); err != nil {
		t.Fatal(err)
	}
	if _, err := c.GetRun(ctx, run.ID); !IsNotFound(err) {
		t.Fatalf("deleted run still present: %v", err)
	}
	if err := ac.DeleteBundle(ctx, info.ID); err != nil {
		t.Fatal(err)
	}
	if _, err := c.GetBundle(ctx, info.ID); !IsNotFound(err) {
		t.Fatalf("deleted bundle still present: %v", err)
	}
}

func TestLiveStopDuringStartup(t *testing.T) {
	c, demo := live(t)
	ctx := context.Background()
	st, err := c.Status(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(st.Workers) == 0 || st.ActiveRun != "" || st.Queued != 0 {
		t.Skip("needs an idle server with pool workers")
	}
	// More workers than are connected keeps the run in its startup phase while the server waits for them.
	run, err := c.Run(ctx, gatePlan(demo, "go startup stop", "/items/1"), RunOptions{CreateRunOptions: CreateRunOptions{Overrides: &Overrides{Workers: Ptr(len(st.Workers) + 5)}}, NoWait: true})
	if err != nil {
		t.Fatal(err)
	}
	time.Sleep(500 * time.Millisecond)
	stopped, err := c.StopRun(ctx, run.ID)
	if err != nil {
		t.Fatalf("stop during startup: %v", err)
	}
	if stopped.State != StateStopping && stopped.State != StateCancelled {
		t.Fatalf("state after stop: %s", stopped.State)
	}
	start := time.Now()
	done, err := c.Wait(ctx, run.ID, WaitOptions{Timeout: 30 * time.Second, PollInterval: 100 * time.Millisecond})
	if err != nil || done.State != StateCancelled || done.StoppedBy == "" || done.StartedAt != "" {
		t.Fatalf("cancelled before start: %+v %v", done, err)
	}
	if time.Since(start) > 5*time.Second {
		t.Fatalf("cancellation waited %v for the worker timeout", time.Since(start))
	}
}
