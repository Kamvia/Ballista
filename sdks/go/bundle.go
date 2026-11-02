// File: ballista/sdks/go/bundle.go
package ballista

import (
	"encoding/base64"
	"fmt"
	"io"
	"maps"
	"os"
	"regexp"
	"slices"
	"strings"
)

const (
	MaxFiles      = 256
	MaxFileBytes  = 16 << 20
	MaxTotalBytes = 64 << 20
	MaxPlanBytes  = 1 << 20
)

var segmentRE = regexp.MustCompile(`^[A-Za-z0-9_-][A-Za-z0-9._-]{0,99}$`)

// ValidBundlePath mirrors the server rule: relative, at most 8 segments, no hidden or parent segments.
func ValidBundlePath(rel string) bool {
	if rel == "" || len(rel) > 200 || strings.HasPrefix(rel, "/") {
		return false
	}
	parts := strings.Split(rel, "/")
	if len(parts) > 8 || parts[0] == "inline" {
		return false
	}
	for _, p := range parts {
		if !segmentRE.MatchString(p) {
			return false
		}
	}
	return rel != "plan.toml" && rel != "bundle.json"
}

// planPayload converts a *Plan, Plan, document map or plan file text into the `plan` field.
func planPayload(plan any) (any, error) {
	switch p := plan.(type) {
	case *Plan:
		if p == nil {
			return nil, configErr("plan is nil")
		}
		return p.Document()
	case Plan:
		return p.Document()
	case map[string]any:
		if len(p) == 0 {
			return nil, configErr("plan document is empty")
		}
		return maps.Clone(p), nil
	case string:
		if strings.TrimSpace(p) == "" || len(p) > MaxPlanBytes {
			return nil, configErr("plan text must be non-empty and at most 1 MiB")
		}
		return p, nil
	default:
		return nil, configErr("plan must be a *Plan, a document map or plan text")
	}
}

// Bundle is a plan with optional assets (descriptor sets, certificates, upload files) sent as content.
// Inline scripts (Scenario.Source) and inline feeder rows need no files.
type Bundle struct {
	plan   any
	labels map[string]string
	files  map[string][]byte
}

// NewBundle wraps a *Plan, a document map or plan file text.
func NewBundle(plan any, labels map[string]string) (*Bundle, error) {
	payload, err := planPayload(plan)
	if err != nil {
		return nil, err
	}
	if err := checkLabels(labels); err != nil {
		return nil, err
	}
	return &Bundle{plan: payload, labels: maps.Clone(labels), files: map[string][]byte{}}, nil
}

// AddBytes stores data under the bundle path rel.
func (b *Bundle) AddBytes(rel string, data []byte) error {
	if !ValidBundlePath(rel) {
		return configErr("invalid bundle path '%s'", rel)
	}
	if len(data) > MaxFileBytes {
		return configErr("'%s' exceeds 16 MiB", rel)
	}
	if _, ok := b.files[rel]; !ok && len(b.files) >= MaxFiles {
		return configErr("a bundle holds at most %d files", MaxFiles)
	}
	total := len(data)
	for k, v := range b.files {
		if k != rel {
			total += len(v)
		}
	}
	if total > MaxTotalBytes {
		return configErr("bundle exceeds 64 MiB")
	}
	b.files[rel] = slices.Clone(data)
	return nil
}

// AddText stores UTF-8 text under the bundle path rel.
func (b *Bundle) AddText(rel, text string) error { return b.AddBytes(rel, []byte(text)) }

// AddFile stores the content of a local regular file under the bundle path rel.
func (b *Bundle) AddFile(rel, path string) error {
	f, err := os.Open(path)
	if err != nil {
		return configErr("'%s': %v", rel, err)
	}
	defer f.Close()
	st, err := f.Stat()
	if err != nil || !st.Mode().IsRegular() {
		return configErr("'%s': %s is not a regular file", rel, path)
	}
	data, err := io.ReadAll(io.LimitReader(f, MaxFileBytes+1))
	if err != nil {
		return configErr("'%s': %v", rel, err)
	}
	if len(data) > MaxFileBytes {
		return configErr("'%s' exceeds 16 MiB", rel)
	}
	return b.AddBytes(rel, data)
}

// Files maps each bundle path to its size in bytes.
func (b *Bundle) Files() map[string]int {
	out := make(map[string]int, len(b.files))
	for k, v := range b.files {
		out[k] = len(v)
	}
	return out
}

func (b *Bundle) String() string {
	kind := "document"
	if _, ok := b.plan.(string); ok {
		kind = "text"
	}
	return fmt.Sprintf("Bundle(plan=%s, files=%d)", kind, len(b.files))
}

type bundlePayload struct {
	Plan   any               `json:"plan"`
	Files  map[string]string `json:"files"`
	Labels map[string]string `json:"labels"`
}

func (b *Bundle) payload() bundlePayload {
	files := make(map[string]string, len(b.files))
	for k, v := range b.files {
		files[k] = base64.StdEncoding.EncodeToString(v)
	}
	labels := b.labels
	if labels == nil {
		labels = map[string]string{}
	}
	return bundlePayload{Plan: b.plan, Files: files, Labels: labels}
}
