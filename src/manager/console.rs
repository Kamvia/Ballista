// File: ballista/src/manager/console.rs
//! Browser console for the run manager. A user signs in with an API token, which is exchanged for an
//! HttpOnly session cookie; the console then calls the same `/api/v1` endpoints with the token's scope.
//! Unsafe requests made with a session cookie must carry the session's CSRF token and come from the same origin.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::ManagerState;
use super::handlers::ApiError;
use super::tokens::Principal;
use crate::error::{Error, Result};

const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const ABSOLUTE_TIMEOUT: Duration = Duration::from_secs(12 * 3600);
const MAX_SESSIONS: usize = 256;
const MAX_TOKEN_LEN: usize = 512;
pub const CSRF_HEADER: &str = "x-csrf-token";

const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; \
base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

const INDEX_HTML: &str = include_str!("../../assets/console/index.html");
const LOGIN_HTML: &str = include_str!("../../assets/console/login.html");
const CONSOLE_JS: &str = include_str!("../../assets/console/console.js");
const LOGIN_JS: &str = include_str!("../../assets/console/consoleLogin.js");
const CONSOLE_CSS: &str = include_str!("../../assets/console/console.css");

fn random_token(bytes: usize) -> Result<String> {
    let mut buf = vec![0_u8; bytes];
    getrandom::fill(&mut buf).map_err(|e| Error::Engine(format!("system rng unavailable: {e}")))?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

struct Session {
    principal: Principal,
    csrf: String,
    created: Instant,
    last_seen: Instant,
}

impl Session {
    fn alive(&self, now: Instant) -> bool {
        now.duration_since(self.last_seen) < IDLE_TIMEOUT && now.duration_since(self.created) < ABSOLUTE_TIMEOUT
    }
}

/// Server-side sessions keyed by the SHA-256 of the cookie value; the cookie itself is never stored.
pub struct Sessions {
    map: Mutex<HashMap<[u8; 32], Session>>,
    tls: bool,
}

/// A valid session: who it belongs to and the CSRF token unsafe requests must present.
pub struct SessionInfo {
    pub principal: Principal,
    pub csrf: String,
}

impl Sessions {
    pub fn new(tls: bool) -> Self {
        Self { map: Mutex::new(HashMap::new()), tls }
    }

    /// `__Host-` binds the cookie to this exact origin when it can be marked Secure.
    pub fn cookie_name(&self) -> &'static str {
        if self.tls { "__Host-console" } else { "console_session" }
    }

    fn create(&self, principal: Principal) -> Result<(String, String)> {
        let token = random_token(32)?;
        let csrf = random_token(32)?;
        let now = Instant::now();
        let mut map = self.map.lock();
        map.retain(|_, s| s.alive(now));
        if map.len() >= MAX_SESSIONS
            && let Some(oldest) = map.iter().min_by_key(|(_, s)| s.last_seen).map(|(k, _)| *k)
        {
            map.remove(&oldest);
        }
        map.insert(digest(&token), Session { principal, csrf: csrf.clone(), created: now, last_seen: now });
        Ok((token, csrf))
    }

    pub fn validate(&self, token: &str) -> Option<SessionInfo> {
        let key = digest(token);
        let now = Instant::now();
        let mut map = self.map.lock();
        if !map.get(&key).is_some_and(|s| s.alive(now)) {
            map.remove(&key);
            return None;
        }
        let s = map.get_mut(&key)?;
        s.last_seen = now;
        Some(SessionInfo { principal: s.principal.clone(), csrf: s.csrf.clone() })
    }

    fn revoke(&self, token: &str) {
        self.map.lock().remove(&digest(token));
    }

    fn cookie(&self, value: &str, max_age: u64) -> HeaderValue {
        let secure = if self.tls { "; Secure" } else { "" };
        let v = format!("{}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{secure}", self.cookie_name());
        HeaderValue::from_str(&v).unwrap_or_else(|_| HeaderValue::from_static("console_session=; Path=/; Max-Age=0"))
    }
}

/// The session cookie value, if present and well formed.
pub fn session_cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
        .filter(|v| !v.is_empty() && v.len() <= 128 && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
}

fn expected_origin(headers: &HeaderMap, tls: bool) -> Option<String> {
    let host = headers.get(header::HOST)?.to_str().ok()?;
    if host.is_empty() || host.len() > 255 || host.contains(['/', '@', ' ', '\\']) {
        return None;
    }
    Some(format!("{}://{}", if tls { "https" } else { "http" }, host.to_ascii_lowercase()))
}

/// Cross-site checks for a cookie-authenticated request: Fetch Metadata, Origin, then the CSRF token.
pub fn check_unsafe(method: &Method, headers: &HeaderMap, csrf: &str, tls: bool) -> std::result::Result<(), ApiError> {
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return Ok(());
    }
    if let Some(site) = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok())
        && !matches!(site, "same-origin" | "none")
    {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "forbidden", "cross-site request rejected"));
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        let ok = origin.to_str().ok().zip(expected_origin(headers, tls)).is_some_and(|(o, e)| o.eq_ignore_ascii_case(&e));
        if !ok {
            return Err(ApiError::new(StatusCode::FORBIDDEN, "forbidden", "origin mismatch"));
        }
    }
    let sent = headers.get(CSRF_HEADER).and_then(|v| v.to_str().ok()).unwrap_or("");
    if !ct_eq(sent.as_bytes(), csrf.as_bytes()) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "forbidden", "invalid csrf token"));
    }
    Ok(())
}

fn respond(body: &'static str, content_type: &'static str, cache: &'static str) -> Response {
    let mut r = body.into_response();
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
    h.insert("cross-origin-opener-policy", HeaderValue::from_static("same-origin"));
    h.insert("permissions-policy", HeaderValue::from_static("camera=(), microphone=(), geolocation=(), payment=(), usb=()"));
    r
}

fn signed_in(st: &ManagerState, headers: &HeaderMap) -> bool {
    session_cookie(headers, st.sessions.cookie_name()).is_some_and(|t| st.sessions.validate(t).is_some())
}

pub async fn root() -> Redirect {
    Redirect::to("/console/")
}

pub async fn index(State(st): State<Arc<ManagerState>>, headers: HeaderMap) -> Response {
    if !signed_in(&st, &headers) {
        return Redirect::to("/console/login").into_response();
    }
    respond(INDEX_HTML, "text/html; charset=utf-8", "no-store")
}

pub async fn login_page() -> Response {
    respond(LOGIN_HTML, "text/html; charset=utf-8", "no-store")
}

pub async fn asset(Path(name): Path<String>) -> Response {
    match name.as_str() {
        "console.js" => respond(CONSOLE_JS, "text/javascript; charset=utf-8", "no-cache"),
        "consoleLogin.js" => respond(LOGIN_JS, "text/javascript; charset=utf-8", "no-cache"),
        "console.css" => respond(CONSOLE_CSS, "text/css; charset=utf-8", "no-cache"),
        _ => ApiError::new(StatusCode::NOT_FOUND, "not_found", "asset not found").into_response(),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginBody {
    token: String,
}

#[derive(Serialize)]
pub struct SessionBody {
    name: String,
    scope: &'static str,
    csrf: String,
}

pub async fn login(
    State(st): State<Arc<ManagerState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<LoginBody>,
) -> Response {
    if let Some(site) = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok())
        && !matches!(site, "same-origin" | "none")
    {
        return ApiError::new(StatusCode::FORBIDDEN, "forbidden", "cross-site request rejected").into_response();
    }
    let token = body.token.trim();
    let principal = if token.is_empty() || token.len() > MAX_TOKEN_LEN { None } else { st.tokens.authenticate(token) };
    let Some(principal) = principal else {
        if !st.auth_failures.allow(addr.ip()) {
            let mut res = ApiError::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "too many failed sign-ins").into_response();
            res.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("30"));
            return res;
        }
        tracing::warn!(client = %addr.ip(), "failed console sign-in");
        return ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized", "invalid token").into_response();
    };
    let Ok((session, csrf)) = st.sessions.create(principal.clone()) else {
        return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", "internal error").into_response();
    };
    tracing::info!(client = %addr.ip(), name = %principal.name, scope = principal.scope.as_str(), "console sign-in");
    let mut res = Json(SessionBody { name: principal.name, scope: principal.scope.as_str(), csrf }).into_response();
    res.headers_mut().insert(header::SET_COOKIE, st.sessions.cookie(&session, ABSOLUTE_TIMEOUT.as_secs()));
    res
}

/// Current session and its CSRF token; the console calls this on load.
pub async fn session(State(st): State<Arc<ManagerState>>, headers: HeaderMap) -> Response {
    let info = session_cookie(&headers, st.sessions.cookie_name()).and_then(|t| st.sessions.validate(t));
    match info {
        Some(s) => Json(SessionBody { name: s.principal.name, scope: s.principal.scope.as_str(), csrf: s.csrf }).into_response(),
        None => ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized", "not signed in").into_response(),
    }
}

pub async fn logout(State(st): State<Arc<ManagerState>>, method: Method, headers: HeaderMap) -> Response {
    let name = st.sessions.cookie_name();
    if let Some(token) = session_cookie(&headers, name) {
        let Some(info) = st.sessions.validate(token) else {
            let mut res = StatusCode::NO_CONTENT.into_response();
            res.headers_mut().insert(header::SET_COOKIE, st.sessions.cookie("", 0));
            return res;
        };
        if let Err(e) = check_unsafe(&method, &headers, &info.csrf, st.tls) {
            return e.into_response();
        }
        st.sessions.revoke(token);
    }
    let mut res = StatusCode::NO_CONTENT.into_response();
    res.headers_mut().insert(header::SET_COOKIE, st.sessions.cookie("", 0));
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::tokens::Scope;

    fn principal() -> Principal {
        Principal { name: "ops".into(), scope: Scope::Admin }
    }

    #[test]
    fn sessions_roundtrip_and_revoke() {
        let s = Sessions::new(false);
        let (token, csrf) = s.create(principal()).expect("session");
        let info = s.validate(&token).expect("valid");
        assert_eq!(info.csrf, csrf);
        assert_eq!(info.principal.scope, Scope::Admin);
        assert!(s.validate("other").is_none());
        s.revoke(&token);
        assert!(s.validate(&token).is_none());
        assert_eq!(Sessions::new(true).cookie_name(), "__Host-console");
    }

    #[test]
    fn session_count_is_bounded() {
        let s = Sessions::new(false);
        let first = s.create(principal()).expect("first").0;
        for _ in 0..MAX_SESSIONS {
            s.create(principal()).expect("session");
        }
        assert!(s.map.lock().len() <= MAX_SESSIONS);
        assert!(s.validate(&first).is_none(), "oldest session evicted");
    }

    #[test]
    fn cookie_parsing_rejects_odd_values() {
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, HeaderValue::from_static("a=1; console_session=abc-_9; b=2"));
        assert_eq!(session_cookie(&h, "console_session"), Some("abc-_9"));
        h.insert(header::COOKIE, HeaderValue::from_static("console_session=bad\"value"));
        assert_eq!(session_cookie(&h, "console_session"), None);
    }

    #[test]
    fn unsafe_requests_need_csrf_and_same_origin() {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_static("lt.example:8070"));
        assert!(check_unsafe(&Method::GET, &h, "t", false).is_ok());
        assert!(check_unsafe(&Method::POST, &h, "t", false).is_err(), "missing csrf");
        h.insert(CSRF_HEADER, HeaderValue::from_static("t"));
        assert!(check_unsafe(&Method::POST, &h, "t", false).is_ok());
        assert!(check_unsafe(&Method::POST, &h, "u", false).is_err(), "wrong csrf");
        h.insert(header::ORIGIN, HeaderValue::from_static("http://lt.example:8070"));
        assert!(check_unsafe(&Method::DELETE, &h, "t", false).is_ok());
        assert!(check_unsafe(&Method::DELETE, &h, "t", true).is_err(), "scheme must match");
        h.insert(header::ORIGIN, HeaderValue::from_static("http://evil.example"));
        assert!(check_unsafe(&Method::POST, &h, "t", false).is_err(), "foreign origin");
        h.remove(header::ORIGIN);
        h.insert("sec-fetch-site", HeaderValue::from_static("cross-site"));
        assert!(check_unsafe(&Method::POST, &h, "t", false).is_err(), "fetch metadata");
    }
}
