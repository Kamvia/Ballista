// File: ballista/src/report/mod.rs
mod html;

use std::path::Path;
use std::time::SystemTime;

use serde::Serialize;

use crate::config::{LoadMode, ReportConfig};
use crate::engine::Engine;
use crate::error::Result;
use crate::metrics::Snapshot;
use crate::threshold::all_passed;

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub tool: &'static str,
    pub version: &'static str,
    pub plan: String,
    pub target: String,
    pub mode: LoadMode,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub duration_secs: f64,
    pub passed: bool,
    pub aborted: bool,
    pub generator_saturated_secs: u64,
    pub snapshot: Snapshot,
}

fn fmt_time(t: Option<SystemTime>) -> Option<String> {
    t.map(|t| humantime::format_rfc3339_seconds(t).to_string())
}

impl Report {
    pub fn from_engine(engine: &Engine) -> Self {
        let snapshot = engine.full_snapshot();
        let (start, end) = engine.times();
        let mut target = engine.plan().target.base_url.clone();
        target.set_query(None);
        target.set_fragment(None);
        let passed = all_passed(&snapshot.thresholds) && !snapshot.aborted;
        let saturated_secs = snapshot.generator.saturated_secs;
        Self {
            tool: "ballista",
            version: env!("CARGO_PKG_VERSION"),
            plan: snapshot.plan.clone(),
            target: target.to_string(),
            mode: snapshot.mode,
            started_at: fmt_time(start),
            ended_at: fmt_time(end),
            duration_secs: snapshot.elapsed_secs,
            passed,
            aborted: snapshot.aborted,
            generator_saturated_secs: saturated_secs,
            snapshot,
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_owned())
    }

    pub fn to_html(&self) -> String {
        html::render(self)
    }

    pub fn write(&self, cfg: &ReportConfig) -> Result<()> {
        if let Some(p) = &cfg.json {
            write_atomic(p, self.to_json().as_bytes())?;
            tracing::info!(path = %p.display(), "json report written");
        }
        if let Some(p) = &cfg.html {
            write_atomic(p, self.to_html().as_bytes())?;
            tracing::info!(path = %p.display(), "html report written");
        }
        if let Some(prefix) = &cfg.csv {
            for (path, kind) in crate::csv::csv_paths(prefix) {
                let body = match kind {
                    "stats" => crate::csv::stats_csv(&self.snapshot),
                    "history" => crate::csv::history_csv(&self.snapshot),
                    _ => crate::csv::failures_csv(&self.snapshot),
                };
                write_atomic(&path, body.as_bytes())?;
            }
            tracing::info!(prefix = %prefix.display(), "csv reports written");
        }
        Ok(())
    }
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
