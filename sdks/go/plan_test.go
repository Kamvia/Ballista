// File: ballista/sdks/go/plan_test.go
package ballista

import (
	"encoding/json"
	"strings"
	"testing"
)

func shopPlan(base string) *Plan {
	rows := []map[string]any{}
	for i := 1; i <= 5; i++ {
		rows = append(rows, map[string]any{"username": "user" + string(rune('0'+i)), "password": "secret-pass"})
	}
	return &Plan{
		Name:   "shop api",
		Target: Target{BaseURL: base, Headers: map[string]string{"accept": "application/json"}},
		Load:   Users(S("2s", 4), S("4s", 4)),
		Scenarios: []Scenario{{
			Name: "buyer",
			Wait: WaitUniform("50ms", "100ms"),
			OnStart: []Step{
				Feed("users"),
				Post("/login").WithJSON(map[string]string{"username": "${users.username}", "password": "${users.password}"}).Capture("token", "json.token"),
			},
			Steps: Steps(
				One(Get("/items").Named("list items").Capture("item_id", "json.items.1.id"), Think("20ms")),
				Transaction("purchase",
					Get("/items/${item_id}").Named("item").Contains("stock"),
					Post("/cart").Named("cart").Header("authorization", "Bearer ${token}").
						WithJSON(map[string]any{"item": "${item_id}", "qty": 1}).Expect(201, 503),
				),
				One(BatchOf("page", 2, Get("/items"), Get("/slow").Named("slow page"))),
			),
		}},
		Feeders:     []Feeder{{Name: "users", Rows: rows}},
		Thresholds:  []Threshold{{Metric: MetricP95, Request: "purchase", Max: Ptr(1000.0)}},
		Percentiles: []float64{75, 99.9},
	}
}

func TestPlanDocument(t *testing.T) {
	doc, err := shopPlan("http://127.0.0.1:8080/").Document()
	if err != nil {
		t.Fatal(err)
	}
	raw, _ := json.Marshal(doc)
	s := string(raw)
	for _, want := range []string{
		`"mode":"users"`, `"txn_begin":"purchase"`, `"expect":[201,503]`, `"between":["50ms","100ms"]`,
		`"feeder":[{"mode":"sequential","name":"users"`, `"threshold":[{"max":1000,"metric":"p95","request":"purchase"}]`,
		`"report":{"percentiles":[75,99.9]}`, `"concurrency":2`,
	} {
		if !strings.Contains(s, want) {
			t.Errorf("document lacks %s\n%s", want, s)
		}
	}
	if strings.Contains(s, `"continue_on_failure"`) || strings.Contains(s, `"cookies"`) {
		t.Error("false flags must be omitted")
	}
}

func TestPlanValidation(t *testing.T) {
	mutate := []func(p *Plan){
		func(p *Plan) { p.Name = "" },
		func(p *Plan) { p.Target.BaseURL = "javascript:alert(1)" },
		func(p *Plan) { p.Target.Timeout = "forever" },
		func(p *Plan) { p.Load = Load{} },
		func(p *Plan) { p.Scenarios[0].Source = "fn run(s, r) {}" },
		func(p *Plan) { p.Scenarios = append(p.Scenarios, p.Scenarios[0]) },
		func(p *Plan) { p.Scenarios[0].Steps[0] = Step{Get: "/a", Post: "/b"} },
		func(p *Plan) { p.Scenarios[0].Steps[0] = Get("/a").Capture("1bad", "json.x") },
		func(p *Plan) { p.Scenarios[0].Steps[0] = Get("/a").WithRetries(11) },
		func(p *Plan) { p.Scenarios[0].Wait = WaitFixed("soon") },
		func(p *Plan) { p.Feeders[0].Rows = nil },
		func(p *Plan) { p.Feeders[0].Rows = [][]any{{1}} },
		func(p *Plan) { p.Thresholds[0].Max = nil },
		func(p *Plan) { p.Percentiles = []float64{0} },
	}
	for i, m := range mutate {
		p := shopPlan("http://127.0.0.1:8080/")
		m(p)
		if _, err := p.Document(); err == nil {
			t.Errorf("mutation %d accepted", i)
		}
	}
}

func TestStepSettersDoNotAlias(t *testing.T) {
	base := Get("/x").Header("a", "1")
	_ = base.Header("b", "2")
	if len(base.Headers) != 1 {
		t.Fatal("setter mutated the original step")
	}
}
