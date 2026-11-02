// File: ballista/sdks/go/plan_rules_test.go
package ballista

import (
	"encoding/json"
	"math"
	"strings"
	"testing"
)

func basePlan() *Plan {
	return &Plan{
		Name:      "p",
		Target:    Target{BaseURL: "http://127.0.0.1:8080/"},
		Load:      Users(S("1s", 1)),
		Scenarios: []Scenario{{Name: "s", Steps: []Step{Get("/")}}},
	}
}

func docJSON(t *testing.T, p *Plan) string {
	t.Helper()
	doc, err := p.Document()
	if err != nil {
		t.Fatal(err)
	}
	raw, _ := json.Marshal(doc)
	return string(raw)
}

func TestLoadModels(t *testing.T) {
	p := basePlan()
	p.Load = ArrivalRate(S("10s", 50), S("1m", 50))
	p.Load.MaxUsers = Ptr(500.0)
	if s := docJSON(t, p); !strings.Contains(s, `"mode":"arrival_rate"`) || !strings.Contains(s, `"max_users":500`) {
		t.Fatal(s)
	}
	p.Load = Shape("fn shape(t) { 5.0 }", ModeUsers)
	p.Load.GracefulStop = "3s"
	if s := docJSON(t, p); !strings.Contains(s, `"shape_source":"fn shape(t) { 5.0 }"`) || strings.Contains(s, `"stages"`) {
		t.Fatal(s)
	}
	p.Load = Load{Stages: []Stage{S("1s", 1)}}
	if s := docJSON(t, p); !strings.Contains(s, `"mode":"users"`) {
		t.Fatal("default mode")
	}
	for _, l := range []Load{
		{Mode: "closed", Stages: []Stage{S("1s", 1)}},
		{Mode: ModeUsers},
		{Mode: ModeUsers, Stages: []Stage{S("1s", 1)}, ShapeSource: "fn shape(t) {}"},
		Users(S("1s", -1)), Users(S("1s", math.Inf(1))), Users(S("soon", 1)),
		{Mode: ModeUsers, Stages: []Stage{S("1s", 1)}, GracefulStop: "later"},
	} {
		p.Load = l
		if _, err := p.Document(); err == nil {
			t.Errorf("accepted load %+v", l)
		}
	}
}

func TestWaitKinds(t *testing.T) {
	for _, w := range []Wait{WaitUniform("1s", "2s"), WaitFixed("1s"), WaitPace("3s"), WaitPerSecond(2), WaitExponential("500ms")} {
		p := basePlan()
		p.Scenarios[0].Wait = w
		if _, err := p.Document(); err != nil {
			t.Errorf("%v: %v", w, err)
		}
	}
	for _, w := range []Wait{WaitUniform("1s", "x"), WaitPerSecond(0), WaitPerSecond(math.NaN()), {"jitter": "1s"}, {"constant": 5}, {"between": []string{"1s"}}, {"constant": "1s", "pacing": "1s"}} {
		p := basePlan()
		p.Scenarios[0].Wait = w
		if _, err := p.Document(); err == nil {
			t.Errorf("accepted wait %v", w)
		}
	}
}

func TestScenarioRules(t *testing.T) {
	cases := map[string]func(*Plan){
		"weight":         func(p *Plan) { p.Scenarios[0].Weight = 10_001 },
		"negative":       func(p *Plan) { p.Scenarios[0].Weight = -1 },
		"tag":            func(p *Plan) { p.Scenarios[0].Tags = []string{"no spaces"} },
		"script+steps":   func(p *Plan) { p.Scenarios[0].Script = "s.rhai" },
		"no scenarios":   func(p *Plan) { p.Scenarios = nil },
		"long name":      func(p *Plan) { p.Name = strings.Repeat("n", 129) },
		"bad on_start":   func(p *Plan) { p.Scenarios[0].OnStart = []Step{{}} },
		"think triple":   func(p *Plan) { p.Scenarios[0].Steps = []Step{{ThinkTime: []string{"1s", "2s", "3s"}}} },
		"think type":     func(p *Plan) { p.Scenarios[0].Steps = []Step{{ThinkTime: 5}} },
		"txn name":       func(p *Plan) { p.Scenarios[0].Steps = Transaction("bad name", Get("/")) },
		"step timeout":   func(p *Plan) { p.Scenarios[0].Steps = []Step{Get("/").WithTimeout("1 week")} },
		"set ident":      func(p *Plan) { p.Scenarios[0].Steps = []Step{Assign(map[string]string{"a-b": "x"})} },
		"batch inner":    func(p *Plan) { p.Scenarios[0].Steps = []Step{BatchOf("b", 2, Get("/"), Step{})} },
		"batch conc":     func(p *Plan) { p.Scenarios[0].Steps = []Step{BatchOf("b", 65, Get("/"))} },
		"abort delay":    func(p *Plan) { p.Thresholds = []Threshold{{Metric: MetricP95, Max: Ptr(1.0), AbortDelay: "x"}} },
		"timeout target": func(p *Plan) { p.Target.ConnectTimeout = "x" },
		"too many pcts":  func(p *Plan) { p.Percentiles = make([]float64, 13) },
		"pct over 100":   func(p *Plan) { p.Percentiles = []float64{100.1} },
	}
	for name, mutate := range cases {
		p := basePlan()
		mutate(p)
		if _, err := p.Document(); err == nil {
			t.Errorf("%s accepted", name)
		}
	}
}

func TestStepKinds(t *testing.T) {
	p := basePlan()
	p.Scenarios[0].Steps = Steps(
		One(Get("/a"), Post("/b").WithBody("raw").Field("f", "1"), Put("/c"), Patch("/d"), Del("/e"), HeadReq("/f")),
		One(Think("1s", "2s"), Feed("users"), Assign(map[string]string{"x": "${y}"})),
		One(Get("/s").Events(3).WithReconnects(2).KeepGoing().Param("q", "1").Ctx("region", "eu").WithRetries(0)),
		Transaction("t", Get("/g")),
	)
	p.Feeders = []Feeder{{Name: "users", Rows: [][]any{{"a", 1}, {"b", 2}}, Columns: []string{"name", "n"}, Mode: FeedRandom}}
	s := docJSON(t, p)
	for _, want := range []string{
		`"put":"/c"`, `"patch":"/d"`, `"delete":"/e"`, `"head":"/f"`, `"body":"raw"`, `"form":{"f":"1"}`,
		`"think":["1s","2s"]`, `"set":{"x":"${y}"}`, `"sse":3`, `"reconnects":2`, `"continue_on_failure":true`,
		`"query":{"q":"1"}`, `"context":{"region":"eu"}`, `"retries":0`, `"txn_end":"t"`,
		`"columns":["name","n"]`, `"mode":"random"`,
	} {
		if !strings.Contains(s, want) {
			t.Errorf("missing %s", want)
		}
	}
}

func TestFeederRules(t *testing.T) {
	for name, f := range map[string]Feeder{
		"bad name":     {Name: "a b", Path: "x.csv"},
		"both":         {Name: "f", Path: "x.csv", Rows: []map[string]any{{"a": 1}}},
		"neither":      {Name: "f"},
		"empty":        {Name: "f", Rows: []map[string]any{}},
		"no columns":   {Name: "f", Rows: [][]any{{1}}},
		"cols on maps": {Name: "f", Rows: []map[string]any{{"a": 1}}, Columns: []string{"a"}},
		"cols no rows": {Name: "f", Path: "x.csv", Columns: []string{"a"}},
		"wrong type":   {Name: "f", Rows: []string{"a"}},
		"mode":         {Name: "f", Path: "x.csv", Mode: "shuffled"},
	} {
		p := basePlan()
		p.Feeders = []Feeder{f}
		if _, err := p.Document(); err == nil {
			t.Errorf("%s accepted", name)
		}
	}
	p := basePlan()
	p.Feeders = []Feeder{{Name: "f", Path: "data/users.csv"}}
	if s := docJSON(t, p); !strings.Contains(s, `"path":"data/users.csv"`) || !strings.Contains(s, `"mode":"sequential"`) {
		t.Fatal(s)
	}
}

func TestTargetDocument(t *testing.T) {
	p := basePlan()
	p.Target = Target{BaseURL: "https://shop.example/", Timeout: "5s", Retries: Ptr(0), RetryOn: []int{502}, InsecureTLS: true, CACert: "certs/ca.pem", HTTP2PriorKnowledge: true}
	s := docJSON(t, p)
	for _, want := range []string{`"retries":0`, `"retry_on":[502]`, `"insecure_tls":true`, `"ca_cert":"certs/ca.pem"`, `"http2_prior_knowledge":true`} {
		if !strings.Contains(s, want) {
			t.Errorf("missing %s in %s", want, s)
		}
	}
	for _, u := range []string{"", "shop.example", "file:///etc/passwd", "https://a:b@shop.example/", "https:// shop.example"} {
		p.Target.BaseURL = u
		if _, err := p.Document(); err == nil {
			t.Errorf("base url %q accepted", u)
		}
	}
}

func TestDocumentDoesNotMutatePlan(t *testing.T) {
	p := basePlan()
	p.Thresholds = []Threshold{{Metric: MetricErrorRate, Max: Ptr(0.0)}}
	p.Feeders = []Feeder{{Name: "f", Path: "x.csv"}}
	if _, err := p.Document(); err != nil {
		t.Fatal(err)
	}
	if p.Thresholds[0].Request != "" || p.Feeders[0].Mode != "" || p.Load.Mode != ModeUsers {
		t.Fatalf("defaults leaked into the plan: %+v %+v", p.Thresholds, p.Feeders)
	}
	if s := docJSON(t, p); !strings.Contains(s, `"request":"*"`) || !strings.Contains(s, `"max":0`) {
		t.Fatal("zero threshold bound must be kept")
	}
}
