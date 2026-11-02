// File: ballista/src/manager/bundles.rs
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result, sanitize};

pub const PLAN_FILE: &str = "plan.toml";
const META_FILE: &str = "bundle.json";
const MAX_FILES: usize = 256;
const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;
const MAX_PLAN_BYTES: usize = 1024 * 1024;
const MAX_PATH_LEN: usize = 200;
const MAX_DEPTH: usize = 8;

/// Upload body: the plan source and supporting files (base64), keyed by relative path.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleUpload {
    /// Structured plan document or plan file text.
    pub plan: super::document::PlanSource,
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleInfo {
    pub id: String,
    pub plan_name: String,
    pub scenarios: Vec<String>,
    pub files: Vec<BundleFile>,
    pub size: u64,
    pub labels: BTreeMap<String, String>,
    pub created_at: String,
    pub created_by: String,
}

/// Accepts only plain relative paths: `[A-Za-z0-9._-]` segments, no dot-prefixed segments.
pub fn valid_path(p: &str) -> bool {
    if p.is_empty() || p.len() > MAX_PATH_LEN || p.starts_with('/') {
        return false;
    }
    let segs: Vec<&str> = p.split('/').collect();
    segs.len() <= MAX_DEPTH
        && segs.iter().all(|s| {
            !s.is_empty()
                && !s.starts_with('.')
                && s.len() <= 100
                && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        })
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

pub(crate) fn valid_labels(labels: &BTreeMap<String, String>) -> Result<()> {
    if labels.len() > 32 {
        return Err(Error::config("at most 32 labels"));
    }
    for (k, v) in labels {
        if !crate::config::is_valid_name(k) || v.len() > 256 || v.chars().any(char::is_control) {
            return Err(Error::config("label keys must be simple names and values up to 256 printable characters"));
        }
    }
    Ok(())
}

pub struct BundleStore {
    root: PathBuf,
}

/// Decoded, validated upload ready to be written.
pub struct Prepared {
    pub id: String,
    plan: String,
    files: Vec<(String, Vec<u8>)>,
    pub labels: BTreeMap<String, String>,
}

impl BundleStore {
    pub fn open(root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&root)?;
        for e in std::fs::read_dir(&root)?.flatten() {
            if e.file_name().to_string_lossy().starts_with(".staging-") {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
        Ok(Self { root })
    }

    pub fn dir(&self, id: &str) -> Option<PathBuf> {
        if !valid_id(id) {
            return None;
        }
        let d = self.root.join(id);
        d.join(META_FILE).is_file().then_some(d)
    }

    /// Validates paths and sizes and computes the content-addressed id.
    pub fn prepare(&self, up: BundleUpload) -> Result<Prepared> {
        let (plan, inline) = up.plan.materialize()?;
        if plan.len() > MAX_PLAN_BYTES {
            return Err(Error::config("plan exceeds 1 MiB"));
        }
        if up.files.len() > MAX_FILES {
            return Err(Error::config(format!("bundle holds at most {MAX_FILES} files")));
        }
        valid_labels(&up.labels)?;
        let mut total = plan.len();
        let mut files = Vec::with_capacity(up.files.len());
        for (path, b64) in up.files {
            if !valid_path(&path) || path == PLAN_FILE || path == META_FILE {
                return Err(Error::config(format!("invalid bundle path '{}'", sanitize(&path, 80))));
            }
            if b64.len() > MAX_FILE_BYTES / 3 * 4 + 4 {
                return Err(Error::config(format!("file '{}' exceeds 16 MiB", sanitize(&path, 80))));
            }
            let data = STANDARD
                .decode(b64.as_bytes())
                .map_err(|_| Error::config(format!("file '{}' is not valid base64", sanitize(&path, 80))))?;
            total += data.len();
            if total > MAX_TOTAL_BYTES {
                return Err(Error::config("bundle exceeds 64 MiB"));
            }
            files.push((path, data));
        }
        for (path, data) in inline {
            if files.iter().any(|(p, _)| *p == path) {
                return Err(Error::config(format!("'{path}' is generated from inline content and cannot also be uploaded")));
            }
            total += data.len();
            if total > MAX_TOTAL_BYTES {
                return Err(Error::config("bundle exceeds 64 MiB"));
            }
            files.push((path, data));
        }
        files.sort_by(|a, b| a.0.cmp(&b.0));
        let prefix_conflict = files.iter().any(|(a, _)| files.iter().any(|(b, _)| b.starts_with(&format!("{a}/"))));
        if prefix_conflict {
            return Err(Error::config("a bundle path is used both as a file and a directory"));
        }
        let mut h = Sha256::new();
        h.update(b"plan\0");
        h.update((plan.len() as u64).to_le_bytes());
        h.update(plan.as_bytes());
        for (p, d) in &files {
            h.update(p.as_bytes());
            h.update(b"\0");
            h.update((d.len() as u64).to_le_bytes());
            h.update(d);
        }
        let id = hex::encode(&h.finalize()[..16]);
        Ok(Prepared { id, plan, files, labels: up.labels })
    }

    /// Writes a prepared bundle to a staging directory, returning its path.
    pub fn stage(&self, p: &Prepared) -> Result<PathBuf> {
        let mut rnd = [0_u8; 8];
        getrandom::fill(&mut rnd).map_err(|e| Error::Engine(format!("system rng unavailable: {e}")))?;
        let dir = self.root.join(format!(".staging-{}", hex::encode(rnd)));
        std::fs::create_dir(&dir)?;
        let write = |rel: &str, data: &[u8]| -> Result<()> {
            let full = dir.join(rel);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&full)?;
            std::io::Write::write_all(&mut f, data)?;
            Ok(())
        };
        let res = (|| {
            write(PLAN_FILE, p.plan.as_bytes())?;
            for (rel, data) in &p.files {
                write(rel, data)?;
            }
            Ok(())
        })();
        if let Err(e) = res {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
        Ok(dir)
    }

    /// Moves a validated staging directory into place. Returns false when the bundle already existed.
    pub fn commit(&self, staging: &Path, info: &BundleInfo) -> Result<bool> {
        let final_dir = self.root.join(&info.id);
        if final_dir.join(META_FILE).is_file() {
            let _ = std::fs::remove_dir_all(staging);
            return Ok(false);
        }
        let meta = serde_json::to_vec_pretty(info).map_err(|e| Error::Engine(e.to_string()))?;
        std::fs::write(staging.join(META_FILE), meta)?;
        if final_dir.exists() {
            std::fs::remove_dir_all(&final_dir)?;
        }
        std::fs::rename(staging, &final_dir)?;
        Ok(true)
    }

    pub fn discard(&self, staging: &Path) {
        let _ = std::fs::remove_dir_all(staging);
    }

    pub fn info(&self, id: &str) -> Option<BundleInfo> {
        let d = self.dir(id)?;
        let raw = std::fs::read(d.join(META_FILE)).ok()?;
        serde_json::from_slice(&raw).ok()
    }

    pub fn list(&self) -> Vec<BundleInfo> {
        let mut out: Vec<BundleInfo> = std::fs::read_dir(&self.root)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| self.info(&e.file_name().to_string_lossy()))
            .collect();
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        out
    }

    pub fn delete(&self, id: &str) -> Result<bool> {
        match self.dir(id) {
            Some(d) => {
                std::fs::remove_dir_all(d)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub fn file_list(p: &Prepared) -> (Vec<BundleFile>, u64) {
        let mut total = p.plan.len() as u64;
        let files = p
            .files
            .iter()
            .map(|(path, d)| {
                total += d.len() as u64;
                BundleFile { path: path.clone(), size: d.len() as u64, sha256: hex::encode(Sha256::digest(d)) }
            })
            .collect();
        (files, total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_paths() {
        for bad in ["", "/etc/passwd", "../x", "a/../b", ".hidden", "a//b", "a\\b", "a b", "a/.git/c"] {
            assert!(!valid_path(bad), "{bad}");
        }
        for good in ["scenarios/shop.rhai", "data/users-1.csv", "proto/api.pb"] {
            assert!(valid_path(good), "{good}");
        }
        assert!(valid_id("0123456789abcdef0123456789abcdef"));
        assert!(!valid_id("../0123456789abcdef0123456789abcd"));
    }
}
