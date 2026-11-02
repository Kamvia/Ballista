// File: ballista/sdks/go/sse.go
package ballista

import (
	"bufio"
	"errors"
	"io"
	"strings"
)

const maxSSELine = 16 << 20

type sseMessage struct {
	Event string
	Data  string
	ID    string
}

// sseReader parses a text/event-stream incrementally.
type sseReader struct {
	r *bufio.Reader
}

func newSSEReader(r io.Reader) *sseReader {
	return &sseReader{r: bufio.NewReaderSize(r, 64<<10)}
}

func (s *sseReader) line() (string, error) {
	var b strings.Builder
	for {
		frag, err := s.r.ReadSlice('\n')
		if b.Len()+len(frag) > maxSSELine {
			return "", errors.New("event stream line too long")
		}
		b.Write(frag)
		if err == bufio.ErrBufferFull {
			continue
		}
		if err != nil {
			if err == io.EOF && b.Len() > 0 {
				break
			}
			return "", err
		}
		break
	}
	out := b.String()
	out = strings.TrimSuffix(out, "\n")
	return strings.TrimSuffix(out, "\r"), nil
}

// Next returns the next complete message, or io.EOF when the stream ends.
func (s *sseReader) Next() (sseMessage, error) {
	msg := sseMessage{Event: "message"}
	var data []string
	size := 0
	for {
		line, err := s.line()
		if err != nil {
			return sseMessage{}, err
		}
		if line == "" {
			if len(data) > 0 {
				msg.Data = strings.Join(data, "\n")
				return msg, nil
			}
			msg = sseMessage{Event: "message"}
			continue
		}
		if strings.HasPrefix(line, ":") {
			continue
		}
		field, value, found := strings.Cut(line, ":")
		if found {
			value = strings.TrimPrefix(value, " ")
		}
		switch field {
		case "event":
			msg.Event = value
		case "data":
			size += len(value)
			if size > maxSSELine {
				return sseMessage{}, errors.New("event stream message too large")
			}
			data = append(data, value)
		case "id":
			if !strings.ContainsRune(value, 0) {
				msg.ID = value
			}
		}
	}
}
