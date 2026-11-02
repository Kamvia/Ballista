// File: ballista/sdks/go/sse_test.go
package ballista

import (
	"errors"
	"io"
	"strings"
	"testing"
	"testing/iotest"
)

func readAll(t *testing.T, r io.Reader) ([]sseMessage, error) {
	t.Helper()
	p := newSSEReader(r)
	var out []sseMessage
	for {
		m, err := p.Next()
		if err != nil {
			return out, err
		}
		out = append(out, m)
	}
}

func TestSSEParsing(t *testing.T) {
	stream := ": comment\n" +
		"event: run\r\ndata: {\"a\":1}\r\n\r\n" +
		"data: line one\ndata:line two\nid: 7\n\n" +
		"event: ignored-without-data\n\n" +
		"id: bad\x00id\ndata: x\n\n" +
		"field-without-colon\ndata\n\n" +
		"data: trailing message is dropped without a blank line"
	msgs, err := readAll(t, iotest.OneByteReader(strings.NewReader(stream)))
	if !errors.Is(err, io.EOF) {
		t.Fatalf("end: %v", err)
	}
	want := []sseMessage{
		{Event: "run", Data: `{"a":1}`},
		{Event: "message", Data: "line one\nline two", ID: "7"},
		{Event: "message", Data: "x"},
		{Event: "message", Data: ""},
	}
	if len(msgs) != len(want) {
		t.Fatalf("got %d messages: %+v", len(msgs), msgs)
	}
	for i := range want {
		if msgs[i] != want[i] {
			t.Errorf("message %d: %+v want %+v", i, msgs[i], want[i])
		}
	}
}

func TestSSEEventResetsBetweenMessages(t *testing.T) {
	msgs, _ := readAll(t, strings.NewReader("event: stats\ndata: 1\n\ndata: 2\n\n"))
	if len(msgs) != 2 || msgs[1].Event != "message" {
		t.Fatalf("event name leaked: %+v", msgs)
	}
}

func TestSSELimits(t *testing.T) {
	long := "data: " + strings.Repeat("x", maxSSELine+1) + "\n\n"
	if _, err := readAll(t, strings.NewReader(long)); err == nil || !strings.Contains(err.Error(), "too long") {
		t.Fatalf("long line: %v", err)
	}
	var b strings.Builder
	chunk := "data: " + strings.Repeat("y", 1<<20) + "\n"
	for i := 0; i < 17; i++ {
		b.WriteString(chunk)
	}
	b.WriteString("\n")
	if _, err := readAll(t, strings.NewReader(b.String())); err == nil || !strings.Contains(err.Error(), "too large") {
		t.Fatalf("large message: %v", err)
	}
}
