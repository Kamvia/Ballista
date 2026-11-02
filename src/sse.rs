// File: ballista/src/sse.rs
use std::time::Duration;

const MAX_LINE: usize = 1024 * 1024;

/// One server-sent event with its arrival time relative to the request start.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
    pub id: Option<String>,
    pub at: Duration,
}

/// Incremental `text/event-stream` parser (WHATWG HTML, server-sent events).
#[derive(Debug, Default)]
pub(crate) struct SseParser {
    pending: Vec<u8>,
    event: String,
    data: String,
    has_data: bool,
    id: Option<String>,
    pub(crate) last_id: Option<String>,
    pub(crate) retry_ms: Option<u64>,
    pub(crate) overflow: bool,
    bom_checked: bool,
}

impl SseParser {
    /// Feeds bytes and appends completed events. `budget` caps the total data bytes kept.
    pub(crate) fn feed(&mut self, chunk: &[u8], at: Duration, out: &mut Vec<SseEvent>, max: usize, budget: &mut usize) {
        self.pending.extend_from_slice(chunk);
        if !self.bom_checked && self.pending.len() >= 3 {
            if self.pending.starts_with(&[0xEF, 0xBB, 0xBF]) {
                self.pending.drain(..3);
            }
            self.bom_checked = true;
        }
        loop {
            if out.len() >= max {
                return;
            }
            let Some(pos) = self.pending.iter().position(|b| *b == b'\n' || *b == b'\r') else {
                if self.pending.len() > MAX_LINE {
                    self.overflow = true;
                    self.pending.clear();
                }
                return;
            };
            let mut end = pos + 1;
            if self.pending[pos] == b'\r' {
                if pos + 1 == self.pending.len() {
                    return;
                }
                if self.pending[pos + 1] == b'\n' {
                    end += 1;
                }
            }
            let line: Vec<u8> = self.pending.drain(..end).take(pos).collect();
            let line = String::from_utf8_lossy(&line);
            self.line(&line, at, out, budget);
        }
    }

    fn line(&mut self, line: &str, at: Duration, out: &mut Vec<SseEvent>, budget: &mut usize) {
        if line.is_empty() {
            self.dispatch(at, out);
            return;
        }
        if line.starts_with(':') {
            return;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = value.to_owned(),
            "data" => {
                if value.len() + 1 > *budget {
                    self.overflow = true;
                    return;
                }
                *budget -= value.len() + 1;
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            "id" if !value.contains('\0') => self.id = Some(value.to_owned()),
            "retry" => {
                if let Ok(ms) = value.parse::<u64>() {
                    self.retry_ms = Some(ms);
                }
            }
            _ => {}
        }
    }

    fn dispatch(&mut self, at: Duration, out: &mut Vec<SseEvent>) {
        if let Some(id) = self.id.take() {
            self.last_id = Some(id);
        }
        if !self.has_data {
            self.event.clear();
            return;
        }
        let event = std::mem::take(&mut self.event);
        out.push(SseEvent {
            event: if event.is_empty() { "message".to_owned() } else { event },
            data: std::mem::take(&mut self.data),
            id: self.last_id.clone(),
            at,
        });
        self.has_data = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fields_multiline_data_and_ids() {
        let mut p = SseParser::default();
        let mut out = Vec::new();
        let mut budget = 1 << 20;
        let raw = b": comment\r\nevent: tick\r\ndata: a\r\ndata: b\r\nid: 7\r\n\r\ndata:x\n\nretry: 1500\n";
        let (a, b) = raw.split_at(17);
        p.feed(a, Duration::ZERO, &mut out, 10, &mut budget);
        p.feed(b, Duration::from_millis(5), &mut out, 10, &mut budget);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].event, "tick");
        assert_eq!(out[0].data, "a\nb");
        assert_eq!(out[0].id.as_deref(), Some("7"));
        assert_eq!(out[1].event, "message");
        assert_eq!(out[1].data, "x");
        assert_eq!(out[1].id.as_deref(), Some("7"));
        assert_eq!(p.retry_ms, Some(1500));
    }

    #[test]
    fn respects_event_limit_and_budget() {
        let mut p = SseParser::default();
        let mut out = Vec::new();
        let mut budget = 4;
        p.feed(b"data: 123456\n\ndata: 1\n\n", Duration::ZERO, &mut out, 5, &mut budget);
        assert!(p.overflow);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].data, "1");
    }
}
