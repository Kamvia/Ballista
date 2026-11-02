// File: ballista/sdks/go/plan.go
package ballista

import (
	"encoding/json"
	"fmt"
	"maps"
	"math"
	"regexp"
)

var identRE = regexp.MustCompile(`^[A-Za-z_][A-Za-z0-9_]{0,63}$`)

// Target describes the system under test.
type Target struct {
	BaseURL             string            `json:"base_url"`
	AllowedHosts        []string          `json:"allowed_hosts,omitempty"`
	Timeout             string            `json:"timeout,omitempty"`
	ConnectTimeout      string            `json:"connect_timeout,omitempty"`
	Headers             map[string]string `json:"headers,omitempty"`
	Cookies             bool              `json:"cookies,omitempty"`
	Retries             *int              `json:"retries,omitempty"`
	RetryBackoff        string            `json:"retry_backoff,omitempty"`
	RetryOn             []int             `json:"retry_on,omitempty"`
	MaxBodyBytes        int64             `json:"max_body_bytes,omitempty"`
	HTTP2PriorKnowledge bool              `json:"http2_prior_knowledge,omitempty"`
	InsecureTLS         bool              `json:"insecure_tls,omitempty"`
	CACert              string            `json:"ca_cert,omitempty"`
}

// Stage ramps to Target (users, or iterations per second) over Duration.
type Stage struct {
	Duration string  `json:"duration"`
	Target   float64 `json:"target"`
}

// S is shorthand for a Stage.
func S(duration string, target float64) Stage { return Stage{Duration: duration, Target: target} }

// LoadMode selects a closed (users) or open (arrival rate) workload model.
type LoadMode string

const (
	ModeUsers       LoadMode = "users"
	ModeArrivalRate LoadMode = "arrival_rate"
)

// Load is the load profile: stages, or an inline shape script.
type Load struct {
	Mode         LoadMode `json:"mode"`
	Stages       []Stage  `json:"stages,omitempty"`
	ShapeSource  string   `json:"shape_source,omitempty"`
	GracefulStop string   `json:"graceful_stop,omitempty"`
	MaxUsers     *float64 `json:"max_users,omitempty"`
	Iterations   *uint64  `json:"iterations,omitempty"`
	Tags         []string `json:"tags,omitempty"`
	ExcludeTags  []string `json:"exclude_tags,omitempty"`
}

// Users is a closed model: each stage ramps to that many concurrent users.
func Users(stages ...Stage) Load { return Load{Mode: ModeUsers, Stages: stages} }

// ArrivalRate is an open model: each stage ramps to that many iterations per second.
func ArrivalRate(stages ...Stage) Load { return Load{Mode: ModeArrivalRate, Stages: stages} }

// Shape is a custom profile from a script defining `fn shape(t)`; the source travels inside the plan.
func Shape(source string, mode LoadMode) Load { return Load{Mode: mode, ShapeSource: source} }

// Wait is the pause between iterations of a scenario.
type Wait map[string]any

func WaitUniform(low, high string) Wait { return Wait{"between": []string{low, high}} }
func WaitFixed(d string) Wait           { return Wait{"constant": d} }
func WaitPace(d string) Wait            { return Wait{"pacing": d} }
func WaitPerSecond(rate float64) Wait   { return Wait{"throughput": rate} }
func WaitExponential(mean string) Wait  { return Wait{"poisson": mean} }

func (w Wait) check(where string) error {
	if w == nil {
		return nil
	}
	if len(w) != 1 {
		return configErr("%s: wait needs exactly one kind", where)
	}
	for k, v := range w {
		switch k {
		case "between":
			pair, ok := v.([]string)
			if !ok || len(pair) != 2 {
				return configErr("%s: wait between needs two durations", where)
			}
			for _, d := range pair {
				if err := checkDuration(d, where); err != nil {
					return err
				}
			}
		case "constant", "pacing", "poisson":
			d, ok := v.(string)
			if !ok {
				return configErr("%s: wait %s needs a duration", where, k)
			}
			return checkDuration(d, where)
		case "throughput":
			r, ok := v.(float64)
			if !ok || math.IsNaN(r) || math.IsInf(r, 0) || r <= 0 {
				return configErr("%s: wait throughput must be positive", where)
			}
		default:
			return configErr("%s: unknown wait kind '%s'", where, k)
		}
	}
	return nil
}

// Step is one declarative action. Build it with Get, Post, Think, Feed, ... and the chained setters.
type Step struct {
	Get               string            `json:"get,omitempty"`
	Post              string            `json:"post,omitempty"`
	Put               string            `json:"put,omitempty"`
	Patch             string            `json:"patch,omitempty"`
	Delete            string            `json:"delete,omitempty"`
	Head              string            `json:"head,omitempty"`
	Name              string            `json:"name,omitempty"`
	Headers           map[string]string `json:"headers,omitempty"`
	Query             map[string]string `json:"query,omitempty"`
	JSON              any               `json:"json,omitempty"`
	Body              string            `json:"body,omitempty"`
	Form              map[string]string `json:"form,omitempty"`
	ExpectStatus      []int             `json:"expect,omitempty"`
	ContainsText      string            `json:"contains,omitempty"`
	Captures          map[string]string `json:"capture,omitempty"`
	Timeout           string            `json:"timeout,omitempty"`
	Retries           *int              `json:"retries,omitempty"`
	ContinueOnFailure bool              `json:"continue_on_failure,omitempty"`
	SSE               int               `json:"sse,omitempty"`
	Reconnects        int               `json:"reconnects,omitempty"`
	Context           map[string]string `json:"context,omitempty"`
	ThinkTime         any               `json:"think,omitempty"`
	FeedFrom          string            `json:"feed,omitempty"`
	Set               map[string]string `json:"set,omitempty"`
	TxnBegin          string            `json:"txn_begin,omitempty"`
	TxnEnd            string            `json:"txn_end,omitempty"`
	Batch             []Step            `json:"batch,omitempty"`
	Concurrency       int               `json:"concurrency,omitempty"`
}

func Get(path string) Step     { return Step{Get: path} }
func Post(path string) Step    { return Step{Post: path} }
func Put(path string) Step     { return Step{Put: path} }
func Patch(path string) Step   { return Step{Patch: path} }
func Del(path string) Step     { return Step{Delete: path} }
func HeadReq(path string) Step { return Step{Head: path} }

// Think pauses for a fixed duration, or a uniform random one when high is given.
func Think(low string, high ...string) Step {
	if len(high) > 0 {
		return Step{ThinkTime: []string{low, high[0]}}
	}
	return Step{ThinkTime: low}
}

// Feed takes the next row of a feeder into the user's variables.
func Feed(feeder string) Step { return Step{FeedFrom: feeder} }

// Assign sets variables from templates.
func Assign(vars map[string]string) Step { return Step{Set: maps.Clone(vars)} }

// BatchOf sends requests in parallel, like a browser loading a page.
func BatchOf(name string, concurrency int, requests ...Step) Step {
	return Step{Batch: requests, Name: name, Concurrency: concurrency}
}

// Transaction wraps steps so their total duration is recorded as one transaction.
func Transaction(name string, steps ...Step) []Step {
	out := make([]Step, 0, len(steps)+2)
	out = append(out, Step{TxnBegin: name})
	out = append(out, steps...)
	return append(out, Step{TxnEnd: name})
}

// Steps concatenates single steps and step groups (such as transactions) in order.
func Steps(groups ...[]Step) []Step {
	var out []Step
	for _, g := range groups {
		out = append(out, g...)
	}
	return out
}

// One wraps a single step for Steps.
func One(steps ...Step) []Step { return steps }

func with(m map[string]string, k, v string) map[string]string {
	out := maps.Clone(m)
	if out == nil {
		out = map[string]string{}
	}
	out[k] = v
	return out
}

func (s Step) Named(n string) Step       { s.Name = n; return s }
func (s Step) Header(k, v string) Step   { s.Headers = with(s.Headers, k, v); return s }
func (s Step) Param(k, v string) Step    { s.Query = with(s.Query, k, v); return s }
func (s Step) WithJSON(v any) Step       { s.JSON = v; return s }
func (s Step) WithBody(b string) Step    { s.Body = b; return s }
func (s Step) Field(k, v string) Step    { s.Form = with(s.Form, k, v); return s }
func (s Step) Expect(codes ...int) Step  { s.ExpectStatus = append([]int(nil), codes...); return s }
func (s Step) Contains(text string) Step { s.ContainsText = text; return s }
func (s Step) Capture(variable, expr string) Step {
	s.Captures = with(s.Captures, variable, expr)
	return s
}
func (s Step) WithTimeout(d string) Step { s.Timeout = d; return s }
func (s Step) WithRetries(n int) Step    { s.Retries = &n; return s }
func (s Step) KeepGoing() Step           { s.ContinueOnFailure = true; return s }
func (s Step) Events(n int) Step         { s.SSE = n; return s }
func (s Step) WithReconnects(n int) Step { s.Reconnects = n; return s }
func (s Step) Ctx(k, v string) Step      { s.Context = with(s.Context, k, v); return s }

func (s Step) check(where string) error {
	methods := 0
	for _, p := range []string{s.Get, s.Post, s.Put, s.Patch, s.Delete, s.Head} {
		if p != "" {
			methods++
		}
	}
	actions := min(methods, 1)
	if s.ThinkTime != nil {
		actions++
	}
	for _, x := range []string{s.FeedFrom, s.TxnBegin, s.TxnEnd} {
		if x != "" {
			actions++
		}
	}
	if s.Set != nil {
		actions++
	}
	if s.Batch != nil {
		actions++
	}
	if methods > 1 || actions != 1 {
		return configErr("%s: a step does exactly one thing (request, think, feed, set, transaction boundary or batch)", where)
	}
	for _, m := range []map[string]string{s.Captures, s.Set} {
		for k := range m {
			if !identRE.MatchString(k) {
				return configErr("%s: variable '%s' must be an identifier", where, k)
			}
		}
	}
	if s.Timeout != "" {
		if err := checkDuration(s.Timeout, where); err != nil {
			return err
		}
	}
	switch t := s.ThinkTime.(type) {
	case nil:
	case string:
		if err := checkDuration(t, where); err != nil {
			return err
		}
	case []string:
		if len(t) != 2 {
			return configErr("%s: think takes one or two durations", where)
		}
		for _, d := range t {
			if err := checkDuration(d, where); err != nil {
				return err
			}
		}
	default:
		return configErr("%s: think takes one or two durations", where)
	}
	for _, n := range []string{s.TxnBegin, s.TxnEnd} {
		if n != "" && !nameRE.MatchString(n) {
			return configErr("%s: transaction '%s' must match [A-Za-z0-9_.-]{1,64}", where, n)
		}
	}
	if s.Retries != nil && (*s.Retries < 0 || *s.Retries > 10) {
		return configErr("%s: retries must be 0..10", where)
	}
	if s.Batch != nil && (s.Concurrency < 1 || s.Concurrency > 64) {
		return configErr("%s: concurrency must be 1..64", where)
	}
	for i, b := range s.Batch {
		if err := b.check(fmt.Sprintf("%s batch[%d]", where, i)); err != nil {
			return err
		}
	}
	return nil
}

// Scenario is one user behaviour: declarative Steps, an inline script Source, or a Script file in the bundle.
type Scenario struct {
	Name       string   `json:"name"`
	Weight     int      `json:"weight,omitempty"`
	Source     string   `json:"source,omitempty"`
	Script     string   `json:"script,omitempty"`
	Wait       Wait     `json:"wait,omitempty"`
	FixedCount int      `json:"fixed_count,omitempty"`
	Tags       []string `json:"tags,omitempty"`
	OnStart    []Step   `json:"on_start,omitempty"`
	Steps      []Step   `json:"step,omitempty"`
}

// Metric is a threshold metric.
type Metric string

const (
	MetricP50               Metric = "p50"
	MetricP90               Metric = "p90"
	MetricP95               Metric = "p95"
	MetricP99               Metric = "p99"
	MetricAvg               Metric = "avg"
	MetricMax               Metric = "max"
	MetricErrorRate         Metric = "error_rate"
	MetricRPS               Metric = "rps"
	MetricDroppedIterations Metric = "dropped_iterations"
)

// Threshold is a pass/fail criterion on a metric; Request "" means every request.
type Threshold struct {
	Metric      Metric   `json:"metric"`
	Request     string   `json:"request,omitempty"`
	Max         *float64 `json:"max,omitempty"`
	Min         *float64 `json:"min,omitempty"`
	AbortOnFail bool     `json:"abort_on_fail,omitempty"`
	AbortDelay  string   `json:"abort_delay,omitempty"`
}

// FeederMode selects how users take rows.
type FeederMode string

const (
	FeedSequential FeederMode = "sequential"
	FeedRandom     FeederMode = "random"
)

// Feeder is test data: Rows inline ([]map[string]any, or [][]any with Columns) or Path to a bundle asset.
type Feeder struct {
	Name    string     `json:"name"`
	Rows    any        `json:"rows,omitempty"`
	Columns []string   `json:"columns,omitempty"`
	Path    string     `json:"path,omitempty"`
	Mode    FeederMode `json:"mode,omitempty"`
}

func (f *Feeder) check() error {
	if !nameRE.MatchString(f.Name) {
		return configErr("feeder: '%s' must match [A-Za-z0-9_.-]{1,64}", f.Name)
	}
	n := -1
	switch r := f.Rows.(type) {
	case nil:
	case []map[string]any:
		n = len(r)
		if f.Columns != nil {
			return configErr("feeder '%s': columns only apply to array rows", f.Name)
		}
	case [][]any:
		n = len(r)
		if len(f.Columns) == 0 {
			return configErr("feeder '%s': array rows need columns", f.Name)
		}
	default:
		return configErr("feeder '%s': rows must be []map[string]any or [][]any", f.Name)
	}
	if (n >= 0) == (f.Path != "") {
		return configErr("feeder '%s' needs either rows or path", f.Name)
	}
	if n == 0 {
		return configErr("feeder '%s' has no rows", f.Name)
	}
	if f.Columns != nil && n < 0 {
		return configErr("feeder columns require rows")
	}
	if f.Mode != "" && f.Mode != FeedSequential && f.Mode != FeedRandom {
		return configErr("feeder '%s': mode must be sequential or random", f.Name)
	}
	return nil
}

// Plan is a structured test plan; Document returns the form the server accepts.
type Plan struct {
	Name        string
	Target      Target
	Load        Load
	Scenarios   []Scenario
	Thresholds  []Threshold
	Feeders     []Feeder
	Percentiles []float64
}

func checkDuration(d, what string) error {
	if !durationRE.MatchString(d) {
		return configErr("%s: '%s' is not a duration such as '500ms' or '2m'", what, d)
	}
	return nil
}

// Validate checks the plan with the same rules the server applies to structure.
func (p *Plan) Validate() error {
	if p.Name == "" || len(p.Name) > 128 {
		return configErr("plan name must be 1..128 characters")
	}
	if !validBaseURL(p.Target.BaseURL) {
		return configErr("target.base_url must be an http(s) URL without credentials")
	}
	for _, d := range [][2]string{{"timeout", p.Target.Timeout}, {"connect_timeout", p.Target.ConnectTimeout}, {"retry_backoff", p.Target.RetryBackoff}} {
		if d[1] != "" {
			if err := checkDuration(d[1], "target."+d[0]); err != nil {
				return err
			}
		}
	}
	if p.Load.Mode != "" && p.Load.Mode != ModeUsers && p.Load.Mode != ModeArrivalRate {
		return configErr("load.mode must be users or arrival_rate")
	}
	if (len(p.Load.Stages) > 0) == (p.Load.ShapeSource != "") {
		return configErr("load needs either stages or shape_source")
	}
	for _, st := range p.Load.Stages {
		if err := checkDuration(st.Duration, "load.stages"); err != nil {
			return err
		}
		if math.IsNaN(st.Target) || math.IsInf(st.Target, 0) || st.Target < 0 {
			return configErr("stage targets must be non-negative")
		}
	}
	if p.Load.GracefulStop != "" {
		if err := checkDuration(p.Load.GracefulStop, "load.graceful_stop"); err != nil {
			return err
		}
	}
	if len(p.Scenarios) == 0 {
		return configErr("plan requires at least one scenario")
	}
	seen := map[string]bool{}
	for i := range p.Scenarios {
		sc := &p.Scenarios[i]
		if !nameRE.MatchString(sc.Name) {
			return configErr("scenario: '%s' must match [A-Za-z0-9_.-]{1,64}", sc.Name)
		}
		if seen[sc.Name] {
			return configErr("duplicate scenario '%s'", sc.Name)
		}
		seen[sc.Name] = true
		kinds := 0
		for _, set := range []bool{sc.Source != "", sc.Script != "", len(sc.Steps) > 0} {
			if set {
				kinds++
			}
		}
		if kinds != 1 {
			return configErr("scenario '%s' needs exactly one of source (inline script), script (bundle asset) or steps", sc.Name)
		}
		if sc.Weight != 0 && (sc.Weight < 1 || sc.Weight > 10_000) {
			return configErr("scenario '%s': weight must be 1..10000", sc.Name)
		}
		if !validNames(sc.Tags, 64) {
			return configErr("scenario '%s': tags must be simple names", sc.Name)
		}
		if err := sc.Wait.check("scenario '" + sc.Name + "'"); err != nil {
			return err
		}
		for _, part := range []struct {
			label string
			list  []Step
		}{{"on_start ", sc.OnStart}, {"", sc.Steps}} {
			for j, st := range part.list {
				where := fmt.Sprintf("scenario '%s' %sstep %d", sc.Name, part.label, j+1)
				if err := st.check(where); err != nil {
					return err
				}
			}
		}
	}
	for _, t := range p.Thresholds {
		if t.Max == nil && t.Min == nil {
			return configErr("threshold %s needs min or max", t.Metric)
		}
		if t.AbortDelay != "" {
			if err := checkDuration(t.AbortDelay, "threshold.abort_delay"); err != nil {
				return err
			}
		}
	}
	for i := range p.Feeders {
		if err := p.Feeders[i].check(); err != nil {
			return err
		}
	}
	if len(p.Percentiles) > 12 {
		return configErr("percentiles: up to 12 values in (0, 100]")
	}
	for _, v := range p.Percentiles {
		if !(v > 0 && v <= 100) {
			return configErr("percentiles: up to 12 values in (0, 100]")
		}
	}
	return nil
}

type planReport struct {
	Percentiles []float64 `json:"percentiles"`
}

type planDocument struct {
	Name      string      `json:"name"`
	Target    Target      `json:"target"`
	Load      Load        `json:"load"`
	Scenario  []Scenario  `json:"scenario"`
	Feeder    []Feeder    `json:"feeder,omitempty"`
	Threshold []Threshold `json:"threshold,omitempty"`
	Report    *planReport `json:"report,omitempty"`
}

// Document validates the plan and returns the structured document sent to the server.
func (p *Plan) Document() (map[string]any, error) {
	if err := p.Validate(); err != nil {
		return nil, err
	}
	doc := planDocument{Name: p.Name, Target: p.Target, Load: p.Load, Scenario: p.Scenarios}
	if doc.Load.Mode == "" {
		doc.Load.Mode = ModeUsers
	}
	for _, f := range p.Feeders {
		if f.Mode == "" {
			f.Mode = FeedSequential
		}
		doc.Feeder = append(doc.Feeder, f)
	}
	for _, t := range p.Thresholds {
		if t.Request == "" {
			t.Request = "*"
		}
		doc.Threshold = append(doc.Threshold, t)
	}
	if len(p.Percentiles) > 0 {
		doc.Report = &planReport{Percentiles: p.Percentiles}
	}
	raw, err := json.Marshal(doc)
	if err != nil {
		return nil, configErr("plan cannot be encoded: %v", err)
	}
	var out map[string]any
	if err := json.Unmarshal(raw, &out); err != nil {
		return nil, configErr("plan cannot be encoded: %v", err)
	}
	return out, nil
}
