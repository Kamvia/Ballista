// File: ballista/src/web/auth.rs
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use axum::Json;
use axum::extract::{ConnectInfo, FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::AppState;
use super::api::ApiError;
use super::security::{ct_eq, retry_after};
use crate::error::{Error, Result};

const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const ABSOLUTE_TIMEOUT: Duration = Duration::from_secs(12 * 3600);
const MAX_SESSIONS: usize = 32;
const MAX_PASSWORD_LEN: usize = 1024;
const CSRF_HEADER: &str = "x-csrf-token";

fn random_token(bytes: usize) -> Result<String> {
    let mut buf = vec![0_u8; bytes];
    getrandom::fill(&mut buf).map_err(|e| Error::Engine(format!("system rng unavailable: {e}")))?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

pub fn hash_password(password: &str) -> Result<String> {
    if password.len() < 12 || password.len() > MAX_PASSWORD_LEN {
        return Err(Error::config("password must be 12..1024 bytes"));
    }
    let h: PasswordHash =
        Argon2::default().hash_password(password.as_bytes()).map_err(|e| Error::config(format!("hashing failed: {e}")))?;
    Ok(h.to_string())
}

pub fn generate_password() -> Result<String> {
    random_token(18)
}

struct Session {
    csrf: String,
    created: Instant,
    last_seen: Instant,
}

pub(crate) struct Auth {
    hash: String,
    sessions: Mutex<HashMap<[u8; 32], Session>>,
}

impl Auth {
    pub fn new(phc_hash: String) -> Result<Self> {
        PasswordHash::new(&phc_hash).map_err(|e| Error::config(format!("invalid admin password hash: {e}")))?;
        Ok(Self { hash: phc_hash, sessions: Mutex::new(HashMap::new()) })
    }

    async fn verify(self: &Arc<Self>, password: String) -> bool {
        let me = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            PasswordHash::new(&me.hash)
                .map(|parsed| Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
                .unwrap_or(false)
        })
        .await
        .unwrap_or(false)
    }

    fn create(&self) -> Result<(String, String)> {
        let token = random_token(32)?;
        let csrf = random_token(32)?;
        let now = Instant::now();
        let mut map = self.sessions.lock();
        map.retain(|_, s| now.duration_since(s.last_seen) < IDLE_TIMEOUT && now.duration_since(s.created) < ABSOLUTE_TIMEOUT);
        if map.len() >= MAX_SESSIONS
            && let Some(oldest) = map.iter().min_by_key(|(_, s)| s.last_seen).map(|(k, _)| *k)
        {
            map.remove(&oldest);
        }
        map.insert(digest(&token), Session { csrf: csrf.clone(), created: now, last_seen: now });
        Ok((token, csrf))
    }

    fn validate(&self, token: &str) -> Option<String> {
        let key = digest(token);
        let now = Instant::now();
        let mut map = self.sessions.lock();
        let alive = map
            .get(&key)
            .is_some_and(|s| now.duration_since(s.last_seen) < IDLE_TIMEOUT && now.duration_since(s.created) < ABSOLUTE_TIMEOUT);
        if !alive {
            map.remove(&key);
            return None;
        }
        let s = map.get_mut(&key)?;
        s.last_seen = now;
        Some(s.csrf.clone())
    }

    pub fn is_valid(&self, token: &str) -> bool {
        let key = digest(token);
        let now = Instant::now();
        self.sessions
            .lock()
            .get(&key)
            .is_some_and(|s| now.duration_since(s.last_seen) < IDLE_TIMEOUT && now.duration_since(s.created) < ABSOLUTE_TIMEOUT)
    }

    fn revoke(&self, token: &str) {
        self.sessions.lock().remove(&digest(token));
    }
}

pub(crate) fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
        .filter(|v| !v.is_empty() && v.len() <= 128)
}

fn session_cookie(state: &AppState, token: &str, max_age: u64) -> HeaderValue {
    let secure = if state.settings.tls { "; Secure" } else { "" };
    let v = format!("{}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{secure}", state.settings.cookie_name);
    HeaderValue::from_str(&v).unwrap_or_else(|_| HeaderValue::from_static(""))
}

/// Authenticated session. For unsafe methods the CSRF header must match the session token.
pub(crate) struct Authed {
    pub token: String,
    pub csrf: String,
}

impl FromRequestParts<Arc<AppState>> for Authed {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &Arc<AppState>) -> std::result::Result<Self, Self::Rejection> {
        let token = cookie_value(&parts.headers, state.settings.cookie_name)
            .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "authentication required").into_response())?
            .to_owned();
        let csrf = state.auth.validate(&token).ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "session expired").into_response())?;
        let safe = matches!(parts.method, Method::GET | Method::HEAD | Method::OPTIONS);
        if !safe {
            let sent = parts.headers.get(CSRF_HEADER).and_then(|v| v.to_str().ok()).unwrap_or("");
            if !ct_eq(sent.as_bytes(), csrf.as_bytes()) {
                return Err(ApiError::new(StatusCode::FORBIDDEN, "invalid csrf token").into_response());
            }
        }
        Ok(Self { token, csrf })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LoginBody {
    password: String,
}

#[derive(Serialize)]
pub(crate) struct CsrfBody {
    csrf: String,
}

pub(crate) async fn login(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<LoginBody>,
) -> Response {
    if !state.login_limiter.allow(addr.ip()) {
        let mut r = ApiError::new(StatusCode::TOO_MANY_REQUESTS, "too many login attempts").into_response();
        r.headers_mut().insert(header::RETRY_AFTER, retry_after(Duration::from_secs(60)));
        return r;
    }
    if body.password.is_empty() || body.password.len() > MAX_PASSWORD_LEN {
        return ApiError::new(StatusCode::UNAUTHORIZED, "invalid credentials").into_response();
    }
    if !state.auth.verify(body.password).await {
        tracing::warn!(client = %addr.ip(), "failed dashboard login");
        return ApiError::new(StatusCode::UNAUTHORIZED, "invalid credentials").into_response();
    }
    let Ok((token, csrf)) = state.auth.create() else {
        return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response();
    };
    tracing::info!(client = %addr.ip(), "dashboard login");
    let mut res = Json(CsrfBody { csrf }).into_response();
    res.headers_mut().insert(header::SET_COOKIE, session_cookie(&state, &token, ABSOLUTE_TIMEOUT.as_secs()));
    res
}

pub(crate) async fn logout(State(state): State<Arc<AppState>>, auth: Authed) -> Response {
    state.auth.revoke(&auth.token);
    let mut res = StatusCode::NO_CONTENT.into_response();
    res.headers_mut().insert(header::SET_COOKIE, session_cookie(&state, "", 0));
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_and_session_roundtrip() {
        let h = hash_password("correct horse battery").expect("hash");
        let auth = Auth::new(h).expect("auth");
        let (token, csrf) = auth.create().expect("session");
        assert_eq!(auth.validate(&token).as_deref(), Some(csrf.as_str()));
        auth.revoke(&token);
        assert!(auth.validate(&token).is_none());
    }

    #[test]
    fn rejects_short_password() {
        assert!(hash_password("short").is_err());
    }
}
