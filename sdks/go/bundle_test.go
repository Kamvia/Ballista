// File: ballista/sdks/go/bundle_test.go
package ballista

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestBundlePathRules(t *testing.T) {
	good := []string{"a", "scenarios/s.rhai", "a/b/c/d/e/f/g/h", "_x/-y/z.v1.csv", "files/report.txt"}
	bad := []string{
		"", "/abs", "a/", "./a", "../a", "a/../b", ".env", "a/.git/config", "a\\b", "a b", "a/b/c/d/e/f/g/h/i",
		"inline/x", "plan.toml", "bundle.json", strings.Repeat("a", 201), "é.txt", "a\x00b",
	}
	for _, p := range good {
		if !ValidBundlePath(p) {
			t.Errorf("rejected %q", p)
		}
	}
	for _, p := range bad {
		if ValidBundlePath(p) {
			t.Errorf("accepted %q", p)
		}
	}
}

func TestBundleSizeLimits(t *testing.T) {
	b, _ := NewBundle("name = \"x\"", nil)
	if b.AddBytes("big", make([]byte, MaxFileBytes+1)) == nil {
		t.Fatal("file over 16 MiB accepted")
	}
	chunk := make([]byte, MaxFileBytes)
	for i := 0; i < 4; i++ {
		if err := b.AddBytes(fmt.Sprintf("part%d", i), chunk); err != nil {
			t.Fatal(err)
		}
	}
	if b.AddBytes("part4", []byte{1}) == nil {
		t.Fatal("total over 64 MiB accepted")
	}
	if err := b.AddBytes("part0", chunk); err != nil {
		t.Fatalf("replacing a file must not count it twice: %v", err)
	}

	many, _ := NewBundle("name = \"x\"", nil)
	for i := 0; i < MaxFiles; i++ {
		if err := many.AddText(fmt.Sprintf("f%d", i), "x"); err != nil {
			t.Fatal(err)
		}
	}
	if many.AddText("one-more", "x") == nil {
		t.Fatal("more than 256 files accepted")
	}
	if err := many.AddText("f0", "replace"); err != nil {
		t.Fatalf("replace at the file limit: %v", err)
	}
}

func TestBundleCopiesInput(t *testing.T) {
	b, _ := NewBundle("name = \"x\"", nil)
	data := []byte("abc")
	_ = b.AddBytes("f", data)
	data[0] = 'z'
	if string(b.files["f"]) != "abc" {
		t.Fatal("bundle aliases the caller's slice")
	}
	labels := map[string]string{"k": "v"}
	b2, _ := NewBundle("name = \"x\"", labels)
	labels["k"] = "changed"
	if b2.payload().Labels["k"] != "v" {
		t.Fatal("bundle aliases the caller's labels")
	}
}

func TestBundleFromFiles(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "s.rhai")
	_ = os.WriteFile(path, []byte("fn run(s, r) {}"), 0o600)
	b, _ := NewBundle(shopPlan("http://127.0.0.1:8080/"), nil)
	if err := b.AddFile("scenarios/s.rhai", path); err != nil {
		t.Fatal(err)
	}
	if err := b.AddFile("x", filepath.Join(dir, "missing")); err == nil {
		t.Fatal("missing file accepted")
	}
	if err := b.AddFile("x", dir); err == nil {
		t.Fatal("directory accepted")
	}
	link := filepath.Join(dir, "link")
	if os.Symlink(path, link) == nil {
		if err := b.AddFile("scenarios/link.rhai", link); err != nil {
			t.Fatalf("symlink to a regular file: %v", err)
		}
	}
	if s := b.String(); s != fmt.Sprintf("Bundle(plan=document, files=%d)", len(b.Files())) {
		t.Fatal(s)
	}
	text, _ := NewBundle("name = \"x\"", nil)
	if text.String() != "Bundle(plan=text, files=0)" {
		t.Fatal(text.String())
	}
}

func TestPlanPayloadForms(t *testing.T) {
	p := shopPlan("http://127.0.0.1:8080/")
	for _, ok := range []any{p, *p, map[string]any{"name": "doc"}, "name = \"text\""} {
		if _, err := planPayload(ok); err != nil {
			t.Errorf("%T: %v", ok, err)
		}
	}
	var nilPlan *Plan
	for _, bad := range []any{nilPlan, map[string]any{}, "", "  \n", strings.Repeat("#", MaxPlanBytes+1), 1, []byte("x"), nil} {
		if _, err := planPayload(bad); err == nil {
			t.Errorf("accepted %T", bad)
		}
	}
	doc := map[string]any{"name": "doc"}
	out, _ := planPayload(doc)
	doc["name"] = "changed"
	if out.(map[string]any)["name"] != "doc" {
		t.Fatal("document map aliased")
	}
	if _, err := NewBundle(p, map[string]string{"bad key": "v"}); err == nil {
		t.Fatal("bad labels accepted")
	}
}
