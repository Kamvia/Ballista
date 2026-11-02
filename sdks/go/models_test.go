// File: ballista/sdks/go/models_test.go
package ballista

import (
	"errors"
	"fmt"
	"math"
	"strings"
	"testing"
)

func TestRunStates(t *testing.T) {
	terminal := map[RunState]bool{
		StateQueued: false, StateRunning: false, StateStopping: false,
		StatePassed: true, StateFailed: true, StateStopped: true, StateCancelled: true, StateError: true,
	}
	for s, want := range terminal {
		if !s.Known() || s.Terminal() != want {
			t.Errorf("%s: known=%t terminal=%t", s, s.Known(), s.Terminal())
		}
	}
	if RunState("paused").Known() || RunState("paused").Terminal() {
		t.Fatal("unknown state")
	}
}

func TestOverridesValidation(t *testing.T) {
	valid := []Overrides{
		{},
		{Users: Ptr(1.0), SpawnRate: Ptr(0.5), RunTime: "1h 30m", Iterations: Ptr(uint64(1)), Workers: Ptr(0)},
		{Workers: Ptr(512), Tags: []string{"read", "a.b-c_d"}, Host: "https://shop.example.com/base"},
	}
	for _, o := range valid {
		if err := o.Validate(); err != nil {
			t.Errorf("%+v: %v", o, err)
		}
	}
	tags := make([]string, 65)
	for i := range tags {
		tags[i] = fmt.Sprintf("t%d", i)
	}
	invalid := []Overrides{
		{Users: Ptr(0.0)}, {Users: Ptr(math.NaN())}, {Users: Ptr(math.Inf(1))}, {SpawnRate: Ptr(-1.0)},
		{Iterations: Ptr(uint64(0))}, {Workers: Ptr(-1)}, {Workers: Ptr(513)},
		{RunTime: "10"}, {RunTime: "ten seconds"}, {RunTime: "5s; rm -rf"},
		{Tags: []string{"has space"}}, {ExcludeTags: []string{""}}, {Tags: tags},
		{Host: "ftp://x"}, {Host: "https://user:pw@x"}, {Host: "https://x/ y"}, {Host: "//x"},
	}
	for _, o := range invalid {
		var ce *ConfigurationError
		if err := o.Validate(); !errors.As(err, &ce) {
			t.Errorf("accepted %+v", o)
		}
	}
	var nilOverrides *Overrides
	if nilOverrides.Validate() != nil {
		t.Fatal("nil overrides")
	}
}

func TestRegressionGateValidation(t *testing.T) {
	valid := []RegressionGate{
		{}, {Baseline: "auto"}, {Baseline: testRunID, MaxRegressionPct: Ptr(0.0), MinChangeMs: Ptr(0.0), MaxErrorRateIncrease: Ptr(1.0)},
		{Metrics: []CompareMetric{CompareP50, CompareP95, CompareP99, CompareAvg, CompareErrorRate, CompareRPS}},
	}
	for _, g := range valid {
		if err := g.Validate(); err != nil {
			t.Errorf("%+v: %v", g, err)
		}
	}
	invalid := []RegressionGate{
		{Baseline: "latest"}, {Baseline: "r0123"}, {MaxRegressionPct: Ptr(-0.1)}, {MaxRegressionPct: Ptr(10_001.0)},
		{MaxRegressionPct: Ptr(math.NaN())}, {MinChangeMs: Ptr(-1.0)}, {MaxErrorRateIncrease: Ptr(1.5)},
		{Metrics: []CompareMetric{"p42"}}, {Metrics: make([]CompareMetric, 7)},
	}
	for _, g := range invalid {
		if err := g.Validate(); err == nil {
			t.Errorf("accepted %+v", g)
		}
	}
}

func TestLabelRules(t *testing.T) {
	ok := map[string]string{"build": strings.Repeat("ü", 256), "a.b-c_d": ""}
	if err := checkLabels(ok); err != nil {
		t.Fatal(err)
	}
	many := map[string]string{}
	for i := 0; i < 33; i++ {
		many[fmt.Sprintf("k%d", i)] = "v"
	}
	for _, bad := range []map[string]string{
		many, {"": "v"}, {"bad key": "v"}, {strings.Repeat("k", 65): "v"},
		{"k": strings.Repeat("v", 257)}, {"k": "line\nbreak"}, {"k": "tab\there"}, {"k": "del\x7f"},
	} {
		if checkLabels(bad) == nil {
			t.Errorf("accepted %d labels", len(bad))
		}
	}
}

func TestErrorTypes(t *testing.T) {
	inner := errors.New("dial tcp: refused")
	te := &TransportError{Op: "GET /x", Err: inner}
	if te.Error() != "GET /x: dial tcp: refused" || !errors.Is(te, inner) || (&TransportError{Op: "op"}).Error() != "op" {
		t.Fatal("transport error")
	}
	if (&APIError{Status: 404, Code: "not_found", Detail: "gone"}).Error() != "404 not_found: gone" {
		t.Fatal("api error")
	}
	if (&ResponseError{What: "x"}).Error() != "unexpected response: x" || (&ConfigurationError{Msg: "m"}).Error() != "m" {
		t.Fatal("error strings")
	}
	wrapped := fmt.Errorf("context: %w", &APIError{Status: 401})
	checks := map[string]bool{
		"unauthorized": IsUnauthorized(wrapped), "not forbidden": !IsForbidden(wrapped), "not found": !IsNotFound(inner),
		"not conflict": !IsConflict(nil), "not validation": !IsValidation(te), "not rate limited": !IsRateLimited(wrapped),
	}
	for name, ok := range checks {
		if !ok {
			t.Error(name)
		}
	}
}
