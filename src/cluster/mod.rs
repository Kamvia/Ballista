// File: ballista/src/cluster/mod.rs
pub mod hub;
pub mod worker;

use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{LoadMode, Plan};
use crate::error::{Error, Result};
use crate::metrics::export::WindowExport;
use crate::monitor::GeneratorHealth;

pub const PROTOCOL_VERSION: u32 = 4;
pub const MIN_TOKEN_LEN: usize = 32;
pub const MAX_FRAME: usize = 16 * 1024 * 1024;
/// Bundle transfers (plan plus files, base64) to pool workers may be larger than ordinary frames.
pub const MAX_LOAD_FRAME: usize = 96 * 1024 * 1024;
/// Fingerprint announced by pool workers, which start without a plan and receive one per run.
pub const POOL_FINGERPRINT: &str = "";
pub const CLUSTER_PATH: &str = "/cluster";

/// Messages sent by a worker to the controller.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToController {
    Hello {
        protocol: u32,
        version: String,
        name: String,
        fingerprint: String,
        cores: u32,
    },
    Report {
        run: u64,
        active: u64,
        iterations: u64,
        health: GeneratorHealth,
        window: WindowExport,
    },
    Stopped {
        run: u64,
    },
    /// A virtual user wrote shared state; the controller relays it to every other worker.
    Shared {
        key: String,
        value: serde_json::Value,
    },
    /// Result of a `Load`: the fingerprint of the plan as built on the worker, or the reason it failed.
    Loaded {
        load: u64,
        fingerprint: String,
        error: Option<String>,
    },
}

/// A file of a bundle sent to pool workers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadFile {
    pub path: String,
    /// Base64 content.
    pub data: String,
}

/// Run overrides a pool worker applies after loading a bundle (they affect tags, host and fingerprint).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadOverrides {
    pub users: Option<f64>,
    pub spawn_rate: Option<f64>,
    pub run_time: Option<String>,
    pub iterations: Option<u64>,
    pub tags: Option<Vec<String>>,
    pub exclude_tags: Option<Vec<String>>,
    pub host: Option<String>,
}

/// Messages sent by the controller to a worker.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToWorker {
    Welcome {
        id: u64,
    },
    Reject {
        reason: String,
    },
    Start {
        run: u64,
        index: u32,
        count: u32,
        iteration_limit: Option<u64>,
        scenarios: Option<Vec<String>>,
        host: Option<String>,
    },
    /// Worker membership changed during a run: new feeder partition and fixed-count share.
    Repartition {
        run: u64,
        index: u32,
        count: u32,
    },
    Shared {
        key: String,
        value: serde_json::Value,
    },
    Target {
        run: u64,
        target: f64,
    },
    Stop {
        run: u64,
    },
    /// The controller is shutting down; the worker should exit.
    Quit,
    /// Pool workers only: replace the current plan with this bundle and report `Loaded`.
    Load {
        load: u64,
        plan: String,
        files: Vec<LoadFile>,
        overrides: LoadOverrides,
    },
}

pub fn validate_token(token: &str) -> Result<()> {
    if token.len() < MIN_TOKEN_LEN || token.len() > 512 || !token.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(Error::config(format!(
            "cluster token must be {MIN_TOKEN_LEN}..512 printable ASCII characters (set BALLISTA_CLUSTER_TOKEN)"
        )));
    }
    Ok(())
}

fn hash_file(h: &mut Sha256, label: &str, path: &Path) -> Result<()> {
    let bytes = std::fs::read(path).map_err(|e| Error::config(format!("cannot read {} for fingerprint: {e}", path.display())))?;
    h.update(label.as_bytes());
    h.update((bytes.len() as u64).to_le_bytes());
    h.update(&bytes);
    Ok(())
}

/// Digest over the plan source, every script, shape and feeder file, and effective target and tag overrides.
pub fn fingerprint(plan: &Plan) -> Result<String> {
    let mut h = Sha256::new();
    h.update(plan.source_digest);
    for s in &plan.scenarios {
        h.update(s.name.as_bytes());
        if let Some(p) = &s.script {
            hash_file(&mut h, "script", p)?;
        }
        if let Some(n) = &s.native {
            h.update(b"native");
            h.update(n.as_bytes());
        }
    }
    if let Some(p) = &plan.load.shape_script {
        hash_file(&mut h, "shape", p)?;
    }
    for f in &plan.feeders {
        h.update(f.name.as_bytes());
        hash_file(&mut h, "feeder", &f.path)?;
    }
    h.update(plan.target.base_url.as_str().as_bytes());
    for t in plan.load.tags.iter().chain(std::iter::once(&"|".to_owned())).chain(plan.load.exclude_tags.iter()) {
        h.update(t.as_bytes());
        h.update(b",");
    }
    Ok(hex::encode(h.finalize()))
}

/// Splits a total target across `n` workers. Users are integral; arrival rates are fractional.
pub fn split_target(total: f64, n: usize, mode: LoadMode) -> Vec<f64> {
    if n == 0 {
        return Vec::new();
    }
    match mode {
        LoadMode::ArrivalRate => vec![total / n as f64; n],
        LoadMode::Users => {
            let whole = total.round().max(0.0) as u64;
            let base = whole / n as u64;
            let extra = (whole % n as u64) as usize;
            (0..n).map(|i| (base + u64::from(i < extra)) as f64).collect()
        }
    }
}

/// Splits an iteration limit across `n` workers so the shares add up exactly.
pub fn split_limit(limit: Option<u64>, n: usize) -> Vec<Option<u64>> {
    match limit {
        None => vec![None; n],
        Some(l) => {
            let base = l / n as u64;
            let extra = (l % n as u64) as usize;
            (0..n).map(|i| Some(base + u64::from(i < extra))).collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_users_exactly() {
        assert_eq!(split_target(10.0, 3, LoadMode::Users), vec![4.0, 3.0, 3.0]);
        assert_eq!(split_target(9.0, 3, LoadMode::ArrivalRate), vec![3.0, 3.0, 3.0]);
        assert_eq!(split_limit(Some(7), 2), vec![Some(4), Some(3)]);
    }

    #[test]
    fn rejects_weak_token() {
        assert!(validate_token("short").is_err());
        assert!(validate_token(&"a".repeat(40)).is_ok());
    }

    #[test]
    fn protocol_rejects_unknown_fields() {
        let bad = r#"{"type":"stopped","run":1,"evil":true}"#;
        assert!(serde_json::from_str::<ToController>(bad).is_err());
    }
}
