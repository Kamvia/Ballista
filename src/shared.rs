// File: ballista/src/shared.rs
use std::collections::HashMap;

use parking_lot::{Mutex, RwLock};
use tokio::sync::mpsc;

use crate::config::is_valid_name;
use crate::error::{Error, Result};

pub const MAX_KEYS: usize = 1000;
pub const MAX_VALUE_BYTES: usize = 64 * 1024;

/// Key/value state shared by all virtual users; replicated to every worker in distributed runs.
#[derive(Default)]
pub struct SharedStore {
    map: RwLock<HashMap<String, serde_json::Value>>,
    outbound: Mutex<Option<mpsc::Sender<(String, serde_json::Value)>>>,
}

pub fn validate(key: &str, value: &serde_json::Value) -> Result<()> {
    if !is_valid_name(key) {
        return Err(Error::check("shared key must be 1..64 characters of [A-Za-z0-9_.-]"));
    }
    let size = serde_json::to_vec(value).map(|v| v.len()).unwrap_or(usize::MAX);
    if size > MAX_VALUE_BYTES {
        return Err(Error::check("shared value exceeds 64 KiB"));
    }
    Ok(())
}

impl SharedStore {
    /// Forwards local writes to the controller (worker role).
    pub fn set_outbound(&self, tx: mpsc::Sender<(String, serde_json::Value)>) {
        *self.outbound.lock() = Some(tx);
    }

    fn insert(&self, key: String, value: serde_json::Value) -> Result<()> {
        let mut map = self.map.write();
        if !map.contains_key(&key) && map.len() >= MAX_KEYS {
            return Err(Error::check("shared store holds the maximum number of keys"));
        }
        map.insert(key, value);
        Ok(())
    }

    pub fn set(&self, key: &str, value: serde_json::Value) -> Result<()> {
        validate(key, &value)?;
        self.insert(key.to_owned(), value.clone())?;
        if let Some(tx) = self.outbound.lock().as_ref()
            && tx.try_send((key.to_owned(), value)).is_err()
        {
            tracing::warn!("shared state update could not be forwarded to the controller");
        }
        Ok(())
    }

    /// Applies an update received from the controller without forwarding it again.
    pub fn apply_remote(&self, key: String, value: serde_json::Value) -> Result<()> {
        validate(&key, &value)?;
        self.insert(key, value)
    }

    pub fn get(&self, key: &str) -> Option<serde_json::Value> {
        self.map.read().get(key).cloned()
    }

    pub fn clear(&self) {
        self.map.write().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enforces_limits() {
        let s = SharedStore::default();
        assert!(s.set("token", serde_json::json!("abc")).is_ok());
        assert_eq!(s.get("token"), Some(serde_json::json!("abc")));
        assert!(s.set("bad key", serde_json::json!(1)).is_err());
        assert!(s.set("big", serde_json::json!("x".repeat(70_000))).is_err());
    }
}
