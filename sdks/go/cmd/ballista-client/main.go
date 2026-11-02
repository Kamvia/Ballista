// File: ballista/sdks/go/cmd/ballista-client/main.go
package main

import (
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"os/signal"
	"slices"
	"strconv"
	"strings"
	"syscall"
	"time"

	ballista "github.com/Kamvia/Ballista/sdks/go"
)

const (
	exitError   = 1
	exitFailed  = 2
	exitTimeout = 3
)

const usage = `usage: ballista-client [--url URL] <command>
  submit <plan.json|plan.toml> [--file REL=PATH]... [--users N] [--spawn-rate N] [--run-time D] [--iterations N]
         [--tags a,b] [--host URL] [--workers N] [--label k=v]... [--gate PCT] [--baseline ID]
         [--no-wait] [--timeout SECS] [--quiet] [--report DIR]
  status <run-id> | stop <run-id> | wait <run-id> | list
  compare <run-id> [--baseline ID] [--max-regression-pct PCT]
  report <run-id> [--name report.html] [--out DIR]`

type multi []string

func (m *multi) String() string     { return strings.Join(*m, ",") }
func (m *multi) Set(v string) error { *m = append(*m, v); return nil }

// parse accepts flags before and after positional arguments.
func parse(fs *flag.FlagSet, args []string) ([]string, error) {
	var pos []string
	for {
		if err := fs.Parse(args); err != nil {
			return nil, err
		}
		args = fs.Args()
		if len(args) == 0 {
			return pos, nil
		}
		pos = append(pos, args[0])
		args = args[1:]
	}
}

func summary(r *ballista.Run) {
	fmt.Printf("run %s %s\n", r.ID, r.State)
	if r.Error != "" {
		fmt.Printf("error: %s\n", r.Error)
	}
	if s := r.Summary; s != nil {
		fmt.Printf("requests=%d failures=%d rps=%.1f p50=%.1fms p95=%.1fms p99=%.1fms duration=%.0fs\n",
			s.Requests, s.Failures, s.RPS, s.P50Ms, s.P95Ms, s.P99Ms, s.DurationSecs)
		for _, t := range s.Thresholds {
			actual, verdict := "n/a", "FAIL"
			if t.Actual != nil {
				actual = strconv.FormatFloat(*t.Actual, 'f', 3, 64)
			}
			if t.Passed {
				verdict = "pass"
			}
			fmt.Printf("threshold %s %s actual=%s %s\n", t.Metric, t.Request, actual, verdict)
		}
	}
	if r.Comparison != nil {
		comparison(r.Comparison)
	}
}

func comparison(c *ballista.Comparison) {
	base := c.Baseline
	if base == "" {
		base = "-"
	}
	fmt.Printf("baseline=%s passed=%t regressions=%d\n", base, c.Passed, c.Regressions)
	for _, ch := range c.Changes {
		pct, flag := "n/a", "ok"
		if ch.ChangePct != nil {
			pct = fmt.Sprintf("%+.1f%%", *ch.ChangePct)
		}
		if ch.Regressed {
			flag = "REGRESSED"
		}
		fmt.Printf("  %s %s %s %s %.3f -> %.3f %s %s\n", ch.Kind, ch.Method, ch.Name, ch.Metric, ch.Baseline, ch.Current, pct, flag)
	}
	if c.Note != "" {
		fmt.Printf("note: %s\n", c.Note)
	}
}

func progress() func(*ballista.Snapshot) {
	last := -1
	return func(s *ballista.Snapshot) {
		second := int(s.ElapsedSecs)
		if second == last {
			return
		}
		last = second
		fmt.Fprintf(os.Stderr, "  t=%ds users=%d rps=%.1f p95=%.1fms failures=%d\n", second, s.Users, s.Total.RPS, s.Total.P95Ms, s.Total.Failures)
	}
}

func exitCode(r *ballista.Run) int {
	switch r.State {
	case ballista.StatePassed:
		return 0
	case ballista.StateFailed, ballista.StateStopped:
		return exitFailed
	}
	return exitError
}

func one(cmd string, pos []string) (string, error) {
	if len(pos) != 1 {
		return "", fmt.Errorf("%s requires one argument", cmd)
	}
	return pos[0], nil
}

func loadPlan(path string) (any, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	raw, err := io.ReadAll(io.LimitReader(f, ballista.MaxPlanBytes+1))
	if err != nil {
		return nil, err
	}
	if len(raw) > ballista.MaxPlanBytes {
		return nil, errors.New("plan file exceeds 1 MiB")
	}
	if strings.HasSuffix(strings.ToLower(path), ".json") {
		var doc map[string]any
		if err := json.Unmarshal(raw, &doc); err != nil {
			return nil, errors.New("plan document must be a JSON object")
		}
		return doc, nil
	}
	return string(raw), nil
}

func submit(ctx context.Context, c *ballista.Client, args []string) (int, error) {
	fs := flag.NewFlagSet("submit", flag.ContinueOnError)
	var files, labels multi
	fs.Var(&files, "file", "bundle asset REL=PATH")
	fs.Var(&labels, "label", "run label k=v")
	users := fs.Float64("users", 0, "")
	rate := fs.Float64("spawn-rate", 0, "")
	runTime := fs.String("run-time", "", "")
	iterations := fs.Uint64("iterations", 0, "")
	tags := fs.String("tags", "", "")
	host := fs.String("host", "", "")
	workers := fs.Int("workers", -1, "")
	gate := fs.Float64("gate", -1, "fail when worse than the baseline by more than PCT percent")
	baseline := fs.String("baseline", "", "")
	noWait := fs.Bool("no-wait", false, "")
	timeout := fs.Float64("timeout", 0, "")
	quiet := fs.Bool("quiet", false, "")
	report := fs.String("report", "", "")
	pos, err := parse(fs, args)
	if err != nil {
		return exitError, err
	}
	path, err := one("submit", pos)
	if err != nil {
		return exitError, err
	}
	labelMap := map[string]string{}
	for _, l := range labels {
		k, v, ok := strings.Cut(l, "=")
		if !ok || k == "" {
			return exitError, fmt.Errorf("label '%s' must be key=value", l)
		}
		labelMap[k] = v
	}
	plan, err := loadPlan(path)
	if err != nil {
		return exitError, err
	}
	b, err := ballista.NewBundle(plan, labelMap)
	if err != nil {
		return exitError, err
	}
	for _, spec := range files {
		rel, p, ok := strings.Cut(spec, "=")
		if !ok || rel == "" {
			return exitError, fmt.Errorf("--file '%s' must be REL=PATH", spec)
		}
		if err := b.AddFile(rel, p); err != nil {
			return exitError, err
		}
	}
	o := &ballista.Overrides{RunTime: *runTime, Host: *host}
	if *users > 0 {
		o.Users = users
	}
	if *rate > 0 {
		o.SpawnRate = rate
	}
	if *iterations > 0 {
		o.Iterations = iterations
	}
	if *workers >= 0 {
		o.Workers = workers
	}
	if *tags != "" {
		o.Tags = strings.Split(*tags, ",")
	}
	opts := ballista.CreateRunOptions{Overrides: o, Labels: labelMap}
	if *gate >= 0 || *baseline != "" {
		g := &ballista.RegressionGate{Baseline: *baseline}
		if *gate >= 0 {
			g.MaxRegressionPct = gate
		}
		opts.Regression = g
	}
	info, err := c.UploadBundle(ctx, b)
	if err != nil {
		return exitError, err
	}
	run, err := c.CreateRun(ctx, info.ID, opts)
	if err != nil {
		return exitError, err
	}
	fmt.Printf("queued run %s (bundle %s)\n", run.ID, info.ID)
	if *noWait {
		return 0, nil
	}
	w := ballista.WaitOptions{Timeout: time.Duration(*timeout * float64(time.Second))}
	if !*quiet {
		w.OnStats = progress()
	}
	run, err = c.Wait(ctx, run.ID, w)
	if err != nil {
		return exitError, err
	}
	summary(run)
	if *report != "" && run.Summary != nil {
		if err := os.MkdirAll(*report, 0o755); err != nil {
			return exitError, err
		}
		for _, name := range []string{"report.json", "report.html"} {
			if _, err := c.Download(ctx, run.ID, name, *report); err != nil {
				return exitError, err
			}
		}
	}
	return exitCode(run), nil
}

func run(args []string) (int, error) {
	global := flag.NewFlagSet("ballista-client", flag.ContinueOnError)
	urlFlag := global.String("url", "", "server url (BALLISTA_URL)")
	global.Usage = func() { fmt.Fprintln(os.Stderr, usage) }
	if err := global.Parse(args); err != nil {
		if errors.Is(err, flag.ErrHelp) {
			return 0, nil
		}
		return exitError, err
	}
	rest := global.Args()
	if len(rest) == 0 {
		fmt.Fprintln(os.Stderr, usage)
		return exitError, nil
	}
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	c, err := ballista.New(ballista.Options{URL: *urlFlag})
	if err != nil {
		return exitError, err
	}
	cmd, args := rest[0], rest[1:]
	switch cmd {
	case "submit":
		return submit(ctx, c, args)
	case "status", "stop", "wait":
		id, err := one(cmd, args)
		if err != nil {
			return exitError, err
		}
		var r *ballista.Run
		switch cmd {
		case "status":
			r, err = c.GetRun(ctx, id)
		case "stop":
			r, err = c.StopRun(ctx, id)
		default:
			r, err = c.Wait(ctx, id, ballista.WaitOptions{})
		}
		if err != nil {
			return exitError, err
		}
		summary(r)
		if cmd == "wait" {
			return exitCode(r), nil
		}
		return 0, nil
	case "compare":
		fs := flag.NewFlagSet("compare", flag.ContinueOnError)
		baseline := fs.String("baseline", "", "")
		pct := fs.Float64("max-regression-pct", -1, "")
		pos, err := parse(fs, args)
		if err != nil {
			return exitError, err
		}
		id, err := one(cmd, pos)
		if err != nil {
			return exitError, err
		}
		opts := ballista.CompareOptions{Baseline: *baseline}
		if *pct >= 0 {
			opts.MaxRegressionPct = pct
		}
		cmp, err := c.Compare(ctx, id, opts)
		if err != nil {
			return exitError, err
		}
		comparison(cmp)
		if !cmp.Passed {
			return exitFailed, nil
		}
		return 0, nil
	case "report":
		fs := flag.NewFlagSet("report", flag.ContinueOnError)
		name := fs.String("name", "report.html", "")
		out := fs.String("out", ".", "")
		pos, err := parse(fs, args)
		if err != nil {
			return exitError, err
		}
		id, err := one(cmd, pos)
		if err != nil {
			return exitError, err
		}
		if !slices.Contains(ballista.Artifacts, *name) {
			return exitError, fmt.Errorf("name must be one of %s", strings.Join(ballista.Artifacts, ", "))
		}
		path, err := c.Download(ctx, id, *name, *out)
		if err != nil {
			return exitError, err
		}
		fmt.Println(path)
		return 0, nil
	case "list":
		page, err := c.ListRuns(ctx, ballista.ListOptions{Limit: 50})
		if err != nil {
			return exitError, err
		}
		enc := json.NewEncoder(os.Stdout)
		for _, r := range page.Items {
			_ = enc.Encode(map[string]string{"id": r.ID, "state": string(r.State), "plan": r.PlanName, "created_at": r.CreatedAt})
		}
		return 0, nil
	}
	fmt.Fprintln(os.Stderr, usage)
	return exitError, nil
}

func main() {
	code, err := run(os.Args[1:])
	if err != nil {
		var timeout *ballista.RunTimeoutError
		if errors.As(err, &timeout) {
			code = exitTimeout
		}
		if !errors.Is(err, flag.ErrHelp) {
			fmt.Fprintf(os.Stderr, "error: %v\n", err)
		}
	}
	os.Exit(code)
}
