// File: ballista/src/manager/tokens.rs
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::web::security::ct_eq;

const TOKEN_PREFIX: &str = "bt_";
const MIN_TOKEN_LEN: usize = 32;
const MAX_TOKENS: usize = 256;

/// Permission levels. Each level includes the ones below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Read,
    Run,
    Admin,
}

impl Scope {
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim() {
            "read" => Ok(Self::Read),
            "run" => Ok(Self::Run),
            "admin" => Ok(Self::Admin),
            other => Err(Error::config(format!("unknown scope '{}' (read, run, admin)", crate::error::sanitize(other, 16)))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Run => "run",
            Self::Admin => "admin",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Principal {
    pub name: String,
    pub scope: Scope,
}

struct Entry {
    principal: Principal,
    digest: [u8; 32],
}

/// API tokens, stored only as SHA-256 digests.
pub struct TokenStore {
    entries: Vec<Entry>,
}

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

impl TokenStore {
    /// Parses `name:scope:sha256hex` entries separated by commas or newlines.
    pub fn parse(spec: &str) -> Result<Self> {
        let mut entries = Vec::new();
        for raw in spec.split([',', '\n']).map(str::trim).filter(|s| !s.is_empty()) {
            let mut parts = raw.split(':');
            let (Some(name), Some(scope), Some(hash), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
                return Err(Error::config("api token entries must be name:scope:sha256hex"));
            };
            if !crate::config::is_valid_name(name) {
                return Err(Error::config("api token names must be simple names"));
            }
            let bytes = hex::decode(hash).map_err(|_| Error::config("api token digest must be hex"))?;
            let digest: [u8; 32] = bytes.try_into().map_err(|_| Error::config("api token digest must be 32 bytes"))?;
            if entries.iter().any(|e: &Entry| e.principal.name == name) {
                return Err(Error::config(format!("duplicate api token name '{name}'")));
            }
            entries.push(Entry { principal: Principal { name: name.to_owned(), scope: Scope::parse(scope)? }, digest });
            if entries.len() > MAX_TOKENS {
                return Err(Error::config("too many api tokens"));
            }
        }
        if entries.is_empty() {
            return Err(Error::config("at least one api token is required"));
        }
        Ok(Self { entries })
    }

    /// Resolves a bearer token. Every entry is compared so timing does not reveal which one matched.
    pub fn authenticate(&self, token: &str) -> Option<Principal> {
        if token.len() < MIN_TOKEN_LEN || token.len() > 256 {
            return None;
        }
        let d = digest(token);
        let mut found = None;
        for e in &self.entries {
            if ct_eq(&d, &e.digest) {
                found = Some(e.principal.clone());
            }
        }
        found
    }
}

/// Creates a new random token and the entry line to configure it.
pub fn generate(name: &str, scope: Scope) -> Result<(String, String)> {
    if !crate::config::is_valid_name(name) {
        return Err(Error::config("token name must be 1..64 characters of [A-Za-z0-9_.-]"));
    }
    let mut buf = [0_u8; 32];
    getrandom::fill(&mut buf).map_err(|e| Error::config(format!("random source unavailable: {e}")))?;
    let token = format!("{TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(buf));
    let entry = format!("{name}:{}:{}", scope.as_str(), hex::encode(digest(&token)));
    Ok((token, entry))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_authenticate_with_scope() {
        let (t1, e1) = generate("ci", Scope::Run).expect("token");
        let (t2, e2) = generate("ops", Scope::Admin).expect("token");
        let store = TokenStore::parse(&format!("{e1},\n{e2}")).expect("parse");
        assert_eq!(store.authenticate(&t1).map(|p| p.scope), Some(Scope::Run));
        assert_eq!(store.authenticate(&t2).map(|p| p.name), Some("ops".to_owned()));
        assert!(store.authenticate("bt_wrong_token_value_that_is_long_enough").is_none());
        assert!(Scope::Admin > Scope::Run && Scope::Run > Scope::Read);
        assert!(TokenStore::parse("x:root:00").is_err());
    }
}
