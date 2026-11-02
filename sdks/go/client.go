// File: ballista/sdks/go/client.go
package ballista

import (
	"bytes"
	"context"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"iter"
	mrand "math/rand/v2"
	"net"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"sync/atomic"
	"time"
	"unicode"
)

const (
	TokenEnv  = "BALLISTA_TOKEN"
	URLEnv    = "BALLISTA_URL"
	userAgent = "ballista-client-go/0.1.0"

	maxJSONBytes     = 32 << 20
	maxArtifactBytes = 512 << 20
	maxErrorBytes    = 64 << 10
)

// Artifacts are the files a finished run keeps.
var Artifacts = []string{"report.json", "report.html", "stats.csv", "history.csv", "failures.csv"}

// RetryPolicy controls retries of idempotent requests on connection errors and 429/502/503/504.
type RetryPolicy struct {
	Attempts  int
	BaseDelay time.Duration
	MaxDelay  time.Duration
}

// Options configure a Client. URL and Token fall back to BALLISTA_URL and BALLISTA_TOKEN.
type Options struct {
	URL   string
	Token string
	// Timeout bounds each request attempt (default 30s). Event streams use an idle timeout instead.
	Timeout time.Duration
	Retry   *RetryPolicy
	// AllowInsecureHTTP permits plain http to a non-loopback host (the token is sent in clear).
	AllowInsecureHTTP bool
	// CACertFile adds a PEM CA bundle to the system roots, for private or self-signed server certificates.
	CACertFile string
	// HTTPClient replaces the default client; redirects are never followed regardless.
	HTTPClient *http.Client
}

// Client talks to the run manager API. It is safe for concurrent use.
type Client struct {
	baseURL string
	token   string
	timeout time.Duration
	retry   RetryPolicy
	http    *http.Client
}

func isLoopback(host string) bool {
	if host == "localhost" {
		return true
	}
	ip := net.ParseIP(host)
	return ip != nil && ip.IsLoopback()
}

func parseBaseURL(raw string, allowInsecure bool) (string, error) {
	value := strings.TrimSpace(raw)
	if value == "" {
		value = strings.TrimSpace(os.Getenv(URLEnv))
	}
	if value == "" {
		return "", configErr("server url required (option or %s)", URLEnv)
	}
	u, err := url.Parse(value)
	if err != nil {
		return "", configErr("server url is not a valid URL")
	}
	if (u.Scheme != "http" && u.Scheme != "https") || u.User != nil || u.Hostname() == "" {
		return "", configErr("server url must be http(s)://host[:port] without credentials")
	}
	if u.Scheme == "http" && !isLoopback(u.Hostname()) && !allowInsecure {
		return "", configErr("refusing to send the token over plain http to a remote host; use https")
	}
	return u.Scheme + "://" + u.Host + "/api/v1", nil
}

func validToken(t string) bool {
	return len(t) >= 32 && len(t) <= 4096 && strings.IndexFunc(t, func(r rune) bool { return unicode.IsSpace(r) || r < 0x21 || r > 0x7e }) < 0
}

func noRedirect(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }

func defaultHTTPClient(caFile string) (*http.Client, error) {
	tr := http.DefaultTransport.(*http.Transport).Clone()
	tr.TLSClientConfig = &tls.Config{MinVersion: tls.VersionTLS12}
	if caFile != "" {
		pem, err := os.ReadFile(caFile)
		if err != nil {
			return nil, configErr("ca cert: %v", err)
		}
		pool, err := x509.SystemCertPool()
		if err != nil || pool == nil {
			pool = x509.NewCertPool()
		}
		if !pool.AppendCertsFromPEM(pem) {
			return nil, configErr("ca cert contains no certificates")
		}
		tr.TLSClientConfig.RootCAs = pool
	}
	return &http.Client{Transport: tr, CheckRedirect: noRedirect}, nil
}

// New creates a client after validating the URL and token.
func New(opts Options) (*Client, error) {
	base, err := parseBaseURL(opts.URL, opts.AllowInsecureHTTP)
	if err != nil {
		return nil, err
	}
	token := strings.TrimSpace(opts.Token)
	if token == "" {
		token = strings.TrimSpace(os.Getenv(TokenEnv))
	}
	if !validToken(token) {
		return nil, configErr("api token required (option or %s)", TokenEnv)
	}
	c := &Client{baseURL: base, token: token, timeout: opts.Timeout, retry: RetryPolicy{Attempts: 4, BaseDelay: 500 * time.Millisecond, MaxDelay: 20 * time.Second}}
	if c.timeout <= 0 {
		c.timeout = 30 * time.Second
	}
	if opts.Retry != nil {
		c.retry = *opts.Retry
		c.retry.Attempts = max(c.retry.Attempts, 1)
	}
	if opts.HTTPClient != nil {
		hc := *opts.HTTPClient
		hc.CheckRedirect = noRedirect
		c.http = &hc
	} else if c.http, err = defaultHTTPClient(opts.CACertFile); err != nil {
		return nil, err
	}
	return c, nil
}

// BaseURL is the API root, e.g. https://loadtest.example.com/api/v1.
func (c *Client) BaseURL() string { return c.baseURL }

func (c *Client) String() string   { return "Client(" + c.baseURL + ")" }
func (c *Client) GoString() string { return c.String() }

func (c *Client) delay(attempt int, after time.Duration) time.Duration {
	if after > 0 {
		return min(after, c.retry.MaxDelay)
	}
	backoff := min(c.retry.BaseDelay<<attempt, c.retry.MaxDelay)
	return time.Duration(float64(backoff) * (0.5 + mrand.Float64()/2))
}

func sleepCtx(ctx context.Context, d time.Duration) error {
	t := time.NewTimer(d)
	defer t.Stop()
	select {
	case <-ctx.Done():
		return ctx.Err()
	case <-t.C:
		return nil
	}
}

func retryAfter(h http.Header) time.Duration {
	n, err := strconv.Atoi(strings.TrimSpace(h.Get("Retry-After")))
	if err != nil || n < 0 {
		return 0
	}
	return time.Duration(n) * time.Second
}

func retryStatus(s int) bool {
	return s == http.StatusTooManyRequests || s == http.StatusBadGateway || s == http.StatusServiceUnavailable || s == http.StatusGatewayTimeout
}

func apiError(status int, h http.Header, body []byte) error {
	e := &APIError{Status: status, Code: "error", Detail: http.StatusText(status), RetryAfter: retryAfter(h)}
	var parsed struct {
		Error struct {
			Code    string `json:"code"`
			Message string `json:"message"`
		} `json:"error"`
	}
	if json.Unmarshal(body, &parsed) == nil {
		if parsed.Error.Code != "" {
			e.Code = parsed.Error.Code
		}
		if parsed.Error.Message != "" {
			e.Detail = parsed.Error.Message
		}
	}
	if r := []rune(e.Detail); len(r) > 1000 {
		e.Detail = string(r[:1000])
	}
	return e
}

type request struct {
	method  string
	path    string
	query   url.Values
	body    any
	headers map[string]string
	limit   int64
}

func (c *Client) headers(req *http.Request, accept string) {
	req.Header.Set("Authorization", "Bearer "+c.token)
	req.Header.Set("User-Agent", userAgent)
	req.Header.Set("Accept", accept)
}

// do sends a request with retries and returns the response body of a 2xx answer.
func (c *Client) do(ctx context.Context, r request) ([]byte, int, error) {
	var payload []byte
	if r.body != nil {
		var err error
		if payload, err = json.Marshal(r.body); err != nil {
			return nil, 0, configErr("request body cannot be encoded: %v", err)
		}
	}
	target := c.baseURL + r.path
	if len(r.query) > 0 {
		target += "?" + r.query.Encode()
	}
	limit := r.limit
	if limit == 0 {
		limit = maxJSONBytes
	}
	op := r.method + " " + r.path
	for attempt := 0; ; attempt++ {
		last := attempt+1 >= c.retry.Attempts
		body, status, h, err := c.attempt(ctx, r, target, payload, limit)
		if err != nil {
			if ctx.Err() != nil {
				return nil, 0, &TransportError{Op: op, Err: ctx.Err()}
			}
			if last {
				return nil, 0, &TransportError{Op: op, Err: err}
			}
			if serr := sleepCtx(ctx, c.delay(attempt, 0)); serr != nil {
				return nil, 0, &TransportError{Op: op, Err: serr}
			}
			continue
		}
		if retryStatus(status) && !last {
			if serr := sleepCtx(ctx, c.delay(attempt, retryAfter(h))); serr != nil {
				return nil, 0, &TransportError{Op: op, Err: serr}
			}
			continue
		}
		if status < 200 || status > 299 {
			return nil, status, apiError(status, h, body)
		}
		return body, status, nil
	}
}

func (c *Client) attempt(ctx context.Context, r request, target string, payload []byte, limit int64) ([]byte, int, http.Header, error) {
	actx, cancel := context.WithTimeout(ctx, c.timeout)
	defer cancel()
	var rd io.Reader
	if payload != nil {
		rd = bytes.NewReader(payload)
	}
	req, err := http.NewRequestWithContext(actx, r.method, target, rd)
	if err != nil {
		return nil, 0, nil, err
	}
	c.headers(req, "application/json")
	if payload != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	for k, v := range r.headers {
		req.Header.Set(k, v)
	}
	res, err := c.http.Do(req)
	if err != nil {
		return nil, 0, nil, scrub(err)
	}
	defer res.Body.Close()
	capBytes := limit
	failed := res.StatusCode < 200 || res.StatusCode > 299
	if failed {
		capBytes = maxErrorBytes
	}
	body, err := io.ReadAll(io.LimitReader(res.Body, capBytes+1))
	if err != nil {
		return nil, 0, nil, scrub(err)
	}
	if int64(len(body)) > capBytes {
		if !failed {
			return nil, 0, nil, fmt.Errorf("response exceeds %d bytes", capBytes)
		}
		body = body[:capBytes]
	}
	return body, res.StatusCode, res.Header, nil
}

// scrub drops the URL from transport errors (it carries query values) while keeping the cause.
func scrub(err error) error {
	var ue *url.Error
	if errors.As(err, &ue) {
		return ue.Err
	}
	return err
}

func (c *Client) json(ctx context.Context, r request, out any) error {
	body, status, err := c.do(ctx, r)
	if err != nil {
		return err
	}
	if out == nil || status == http.StatusNoContent || len(body) == 0 {
		return nil
	}
	if err := json.Unmarshal(body, out); err != nil {
		return &ResponseError{What: "body is not valid JSON for " + r.path}
	}
	return nil
}

func checkID(id string, bundle bool) (string, error) {
	if bundle {
		if !bundleIDRE.MatchString(id) {
			return "", configErr("invalid bundle id '%s'", id)
		}
	} else if !runIDRE.MatchString(id) {
		return "", configErr("invalid run id '%s'", id)
	}
	return id, nil
}

func newKey() string {
	var b [16]byte
	_, _ = rand.Read(b[:])
	b[6] = b[6]&0x0f | 0x40
	b[8] = b[8]&0x3f | 0x80
	h := hex.EncodeToString(b[:])
	return h[0:8] + "-" + h[8:12] + "-" + h[12:16] + "-" + h[16:20] + "-" + h[20:]
}

func (c *Client) Health(ctx context.Context) (*Health, error) {
	var out Health
	if err := c.json(ctx, request{method: "GET", path: "/health"}, &out); err != nil {
		return nil, err
	}
	if out.Status == "" {
		return nil, &ResponseError{What: "health.status is missing"}
	}
	return &out, nil
}

func (c *Client) WhoAmI(ctx context.Context) (*WhoAmI, error) {
	var out WhoAmI
	if err := c.json(ctx, request{method: "GET", path: "/whoami"}, &out); err != nil {
		return nil, err
	}
	if out.Scope == "" {
		return nil, &ResponseError{What: "whoami.scope is missing"}
	}
	return &out, nil
}

func (c *Client) Status(ctx context.Context) (*ServerStatus, error) {
	var out ServerStatus
	if err := c.json(ctx, request{method: "GET", path: "/status"}, &out); err != nil {
		return nil, err
	}
	return &out, nil
}

// ValidatePlan checks a plan (as it would run with overrides) without storing it and lists files a bundle must still carry.
// plan is a *Plan, a document map or plan file text.
func (c *Client) ValidatePlan(ctx context.Context, plan any, overrides *Overrides) (*PlanCheck, error) {
	payload, err := planPayload(plan)
	if err != nil {
		return nil, err
	}
	if err := overrides.Validate(); err != nil {
		return nil, err
	}
	body := map[string]any{"plan": payload}
	if overrides != nil {
		body["overrides"] = overrides
	}
	var out PlanCheck
	if err := c.json(ctx, request{method: "POST", path: "/plans/validate", body: body}, &out); err != nil {
		return nil, err
	}
	if out.Files == nil {
		out.Files = []string{}
	}
	return &out, nil
}

// UploadBundle stores a plan with its assets; identical content returns the stored bundle, so it is safe to retry.
func (c *Client) UploadBundle(ctx context.Context, b *Bundle) (*BundleInfo, error) {
	if b == nil {
		return nil, configErr("bundle is nil")
	}
	var out BundleInfo
	if err := c.json(ctx, request{method: "POST", path: "/bundles", body: b.payload()}, &out); err != nil {
		return nil, err
	}
	if _, err := checkID(out.ID, true); err != nil {
		return nil, &ResponseError{What: "bundle.id is malformed"}
	}
	return &out, nil
}

// UploadPlan wraps a plan without assets in a bundle and uploads it.
func (c *Client) UploadPlan(ctx context.Context, plan any, labels map[string]string) (*BundleInfo, error) {
	b, err := NewBundle(plan, labels)
	if err != nil {
		return nil, err
	}
	return c.UploadBundle(ctx, b)
}

func (c *Client) ListBundles(ctx context.Context) ([]BundleInfo, error) {
	var out Page[BundleInfo]
	if err := c.json(ctx, request{method: "GET", path: "/bundles"}, &out); err != nil {
		return nil, err
	}
	return out.Items, nil
}

func (c *Client) GetBundle(ctx context.Context, id string) (*BundleInfo, error) {
	id, err := checkID(id, true)
	if err != nil {
		return nil, err
	}
	var out BundleInfo
	if err := c.json(ctx, request{method: "GET", path: "/bundles/" + id}, &out); err != nil {
		return nil, err
	}
	return &out, nil
}

func (c *Client) DeleteBundle(ctx context.Context, id string) error {
	id, err := checkID(id, true)
	if err != nil {
		return err
	}
	_, _, err = c.do(ctx, request{method: "DELETE", path: "/bundles/" + id})
	return err
}

// CreateRunOptions configure a queued run.
type CreateRunOptions struct {
	Overrides *Overrides
	Labels    map[string]string
	// IdempotencyKey makes retries safe against duplicate runs; one is generated when empty.
	IdempotencyKey string
	// Regression fails the run when it is worse than its baseline.
	Regression *RegressionGate
}

// CreateRun queues a run of a stored bundle.
func (c *Client) CreateRun(ctx context.Context, bundleID string, opts CreateRunOptions) (*Run, error) {
	id, err := checkID(bundleID, true)
	if err != nil {
		return nil, err
	}
	key := opts.IdempotencyKey
	if key == "" {
		key = newKey()
	}
	if !keyRE.MatchString(key) {
		return nil, configErr("idempotency key must be 1..128 printable characters")
	}
	if err := opts.Overrides.Validate(); err != nil {
		return nil, err
	}
	if err := checkLabels(opts.Labels); err != nil {
		return nil, err
	}
	if err := opts.Regression.Validate(); err != nil {
		return nil, err
	}
	body := map[string]any{"bundle": id, "overrides": map[string]any{}, "labels": map[string]string{}}
	if opts.Overrides != nil {
		body["overrides"] = opts.Overrides
	}
	if opts.Labels != nil {
		body["labels"] = opts.Labels
	}
	if opts.Regression != nil {
		body["regression"] = opts.Regression
	}
	return c.runResult(ctx, request{method: "POST", path: "/runs", body: body, headers: map[string]string{"Idempotency-Key": key}})
}

func (c *Client) runResult(ctx context.Context, r request) (*Run, error) {
	var out Run
	if err := c.json(ctx, r, &out); err != nil {
		return nil, err
	}
	if err := out.check(); err != nil {
		return nil, err
	}
	return &out, nil
}

// ListOptions filter and page run listings.
type ListOptions struct {
	State  RunState
	Limit  int
	Cursor string
}

func (c *Client) ListRuns(ctx context.Context, opts ListOptions) (*Page[Run], error) {
	q := url.Values{}
	limit := opts.Limit
	if limit <= 0 {
		limit = 50
	}
	q.Set("limit", strconv.Itoa(min(limit, 200)))
	if opts.State != "" {
		if !opts.State.Known() {
			return nil, configErr("unknown state '%s'", opts.State)
		}
		q.Set("state", string(opts.State))
	}
	if opts.Cursor != "" {
		q.Set("cursor", opts.Cursor)
	}
	var out Page[Run]
	if err := c.json(ctx, request{method: "GET", path: "/runs", query: q}, &out); err != nil {
		return nil, err
	}
	for i := range out.Items {
		if err := out.Items[i].check(); err != nil {
			return nil, err
		}
	}
	return &out, nil
}

// Runs iterates over every run (newest first), fetching pages as needed.
func (c *Client) Runs(ctx context.Context, state RunState, pageSize int) iter.Seq2[Run, error] {
	return func(yield func(Run, error) bool) {
		cursor := ""
		if pageSize <= 0 {
			pageSize = 100
		}
		for {
			page, err := c.ListRuns(ctx, ListOptions{State: state, Limit: pageSize, Cursor: cursor})
			if err != nil {
				yield(Run{}, err)
				return
			}
			for _, r := range page.Items {
				if !yield(r, nil) {
					return
				}
			}
			if page.NextCursor == "" {
				return
			}
			cursor = page.NextCursor
		}
	}
}

func (c *Client) GetRun(ctx context.Context, id string) (*Run, error) {
	id, err := checkID(id, false)
	if err != nil {
		return nil, err
	}
	return c.runResult(ctx, request{method: "GET", path: "/runs/" + id})
}

// StopRun stops a running run or cancels a queued one.
func (c *Client) StopRun(ctx context.Context, id string) (*Run, error) {
	id, err := checkID(id, false)
	if err != nil {
		return nil, err
	}
	return c.runResult(ctx, request{method: "POST", path: "/runs/" + id + "/stop"})
}

func (c *Client) DeleteRun(ctx context.Context, id string) error {
	id, err := checkID(id, false)
	if err != nil {
		return err
	}
	_, _, err = c.do(ctx, request{method: "DELETE", path: "/runs/" + id})
	return err
}

// GetStats returns the latest statistics of a running or finished run.
func (c *Client) GetStats(ctx context.Context, id string) (*Snapshot, error) {
	id, err := checkID(id, false)
	if err != nil {
		return nil, err
	}
	var out Snapshot
	if err := c.json(ctx, request{method: "GET", path: "/runs/" + id + "/stats"}, &out); err != nil {
		return nil, err
	}
	return &out, nil
}

// CompareOptions select the baseline and rule for Compare; zero values use the server defaults.
type CompareOptions struct {
	Baseline         string
	MaxRegressionPct *float64
	Metrics          []CompareMetric
}

// Compare compares a finished run with a baseline (default: the latest earlier passed run of the same plan).
func (c *Client) Compare(ctx context.Context, id string, opts CompareOptions) (*Comparison, error) {
	id, err := checkID(id, false)
	if err != nil {
		return nil, err
	}
	gate := RegressionGate{Baseline: opts.Baseline, MaxRegressionPct: opts.MaxRegressionPct, Metrics: opts.Metrics}
	if err := gate.Validate(); err != nil {
		return nil, err
	}
	q := url.Values{}
	q.Set("baseline", "auto")
	if opts.Baseline != "" {
		q.Set("baseline", opts.Baseline)
	}
	if opts.MaxRegressionPct != nil {
		q.Set("max_regression_pct", strconv.FormatFloat(*opts.MaxRegressionPct, 'f', -1, 64))
	}
	if len(opts.Metrics) > 0 {
		names := make([]string, len(opts.Metrics))
		for i, m := range opts.Metrics {
			names[i] = string(m)
		}
		q.Set("metrics", strings.Join(names, ","))
	}
	var out Comparison
	if err := c.json(ctx, request{method: "GET", path: "/runs/" + id + "/compare", query: q}, &out); err != nil {
		return nil, err
	}
	return &out, nil
}

// Artifact downloads one of the files in Artifacts.
func (c *Client) Artifact(ctx context.Context, id, name string) ([]byte, error) {
	id, err := checkID(id, false)
	if err != nil {
		return nil, err
	}
	if !slices.Contains(Artifacts, name) {
		return nil, configErr("unknown artifact '%s'", name)
	}
	body, _, err := c.do(ctx, request{method: "GET", path: "/runs/" + id + "/artifacts/" + name, limit: maxArtifactBytes})
	return body, err
}

// Report returns the parsed report.json of a finished run.
func (c *Client) Report(ctx context.Context, id string) (map[string]any, error) {
	raw, err := c.Artifact(ctx, id, "report.json")
	if err != nil {
		return nil, err
	}
	var out map[string]any
	if err := json.Unmarshal(raw, &out); err != nil {
		return nil, &ResponseError{What: "report.json is not a JSON object"}
	}
	return out, nil
}

// Download writes an artifact to dest (a file path, or an existing directory to place it in) and returns the path.
func (c *Client) Download(ctx context.Context, id, name, dest string) (string, error) {
	data, err := c.Artifact(ctx, id, name)
	if err != nil {
		return "", err
	}
	target := dest
	if st, err := os.Stat(dest); err == nil && st.IsDir() {
		target = filepath.Join(dest, id+"-"+name)
	}
	tmp, err := os.CreateTemp(filepath.Dir(target), ".download-*")
	if err != nil {
		return "", err
	}
	defer os.Remove(tmp.Name())
	if _, err := tmp.Write(data); err != nil {
		tmp.Close()
		return "", err
	}
	if err := tmp.Chmod(0o644); err != nil {
		tmp.Close()
		return "", err
	}
	if err := tmp.Close(); err != nil {
		return "", err
	}
	return target, os.Rename(tmp.Name(), target)
}

// Events streams run events until the run finishes (end is the last event).
// The stream fails with a TransportError when no data arrives for idleTimeout (default 60s).
func (c *Client) Events(ctx context.Context, id string, idleTimeout time.Duration) iter.Seq2[Event, error] {
	return func(yield func(Event, error) bool) {
		id, err := checkID(id, false)
		if err != nil {
			yield(Event{}, err)
			return
		}
		if idleTimeout <= 0 {
			idleTimeout = 60 * time.Second
		}
		path := "/runs/" + id + "/events"
		sctx, cancel := context.WithCancel(ctx)
		defer cancel()
		var idled atomic.Bool
		timer := time.AfterFunc(idleTimeout, func() { idled.Store(true); cancel() })
		defer timer.Stop()
		req, err := http.NewRequestWithContext(sctx, "GET", c.baseURL+path, nil)
		if err != nil {
			yield(Event{}, &TransportError{Op: "GET " + path, Err: err})
			return
		}
		c.headers(req, "text/event-stream")
		res, err := c.http.Do(req)
		if err != nil {
			yield(Event{}, &TransportError{Op: "GET " + path, Err: scrub(err)})
			return
		}
		defer res.Body.Close()
		if res.StatusCode < 200 || res.StatusCode > 299 {
			body, _ := io.ReadAll(io.LimitReader(res.Body, maxErrorBytes))
			yield(Event{}, apiError(res.StatusCode, res.Header, body))
			return
		}
		rd := newSSEReader(res.Body)
		for {
			msg, err := rd.Next()
			if err != nil {
				reason := "closed"
				if idled.Load() {
					reason = "idle timeout"
				} else if ctx.Err() != nil {
					reason = ctx.Err().Error()
				}
				yield(Event{}, &TransportError{Op: "event stream interrupted", Err: errors.New(reason)})
				return
			}
			timer.Reset(idleTimeout)
			var ev Event
			switch msg.Event {
			case "stats":
				var s Snapshot
				if json.Unmarshal([]byte(msg.Data), &s) != nil {
					yield(Event{}, &ResponseError{What: "stats event is not valid JSON"})
					return
				}
				ev = Event{Type: EventStats, Stats: &s}
			case "run", "end":
				var r Run
				if json.Unmarshal([]byte(msg.Data), &r) != nil {
					yield(Event{}, &ResponseError{What: "run event is not valid JSON"})
					return
				}
				if err := r.check(); err != nil {
					yield(Event{}, err)
					return
				}
				ev = Event{Type: EventType(msg.Event), Run: &r}
			default:
				continue
			}
			if !yield(ev, nil) || ev.Type == EventEnd {
				return
			}
		}
	}
}

// WaitOptions configure Wait.
type WaitOptions struct {
	// Timeout bounds the whole wait; zero waits until the run finishes or ctx ends.
	Timeout      time.Duration
	PollInterval time.Duration
	OnStats      func(*Snapshot)
	OnState      func(*Run)
}

// Wait returns when the run finishes. It follows the event stream and falls back to polling.
func (c *Client) Wait(ctx context.Context, id string, opts WaitOptions) (*Run, error) {
	if _, err := checkID(id, false); err != nil {
		return nil, err
	}
	wctx := ctx
	if opts.Timeout > 0 {
		var cancel context.CancelFunc
		wctx, cancel = context.WithTimeout(ctx, opts.Timeout)
		defer cancel()
	}
	expired := func(err error) error {
		if wctx.Err() != nil && ctx.Err() == nil {
			return &RunTimeoutError{RunID: id, Timeout: opts.Timeout}
		}
		return err
	}
	poll := opts.PollInterval
	if poll <= 0 {
		poll = 2 * time.Second
	}
	for {
		for ev, err := range c.Events(wctx, id, max(poll*10, 30*time.Second)) {
			if err != nil {
				var api *APIError
				var resp *ResponseError
				if wctx.Err() != nil {
					return nil, expired(err)
				}
				if errors.As(err, &resp) || (errors.As(err, &api) && !retryStatus(api.Status)) {
					return nil, err
				}
				break
			}
			switch ev.Type {
			case EventStats:
				if opts.OnStats != nil {
					opts.OnStats(ev.Stats)
				}
			case EventRun, EventEnd:
				if opts.OnState != nil {
					opts.OnState(ev.Run)
				}
				if ev.Type == EventEnd {
					return ev.Run, nil
				}
			}
		}
		run, err := c.GetRun(wctx, id)
		if err != nil {
			return nil, expired(err)
		}
		if run.State.Terminal() {
			return run, nil
		}
		if err := sleepCtx(wctx, poll); err != nil {
			return nil, expired(err)
		}
	}
}

// RunOptions configure Run: the queued run plus whether and how to wait for it.
type RunOptions struct {
	CreateRunOptions
	NoWait bool
	Wait   WaitOptions
}

// Run uploads (for a *Bundle, *Plan, Plan or document map), queues and by default waits for a run.
// A string or *BundleInfo refers to a stored bundle.
func (c *Client) Run(ctx context.Context, target any, opts RunOptions) (*Run, error) {
	var bundleID string
	switch t := target.(type) {
	case string:
		bundleID = t
	case *BundleInfo:
		bundleID = t.ID
	case BundleInfo:
		bundleID = t.ID
	case *Bundle:
		info, err := c.UploadBundle(ctx, t)
		if err != nil {
			return nil, err
		}
		bundleID = info.ID
	default:
		info, err := c.UploadPlan(ctx, target, nil)
		if err != nil {
			return nil, err
		}
		bundleID = info.ID
	}
	run, err := c.CreateRun(ctx, bundleID, opts.CreateRunOptions)
	if err != nil || opts.NoWait {
		return run, err
	}
	return c.Wait(ctx, run.ID, opts.Wait)
}
