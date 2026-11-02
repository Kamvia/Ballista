// File: ballista/sdks/go/models.go
package ballista

import (
	"math"
	"net/url"
	"regexp"
	"strings"
)

var (
	nameRE     = regexp.MustCompile(`^[A-Za-z0-9_.-]{1,64}$`)
	bundleIDRE = regexp.MustCompile(`^[0-9a-f]{32}$`)
	runIDRE    = regexp.MustCompile(`^r[0-9a-f]{24}$`)
	durationRE = regexp.MustCompile(`^\s*\d+(\.\d+)?\s*(ns|us|ms|s|sec|m|min|h|hr|d)(\s*\d+(\.\d+)?\s*(ns|us|ms|s|sec|m|min|h|hr|d))*\s*$`)
	keyRE      = regexp.MustCompile(`^[\x21-\x7e]{1,128}$`)
)

// Ptr returns a pointer to v, for optional fields.
func Ptr[T any](v T) *T { return &v }

// RunState is the lifecycle state of a run.
type RunState string

const (
	StateQueued    RunState = "queued"
	StateRunning   RunState = "running"
	StateStopping  RunState = "stopping"
	StatePassed    RunState = "passed"
	StateFailed    RunState = "failed"
	StateStopped   RunState = "stopped"
	StateCancelled RunState = "cancelled"
	StateError     RunState = "error"
)

// Known reports whether s is a state defined by the API.
func (s RunState) Known() bool {
	switch s {
	case StateQueued, StateRunning, StateStopping, StatePassed, StateFailed, StateStopped, StateCancelled, StateError:
		return true
	}
	return false
}

// Terminal reports whether a run in this state has finished.
func (s RunState) Terminal() bool {
	switch s {
	case StatePassed, StateFailed, StateStopped, StateCancelled, StateError:
		return true
	}
	return false
}

type Health struct {
	Status  string `json:"status"`
	Version string `json:"version"`
}

type WhoAmI struct {
	Name  string `json:"name"`
	Scope string `json:"scope"`
}

type Limits struct {
	MaxUsers       float64 `json:"max_users"`
	MaxRunTimeSecs int64   `json:"max_run_time_secs"`
	MaxQueue       int     `json:"max_queue"`
}

type WorkerInfo struct {
	ID             uint64   `json:"id"`
	Name           string   `json:"name"`
	Address        string   `json:"address"`
	Cores          int      `json:"cores"`
	State          string   `json:"state"`
	Users          uint64   `json:"users"`
	Iterations     uint64   `json:"iterations"`
	Target         float64  `json:"target"`
	CPUPct         *float64 `json:"cpu_pct"`
	SchedulerLagMs float64  `json:"scheduler_lag_ms"`
	Saturated      bool     `json:"saturated"`
	LastReportSecs float64  `json:"last_report_secs"`
}

type ServerStatus struct {
	Version   string       `json:"version"`
	ActiveRun string       `json:"active_run"`
	Queued    int          `json:"queued"`
	Limits    Limits       `json:"limits"`
	Workers   []WorkerInfo `json:"workers"`
}

type BundleFile struct {
	Path   string `json:"path"`
	Size   int64  `json:"size"`
	SHA256 string `json:"sha256"`
}

type BundleInfo struct {
	ID        string            `json:"id"`
	PlanName  string            `json:"plan_name"`
	Scenarios []string          `json:"scenarios"`
	Files     []BundleFile      `json:"files"`
	Size      int64             `json:"size"`
	Labels    map[string]string `json:"labels"`
	CreatedAt string            `json:"created_at"`
	CreatedBy string            `json:"created_by"`
}

// Overrides adjust a stored plan for one run. Nil and empty fields keep the plan's value.
type Overrides struct {
	Users       *float64 `json:"users,omitempty"`
	SpawnRate   *float64 `json:"spawn_rate,omitempty"`
	RunTime     string   `json:"run_time,omitempty"`
	Iterations  *uint64  `json:"iterations,omitempty"`
	Tags        []string `json:"tags,omitempty"`
	ExcludeTags []string `json:"exclude_tags,omitempty"`
	Host        string   `json:"host,omitempty"`
	// Workers selects pool workers: 0 runs on the server only; nil uses every available worker.
	Workers *int `json:"workers,omitempty"`
}

func positive(p *float64) bool {
	return p == nil || (!math.IsNaN(*p) && !math.IsInf(*p, 0) && *p > 0)
}

func validBaseURL(raw string) bool {
	if strings.ContainsAny(raw, " \t\r\n") {
		return false
	}
	u, err := url.Parse(raw)
	return err == nil && (u.Scheme == "http" || u.Scheme == "https") && u.Hostname() != "" && u.User == nil
}

func validNames(list []string, max int) bool {
	if len(list) > max {
		return false
	}
	for _, t := range list {
		if !nameRE.MatchString(t) {
			return false
		}
	}
	return true
}

// Validate applies the same rules as the server before anything is sent.
func (o *Overrides) Validate() error {
	if o == nil {
		return nil
	}
	if !positive(o.Users) {
		return configErr("users must be positive")
	}
	if !positive(o.SpawnRate) {
		return configErr("spawn_rate must be positive")
	}
	if o.Iterations != nil && *o.Iterations < 1 {
		return configErr("iterations must be a positive integer")
	}
	if o.Workers != nil && (*o.Workers < 0 || *o.Workers > 512) {
		return configErr("workers must be an integer 0..512")
	}
	if o.RunTime != "" && !durationRE.MatchString(o.RunTime) {
		return configErr("run_time must be a duration such as '90s'")
	}
	if !validNames(o.Tags, 64) || !validNames(o.ExcludeTags, 64) {
		return configErr("tags must be up to 64 simple names")
	}
	if o.Host != "" && !validBaseURL(o.Host) {
		return configErr("host must be an http(s) base URL without credentials")
	}
	return nil
}

// CompareMetric is a metric the regression gate can compare.
type CompareMetric string

const (
	CompareP50       CompareMetric = "p50"
	CompareP95       CompareMetric = "p95"
	CompareP99       CompareMetric = "p99"
	CompareAvg       CompareMetric = "avg"
	CompareErrorRate CompareMetric = "error_rate"
	CompareRPS       CompareMetric = "rps"
)

func (m CompareMetric) known() bool {
	switch m {
	case CompareP50, CompareP95, CompareP99, CompareAvg, CompareErrorRate, CompareRPS:
		return true
	}
	return false
}

// RegressionGate fails a run that is worse than a baseline run beyond these limits.
// Nil fields use the server defaults (auto baseline, 10%, p95/p99/error_rate, 30 samples, 1 ms, 0.01).
type RegressionGate struct {
	// Baseline is "auto" (latest passed run of the same plan) or a run id.
	Baseline             string          `json:"baseline,omitempty"`
	MaxRegressionPct     *float64        `json:"max_regression_pct,omitempty"`
	Metrics              []CompareMetric `json:"metrics,omitempty"`
	MinSamples           *uint64         `json:"min_samples,omitempty"`
	MinChangeMs          *float64        `json:"min_change_ms,omitempty"`
	MaxErrorRateIncrease *float64        `json:"max_error_rate_increase,omitempty"`
}

func nonNegative(p *float64, max float64) bool {
	return p == nil || (!math.IsNaN(*p) && !math.IsInf(*p, 0) && *p >= 0 && *p <= max)
}

func (g *RegressionGate) Validate() error {
	if g == nil {
		return nil
	}
	if g.Baseline != "" && g.Baseline != "auto" && !runIDRE.MatchString(g.Baseline) {
		return configErr("baseline must be 'auto' or a run id")
	}
	if !nonNegative(g.MaxRegressionPct, 10_000) {
		return configErr("max_regression_pct must be within 0..10000")
	}
	if !nonNegative(g.MinChangeMs, math.MaxFloat64) || !nonNegative(g.MaxErrorRateIncrease, 1) {
		return configErr("min_change_ms and max_error_rate_increase must be non-negative (error rate at most 1)")
	}
	return checkMetrics(g.Metrics, true)
}

func checkMetrics(m []CompareMetric, optional bool) error {
	if len(m) == 0 && optional {
		return nil
	}
	if len(m) == 0 || len(m) > 6 {
		return configErr("metrics must list 1..6 metrics")
	}
	for _, x := range m {
		if !x.known() {
			return configErr("unknown metric '%s'", x)
		}
	}
	return nil
}

type MetricChange struct {
	Kind      string        `json:"kind"`
	Method    string        `json:"method"`
	Name      string        `json:"name"`
	Metric    CompareMetric `json:"metric"`
	Baseline  float64       `json:"baseline"`
	Current   float64       `json:"current"`
	ChangePct *float64      `json:"change_pct"`
	Regressed bool          `json:"regressed"`
}

type Comparison struct {
	Baseline    string         `json:"baseline"`
	Passed      bool           `json:"passed"`
	Regressions int            `json:"regressions"`
	Changes     []MetricChange `json:"changes"`
	Unmatched   []string       `json:"unmatched"`
	Note        string         `json:"note"`
}

type ThresholdOutcome struct {
	Metric  string   `json:"metric"`
	Request string   `json:"request"`
	Min     *float64 `json:"min"`
	Max     *float64 `json:"max"`
	Actual  *float64 `json:"actual"`
	Passed  bool     `json:"passed"`
}

type Summary struct {
	Requests               uint64             `json:"requests"`
	Failures               uint64             `json:"failures"`
	FailRatio              float64            `json:"fail_ratio"`
	RPS                    float64            `json:"rps"`
	AvgMs                  float64            `json:"avg_ms"`
	P50Ms                  float64            `json:"p50_ms"`
	P95Ms                  float64            `json:"p95_ms"`
	P99Ms                  float64            `json:"p99_ms"`
	MaxMs                  float64            `json:"max_ms"`
	Iterations             uint64             `json:"iterations"`
	DurationSecs           float64            `json:"duration_secs"`
	Aborted                bool               `json:"aborted"`
	GeneratorSaturatedSecs float64            `json:"generator_saturated_secs"`
	Thresholds             []ThresholdOutcome `json:"thresholds"`
}

type Run struct {
	ID             string            `json:"id"`
	Seq            uint64            `json:"seq"`
	Bundle         string            `json:"bundle"`
	PlanName       string            `json:"plan_name"`
	State          RunState          `json:"state"`
	CreatedAt      string            `json:"created_at"`
	CreatedBy      string            `json:"created_by"`
	StartedAt      string            `json:"started_at"`
	EndedAt        string            `json:"ended_at"`
	Labels         map[string]string `json:"labels"`
	Overrides      Overrides         `json:"overrides"`
	Passed         *bool             `json:"passed"`
	Error          string            `json:"error"`
	Summary        *Summary          `json:"summary"`
	StoppedBy      string            `json:"stopped_by"`
	Workers        *int              `json:"workers"`
	Regression     *RegressionGate   `json:"regression"`
	Comparison     *Comparison       `json:"comparison"`
	IdempotencyKey string            `json:"idempotency_key"`
	QueuePosition  *int              `json:"queue_position"`
}

func (r *Run) check() error {
	if !runIDRE.MatchString(r.ID) {
		return &ResponseError{What: "run.id is malformed"}
	}
	if !r.State.Known() {
		return &ResponseError{What: "run.state is unknown"}
	}
	if r.Labels == nil {
		r.Labels = map[string]string{}
	}
	return nil
}

type StatsRow struct {
	Scenario    string    `json:"scenario"`
	Method      string    `json:"method"`
	Name        string    `json:"name"`
	Count       uint64    `json:"count"`
	Failures    uint64    `json:"failures"`
	FailRatio   float64   `json:"fail_ratio"`
	RPS         float64   `json:"rps"`
	FailRPS     float64   `json:"fail_rps"`
	RPSAvg      float64   `json:"rps_avg"`
	AvgMs       float64   `json:"avg_ms"`
	MinMs       float64   `json:"min_ms"`
	MaxMs       float64   `json:"max_ms"`
	P50Ms       float64   `json:"p50_ms"`
	P90Ms       float64   `json:"p90_ms"`
	P95Ms       float64   `json:"p95_ms"`
	P99Ms       float64   `json:"p99_ms"`
	P999Ms      float64   `json:"p999_ms"`
	AvgBytes    float64   `json:"avg_bytes"`
	Percentiles []float64 `json:"percentiles"`
}

type Bucket struct {
	UpperMs *float64 `json:"upper_ms"`
	Count   uint64   `json:"count"`
}

type Snapshot struct {
	Plan              string           `json:"plan"`
	State             string           `json:"state"`
	ElapsedSecs       float64          `json:"elapsed_secs"`
	Users             uint64           `json:"users"`
	Target            float64          `json:"target"`
	Total             StatsRow         `json:"total"`
	Rows              []StatsRow       `json:"rows"`
	Transactions      []StatsRow       `json:"transactions"`
	Iterations        []StatsRow       `json:"iterations"`
	Errors            []map[string]any `json:"errors"`
	Thresholds        []map[string]any `json:"thresholds"`
	Distribution      []Bucket         `json:"distribution"`
	IterationsStarted uint64           `json:"iterations_started"`
	DroppedIterations uint64           `json:"dropped_iterations"`
	Aborted           bool             `json:"aborted"`
}

// FindRow returns the request or transaction row with this name (and method, when not empty).
func (s *Snapshot) FindRow(name, method string) *StatsRow {
	for _, list := range [][]StatsRow{s.Rows, s.Transactions} {
		for i := range list {
			if list[i].Name == name && (method == "" || list[i].Method == method) {
				return &list[i]
			}
		}
	}
	return nil
}

// PlanCheck is the server-side validation result: files a bundle must still carry and the normalized plan.
type PlanCheck struct {
	Name      string         `json:"name"`
	Scenarios []string       `json:"scenarios"`
	Files     []string       `json:"files"`
	Document  map[string]any `json:"document"`
}

type Page[T any] struct {
	Items      []T    `json:"items"`
	NextCursor string `json:"next_cursor"`
}

// EventType names a run event on the event stream.
type EventType string

const (
	EventRun   EventType = "run"
	EventStats EventType = "stats"
	EventEnd   EventType = "end"
)

// Event is one message of a run's event stream; Run is set for run and end, Stats for stats.
type Event struct {
	Type  EventType
	Run   *Run
	Stats *Snapshot
}

func checkLabels(labels map[string]string) error {
	if len(labels) > 32 {
		return configErr("at most 32 labels")
	}
	for k, v := range labels {
		if !nameRE.MatchString(k) {
			return configErr("label key '%s' must match [A-Za-z0-9_.-]{1,64}", k)
		}
		if len([]rune(v)) > 256 || strings.IndexFunc(v, func(r rune) bool { return r < 0x20 || r == 0x7f }) >= 0 {
			return configErr("label '%s' must be up to 256 printable characters", k)
		}
	}
	return nil
}
