// File: ballista/sdks/go/transport_test.go
package ballista

import (
	"context"
	"encoding/pem"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

func TestRetryAfterIsHonoredAndCapped(t *testing.T) {
	var calls atomic.Int32
	var gaps []time.Duration
	last := time.Now()
	c := testClient(t, func(w http.ResponseWriter, r *http.Request) {
		now := time.Now()
		gaps = append(gaps, now.Sub(last))
		last = now
		if calls.Add(1) == 1 {
			w.Header().Set("Retry-After", "3600")
			w.WriteHeader(http.StatusTooManyRequests)
			return
		}
		_, _ = io.WriteString(w, `{"status":"ok","version":"1"}`)
	})
	if _, err := c.Health(context.Background()); err != nil {
		t.Fatal(err)
	}
	if gaps[1] > time.Second {
		t.Fatalf("Retry-After must be capped by MaxDelay, waited %v", gaps[1])
	}
}

func TestRetriesExhaustAndClientErrorsAreNotRetried(t *testing.T) {
	var calls atomic.Int32
	status := http.StatusServiceUnavailable
	c := testClient(t, func(w http.ResponseWriter, r *http.Request) {
		calls.Add(1)
		w.Header().Set("Retry-After", "0")
		w.WriteHeader(status)
		_, _ = io.WriteString(w, "<html>gateway</html>")
	})
	_, err := c.Health(context.Background())
	var api *APIError
	if !errors.As(err, &api) || api.Status != 503 || api.Code != "error" || calls.Load() != 3 {
		t.Fatalf("exhausted: %v calls=%d", err, calls.Load())
	}
	calls.Store(0)
	status = http.StatusUnprocessableEntity
	if _, err := c.Health(context.Background()); !IsValidation(err) || calls.Load() != 1 {
		t.Fatalf("422 retried: %v calls=%d", err, calls.Load())
	}
	status = http.StatusConflict
	if _, err := c.Health(context.Background()); !IsConflict(err) {
		t.Fatal("409")
	}
	status = http.StatusTooManyRequests
	if _, err := c.Health(context.Background()); !IsRateLimited(err) || !errors.As(err, &api) || api.RetryAfter != 0 {
		t.Fatalf("429: %v", err)
	}
}

func TestErrorDetailIsBoundedAndTokenFree(t *testing.T) {
	long := strings.Repeat("é", 5000)
	c := testClient(t, func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusBadRequest)
		fmt.Fprintf(w, `{"error":{"code":"bad_request","message":%q}}`, long)
	})
	_, err := c.Health(context.Background())
	var api *APIError
	if !errors.As(err, &api) || len([]rune(api.Detail)) != 1000 || api.Code != "bad_request" {
		t.Fatalf("detail: %d runes, %v", len([]rune(api.Detail)), api.Code)
	}
	if strings.Contains(err.Error(), testToken) {
		t.Fatal("token in error")
	}
}

func TestTransportErrors(t *testing.T) {
	srv := httptest.NewServer(http.NotFoundHandler())
	url := srv.URL
	srv.Close()
	c, _ := New(Options{URL: url, Token: testToken, Retry: &RetryPolicy{Attempts: 2, BaseDelay: time.Millisecond, MaxDelay: time.Millisecond}})
	_, err := c.Health(context.Background())
	var te *TransportError
	if !errors.As(err, &te) || te.Op != "GET /health" || errors.Unwrap(err) == nil || strings.Contains(err.Error(), testToken) {
		t.Fatalf("connection refused: %v", err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if _, err := c.Health(ctx); !errors.As(err, &te) || !errors.Is(err, context.Canceled) {
		t.Fatalf("cancelled: %v", err)
	}
}

func TestSlowServerHitsRequestTimeout(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		select {
		case <-r.Context().Done():
		case <-time.After(2 * time.Second):
		}
	}))
	defer srv.Close()
	c, _ := New(Options{URL: srv.URL, Token: testToken, Timeout: 50 * time.Millisecond, Retry: &RetryPolicy{Attempts: 1}})
	start := time.Now()
	var te *TransportError
	if _, err := c.Health(context.Background()); !errors.As(err, &te) || time.Since(start) > time.Second {
		t.Fatalf("timeout: %v after %v", err, time.Since(start))
	}
}

func TestMalformedResponsesAreRejected(t *testing.T) {
	bodies := map[string]string{
		"/api/v1/health":                                   `not json`,
		"/api/v1/whoami":                                   `{"name":"x"}`,
		"/api/v1/runs/r000000000000000000000001":           `{"id":"../../etc","state":"passed"}`,
		"/api/v1/runs/r000000000000000000000002":           `{"id":"r000000000000000000000002","state":"exploded"}`,
		"/api/v1/bundles/0123456789abcdef0123456789abcdef": `{"id":"0123456789abcdef0123456789abcdef","files":"oops"}`,
	}
	c := testClient(t, func(w http.ResponseWriter, r *http.Request) {
		_, _ = io.WriteString(w, bodies[r.URL.Path])
	})
	ctx := context.Background()
	var re *ResponseError
	if _, err := c.Health(ctx); !errors.As(err, &re) {
		t.Errorf("invalid JSON: %v", err)
	}
	if _, err := c.WhoAmI(ctx); !errors.As(err, &re) {
		t.Errorf("missing scope: %v", err)
	}
	if _, err := c.GetRun(ctx, "r000000000000000000000001"); !errors.As(err, &re) {
		t.Errorf("malformed id: %v", err)
	}
	if _, err := c.GetRun(ctx, "r000000000000000000000002"); !errors.As(err, &re) {
		t.Errorf("unknown state: %v", err)
	}
	if _, err := c.GetBundle(ctx, "0123456789abcdef0123456789abcdef"); !errors.As(err, &re) {
		t.Errorf("wrong field type: %v", err)
	}
}

func TestOversizedResponseIsRejected(t *testing.T) {
	c := testClient(t, func(w http.ResponseWriter, r *http.Request) {
		_, _ = io.WriteString(w, `{"status":"`)
		chunk := strings.Repeat("a", 1<<20)
		for i := 0; i < 33; i++ {
			_, _ = io.WriteString(w, chunk)
		}
		_, _ = io.WriteString(w, `"}`)
	})
	if _, err := c.Health(context.Background()); err == nil || !strings.Contains(err.Error(), "exceeds") {
		t.Fatalf("32 MiB limit: %v", err)
	}
}

func TestCustomCertificateAuthority(t *testing.T) {
	srv := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = io.WriteString(w, `{"status":"ok","version":"tls"}`)
	}))
	defer srv.Close()
	dir := t.TempDir()
	ca := filepath.Join(dir, "ca.pem")
	if err := os.WriteFile(ca, pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: srv.Certificate().Raw}), 0o600); err != nil {
		t.Fatal(err)
	}
	retry := &RetryPolicy{Attempts: 1}
	untrusted, _ := New(Options{URL: srv.URL, Token: testToken, Retry: retry})
	if _, err := untrusted.Health(context.Background()); err == nil {
		t.Fatal("self-signed certificate accepted without the CA")
	}
	trusted, err := New(Options{URL: srv.URL, Token: testToken, CACertFile: ca, Retry: retry})
	if err != nil {
		t.Fatal(err)
	}
	if h, err := trusted.Health(context.Background()); err != nil || h.Version != "tls" {
		t.Fatalf("with CA: %v %v", h, err)
	}
	empty := filepath.Join(dir, "empty.pem")
	_ = os.WriteFile(empty, []byte("no certs here"), 0o600)
	for _, f := range []string{empty, filepath.Join(dir, "missing.pem")} {
		if _, err := New(Options{URL: srv.URL, Token: testToken, CACertFile: f}); err == nil {
			t.Errorf("CA file %s accepted", f)
		}
	}
}

func TestCustomHTTPClientStillRefusesRedirects(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/elsewhere" {
			t.Error("redirect followed")
		}
		http.Redirect(w, r, "/elsewhere", http.StatusTemporaryRedirect)
	}))
	defer srv.Close()
	hc := &http.Client{}
	c, _ := New(Options{URL: srv.URL, Token: testToken, HTTPClient: hc, Retry: &RetryPolicy{Attempts: 1}})
	var api *APIError
	if _, err := c.Health(context.Background()); !errors.As(err, &api) || api.Status != 307 {
		t.Fatalf("redirect: %v", err)
	}
	if hc.CheckRedirect != nil {
		t.Fatal("caller's client was modified")
	}
}

func TestEnvironmentConfiguration(t *testing.T) {
	t.Setenv(URLEnv, "http://localhost:9999")
	t.Setenv(TokenEnv, "  "+testToken+"\n")
	c, err := New(Options{})
	if err != nil || c.BaseURL() != "http://localhost:9999/api/v1" {
		t.Fatalf("env: %v %v", c, err)
	}
	t.Setenv(URLEnv, "")
	if _, err := New(Options{}); err == nil {
		t.Fatal("missing url accepted")
	}
	if c, err := New(Options{URL: "http://10.1.2.3", Token: testToken, AllowInsecureHTTP: true}); err != nil || !strings.HasPrefix(c.BaseURL(), "http://10.1.2.3") {
		t.Fatalf("explicit insecure: %v", err)
	}
	for _, u := range []string{"http://[::1]:8070", "http://127.10.0.1"} {
		if _, err := New(Options{URL: u, Token: testToken}); err != nil {
			t.Errorf("loopback %s refused: %v", u, err)
		}
	}
	if _, err := New(Options{URL: "http://localhost.evil.example", Token: testToken}); err == nil {
		t.Fatal("look-alike loopback host accepted over http")
	}
}
