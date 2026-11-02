// File: ballista/src/web/assets.rs
use axum::extract::Path;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

pub const INDEX_HTML: &str = include_str!("../../assets/index.html");
pub const LOGIN_HTML: &str = include_str!("../../assets/login.html");
const APP_JS: &str = include_str!("../../assets/app.js");
const LOGIN_JS: &str = include_str!("../../assets/login.js");
const APP_CSS: &str = include_str!("../../assets/app.css");

fn respond(body: &'static str, content_type: &'static str, cache: &'static str) -> Response {
    let mut r = body.into_response();
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    r
}

pub fn html(body: &'static str) -> Response {
    respond(body, "text/html; charset=utf-8", "no-store")
}

pub async fn asset(Path(name): Path<String>) -> Response {
    match name.as_str() {
        "app.js" => respond(APP_JS, "text/javascript; charset=utf-8", "no-cache"),
        "login.js" => respond(LOGIN_JS, "text/javascript; charset=utf-8", "no-cache"),
        "app.css" => respond(APP_CSS, "text/css; charset=utf-8", "no-cache"),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}
