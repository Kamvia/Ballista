// File: ballista/src/scenario/feeder.rs
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::config::{FeederConfig, FeederMode};
use crate::error::{Error, Result};

pub type FeedRow = HashMap<String, String>;

const MAX_FEEDER_BYTES: u64 = 256 * 1024 * 1024;
const MAX_COLUMNS: usize = 256;
const MAX_ROWS: usize = 10_000_000;

pub struct Feeder {
    rows: Vec<Arc<FeedRow>>,
    cursor: AtomicUsize,
    mode: FeederMode,
    /// Packed (index << 32 | count) partition used in distributed runs so workers never share rows.
    partition: AtomicU64,
}

fn unpack(p: u64) -> (usize, usize) {
    ((p >> 32) as usize, (p & 0xFFFF_FFFF).max(1) as usize)
}

impl Feeder {
    pub fn next(&self) -> Option<Arc<FeedRow>> {
        if self.rows.is_empty() {
            return None;
        }
        let (index, count) = unpack(self.partition.load(Ordering::Relaxed));
        let len = self.rows.len();
        let (index, count) = if index < len { (index, count) } else { (0, 1) };
        let slots = (len - index).div_ceil(count);
        let pos = match self.mode {
            FeederMode::Sequential => self.cursor.fetch_add(1, Ordering::Relaxed) % slots,
            FeederMode::Random => rand::random_range(0..slots),
        };
        Some(Arc::clone(&self.rows[index + pos * count]))
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

#[derive(Default)]
pub struct Feeders {
    map: HashMap<String, Feeder>,
}

impl Feeders {
    pub fn load(cfgs: &[FeederConfig]) -> Result<Self> {
        let mut map = HashMap::new();
        for c in cfgs {
            let rows = load_csv(&c.path)?;
            map.insert(c.name.clone(), Feeder { rows, cursor: AtomicUsize::new(0), mode: c.mode, partition: AtomicU64::new(1) });
        }
        Ok(Self { map })
    }

    pub fn next(&self, name: &str) -> Option<Arc<FeedRow>> {
        self.map.get(name).and_then(Feeder::next)
    }

    /// Restricts every feeder to rows `index, index + count, ...` and rewinds sequential cursors.
    pub fn set_partition(&self, index: u32, count: u32) {
        self.repartition(index, count, true);
    }

    /// Changes the partition; `rewind` restarts sequential cursors (a new run) or keeps them (membership change).
    pub fn repartition(&self, index: u32, count: u32, rewind: bool) {
        let packed = (u64::from(index) << 32) | u64::from(count.max(1));
        for f in self.map.values() {
            f.partition.store(packed, Ordering::Relaxed);
            if rewind {
                f.cursor.store(0, Ordering::Relaxed);
            }
        }
    }

    pub fn get(&self, name: &str) -> Option<&Feeder> {
        self.map.get(name)
    }
}

fn load_csv(path: &Path) -> Result<Vec<Arc<FeedRow>>> {
    let meta = std::fs::metadata(path).map_err(|e| Error::config(format!("feeder {}: {e}", path.display())))?;
    if !meta.is_file() || meta.len() > MAX_FEEDER_BYTES {
        return Err(Error::config(format!("feeder {} must be a regular file up to 256 MiB", path.display())));
    }
    let raw = std::fs::read_to_string(path)?;
    let records = parse_csv(&raw)?;
    let mut it = records.into_iter();
    let header = it.next().ok_or_else(|| Error::config(format!("feeder {} is empty", path.display())))?;
    if header.is_empty() || header.len() > MAX_COLUMNS || header.iter().any(|h| h.trim().is_empty()) {
        return Err(Error::config(format!("feeder {} has an invalid header", path.display())));
    }
    let header: Vec<String> = header.into_iter().map(|h| h.trim().to_owned()).collect();
    let mut rows = Vec::new();
    for (line, rec) in it.enumerate() {
        if rec.len() == 1 && rec[0].is_empty() {
            continue;
        }
        if rec.len() != header.len() {
            return Err(Error::config(format!(
                "feeder {} row {} has {} fields, expected {}",
                path.display(),
                line + 2,
                rec.len(),
                header.len()
            )));
        }
        if rows.len() >= MAX_ROWS {
            return Err(Error::config("feeder exceeds 10M rows"));
        }
        rows.push(Arc::new(header.iter().cloned().zip(rec).collect()));
    }
    Ok(rows)
}

/// RFC 4180 parser: quoted fields, doubled quotes, CRLF or LF line endings.
pub fn parse_csv(input: &str) -> Result<Vec<Vec<String>>> {
    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => in_quotes = false,
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => in_quotes = true,
            ',' => record.push(std::mem::take(&mut field)),
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' => {
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
            }
            _ => field.push(c),
        }
    }
    if in_quotes {
        return Err(Error::config("unterminated quoted field in CSV"));
    }
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partitions_rows_between_workers() {
        let rows: Vec<Arc<FeedRow>> = (0..5).map(|i| Arc::new(FeedRow::from([("n".to_owned(), i.to_string())]))).collect();
        let f = Feeder { rows, cursor: AtomicUsize::new(0), mode: FeederMode::Sequential, partition: AtomicU64::new(1) };
        f.partition.store((1 << 32) | 2, Ordering::Relaxed);
        let got: Vec<String> = (0..3).map(|_| f.next().expect("row")["n"].clone()).collect();
        assert_eq!(got, ["1", "3", "1"]);
    }

    #[test]
    fn parses_quotes_and_crlf() {
        let r = parse_csv("a,b\r\n\"x,1\",\"say \"\"hi\"\"\"\r\n").expect("csv");
        assert_eq!(r, vec![vec!["a", "b"], vec!["x,1", "say \"hi\""]]);
    }
}
