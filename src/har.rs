// File: ballista/src/har.rs
//! Converts a browser recording (HAR 1.2) into declarative plan steps or a script.
//! Credentials found in the recording are redacted and never written to the output.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::{Error, Result, sanitize};
use crate::scenario::steps::{Step, StepExpect, Think, compile};

const MAX_HAR_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ENTRIES: usize = 10_000;
const MAX_BODY: usize = 1024 * 1024;
const REDACTED: &str = "REDACTED";

#[derive(Deserialize)]
struct Har {
    log: Log,
}

#[derive(Deserialize)]
struct Log {
    #[serde(default)]
    entries: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    #[serde(rename = "startedDateTime", default)]
    started: String,
    #[serde(default)]
    time: f64,
    request: HarRequest,
    #[serde(default)]
    response: Option<HarResponse>,
    #[serde(rename = "_resourceType", default)]
    resource_type: Option<String>,
}

#[derive(Deserialize)]
struct NameValue {
    name: String,
    #[serde(default)]
    value: Option<String>,
}

#[derive(Deserialize)]
struct HarRequest {
    method: String,
    url: String,
    #[serde(default)]
    headers: Vec<NameValue>,
    #[serde(rename = "postData", default)]
    post_data: Option<PostData>,
}

#[derive(Deserialize)]
struct PostData {
    #[serde(rename = "mimeType", default)]
    mime: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    params: Vec<NameValue>,
}

#[derive(Deserialize)]
struct HarResponse {
    #[serde(default)]
    status: i64,
    #[serde(default)]
    content: Option<Content>,
}

#[derive(Deserialize)]
struct Content {
    #[serde(rename = "mimeType", default)]
    mime: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    encoding: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Plan,
    Script,
}

#[derive(Debug, Clone)]
pub struct ImportOptions {
    /// Keep only requests to this host; defaults to the host of the first page request.
    pub host: Option<String>,
    pub include_static: bool,
    /// Gaps between requests at least this long become think steps.
    pub think_min: Duration,
    pub think_max: Duration,
    pub scenario: String,
    pub format: OutputFormat,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            host: None,
            include_static: false,
            think_min: Duration::from_secs(1),
            think_max: Duration::from_secs(30),
            scenario: "recorded".to_owned(),
            format: OutputFormat::Plan,
        }
    }
}

#[derive(Debug, Default)]
pub struct ImportSummary {
    pub requests: usize,
    pub skipped_static: usize,
    pub skipped_foreign: usize,
    pub skipped_other: usize,
    pub redacted: usize,
    /// Secrets replaced by values captured from earlier responses.
    pub correlated: usize,
    pub notes: Vec<String>,
}

const DROP_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "connection",
    "keep-alive",
    "accept-encoding",
    "cookie",
    "te",
    "upgrade",
    "upgrade-insecure-requests",
    "priority",
    "pragma",
    "cache-control",
    "if-none-match",
    "if-modified-since",
    "dnt",
    "referer",
    "user-agent",
    "transfer-encoding",
    "content-type",
];

const SECRET_HEADERS: &[&str] =
    &["authorization", "proxy-authorization", "x-api-key", "x-auth-token", "x-csrf-token", "x-xsrf-token", "x-amz-security-token"];

fn is_secret_key(k: &str) -> bool {
    let k = k.to_ascii_lowercase();
    [
        "password",
        "passwd",
        "secret",
        "token",
        "apikey",
        "api_key",
        "api-key",
        "auth",
        "session",
        "signature",
        "sig",
        "otp",
        "pin",
        "credential",
        "private",
    ]
    .iter()
    .any(|s| k.contains(s))
}

fn is_static(e: &Entry, url: &Url) -> bool {
    if e.resource_type.as_deref().is_some_and(|t| matches!(t, "script" | "stylesheet" | "image" | "font" | "media" | "manifest")) {
        return true;
    }
    let ext = url.path().rsplit_once('.').map(|(_, x)| x.to_ascii_lowercase());
    if ext.as_deref().is_some_and(static_ext) {
        return true;
    }
    let mime = e.response.as_ref().and_then(|r| r.content.as_ref()).map(|c| c.mime.to_ascii_lowercase()).unwrap_or_default();
    mime.starts_with("image/")
        || mime.starts_with("font/")
        || mime.starts_with("video/")
        || mime.starts_with("audio/")
        || mime.contains("css")
        || mime.contains("javascript")
}

fn static_ext(x: &str) -> bool {
    matches!(
        x.to_ascii_lowercase().as_str(),
        "js" | "mjs"
            | "css"
            | "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "avif"
            | "svg"
            | "ico"
            | "woff"
            | "woff2"
            | "ttf"
            | "otf"
            | "eot"
            | "mp4"
            | "webm"
            | "mp3"
            | "wav"
            | "map"
            | "bmp"
    )
}

/// Milliseconds since the epoch for an RFC 3339 timestamp with optional fraction and offset.
fn epoch_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r).and_then(|x| x.parse::<i64>().ok());
    let (y, mo, d, h, mi, se) = (n(0..4)?, n(5..7)?, n(8..10)?, n(11..13)?, n(14..16)?, n(17..19)?);
    let mut rest = &s[19..];
    let mut frac_ms = 0_i64;
    if let Some(f) = rest.strip_prefix('.') {
        let digits = f.bytes().take_while(u8::is_ascii_digit).count();
        let ms: String = f[..digits].chars().chain("000".chars()).take(3).collect();
        frac_ms = ms.parse().ok()?;
        rest = &f[digits..];
    }
    let offset_min = match rest {
        "" | "Z" | "z" => 0,
        o if o.len() == 6 && (o.starts_with('+') || o.starts_with('-')) => {
            let sign = if o.starts_with('-') { -1 } else { 1 };
            sign * (o[1..3].parse::<i64>().ok()? * 60 + o[4..6].parse::<i64>().ok()?)
        }
        _ => return None,
    };
    let (yy, mm) = if mo <= 2 { (y - 1, mo + 9) } else { (y, mo - 3) };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * mm + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 86_400 + h * 3600 + mi * 60 + se - offset_min * 60) * 1000) + frac_ms)
}

/// Replaces ids in path segments so statistics group by endpoint, e.g. `/items/42` -> `/items/{id}`.
fn stat_name(path: &str) -> String {
    let segs: Vec<String> = path
        .split('/')
        .map(|s| {
            let numeric = !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
            let hexish = s.len() >= 16 && s.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-');
            if numeric || hexish { "{id}".to_owned() } else { s.to_owned() }
        })
        .collect();
    sanitize(&segs.join("/"), 128)
}

fn redact_json(v: &mut serde_json::Value, count: &mut usize, dropped_nulls: &mut bool) {
    match v {
        serde_json::Value::Object(m) => {
            m.retain(|_, x| {
                let keep = !x.is_null();
                *dropped_nulls |= !keep;
                keep
            });
            for (k, x) in m.iter_mut() {
                if is_secret_key(k) && !x.is_object() && !x.is_array() {
                    *x = serde_json::Value::String(REDACTED.to_owned());
                    *count += 1;
                } else {
                    redact_json(x, count, dropped_nulls);
                }
            }
        }
        serde_json::Value::Array(a) => {
            for x in a.iter_mut() {
                if x.is_null() {
                    *x = serde_json::Value::String(String::new());
                    *dropped_nulls = true;
                }
                redact_json(x, count, dropped_nulls);
            }
        }
        _ => {}
    }
}

/// Template text from a recording must not be interpreted as a variable reference.
fn escape_template(s: &str) -> String {
    s.replace("${", "$${")
}

fn escape_json(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::String(s) => *s = escape_template(s),
        serde_json::Value::Array(a) => a.iter_mut().for_each(escape_json),
        serde_json::Value::Object(m) => m.values_mut().for_each(escape_json),
        _ => {}
    }
}

fn origin(u: &Url) -> String {
    let port = u.port().map(|p| format!(":{p}")).unwrap_or_default();
    format!("{}://{}{port}/", u.scheme(), u.host_str().unwrap_or(""))
}

fn entry_step(e: &Entry, url: &Url, sum: &mut ImportSummary, null_note: &mut bool) -> Option<Step> {
    let mut s = Step::default();
    let mut path = url.path().to_owned();
    let mut seen = std::collections::HashSet::new();
    let pairs: Vec<(String, String)> = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    let duplicate = pairs.iter().any(|(k, _)| !seen.insert(k.clone()));
    if duplicate {
        let mut q = url::form_urlencoded::Serializer::new(String::new());
        for (k, v) in &pairs {
            let v = if is_secret_key(k) {
                sum.redacted += 1;
                REDACTED
            } else {
                v.as_str()
            };
            q.append_pair(k, v);
        }
        path = format!("{path}?{}", q.finish());
    } else {
        for (k, v) in pairs {
            let v = if is_secret_key(&k) {
                sum.redacted += 1;
                REDACTED.to_owned()
            } else {
                escape_template(&v)
            };
            s.query.insert(escape_template(&k), v);
        }
    }
    let path = escape_template(&path);
    match e.request.method.to_ascii_uppercase().as_str() {
        "GET" => s.get = Some(path.clone()),
        "POST" => s.post = Some(path.clone()),
        "PUT" => s.put = Some(path.clone()),
        "PATCH" => s.patch = Some(path.clone()),
        "DELETE" => s.delete = Some(path.clone()),
        "HEAD" => s.head = Some(path.clone()),
        "OPTIONS" => {
            s.method = Some("OPTIONS".to_owned());
            s.path = Some(path.clone());
        }
        _ => return None,
    }
    s.name = Some(stat_name(url.path()));
    for h in &e.request.headers {
        let name = h.name.to_ascii_lowercase();
        if name.starts_with(':') || name.starts_with("sec-") || DROP_HEADERS.contains(&name.as_str()) {
            continue;
        }
        if http::HeaderName::from_bytes(name.as_bytes()).is_err() {
            continue;
        }
        let value = h.value.clone().unwrap_or_default();
        if SECRET_HEADERS.contains(&name.as_str()) || is_secret_key(&name) {
            s.headers.insert(name, REDACTED.to_owned());
            sum.redacted += 1;
        } else if http::HeaderValue::from_str(&value).is_ok() {
            s.headers.insert(name, escape_template(&value));
        }
    }
    if let Some(pd) = &e.request.post_data {
        let mime = pd.mime.to_ascii_lowercase();
        let text = pd.text.as_deref().unwrap_or("");
        if mime.contains("multipart/") {
            sum.notes.push(format!("multipart body of {} {} was not imported", e.request.method, stat_name(url.path())));
        } else if text.len() > MAX_BODY {
            sum.notes.push(format!("body over 1 MiB of {} {} was not imported", e.request.method, stat_name(url.path())));
        } else if mime.contains("json")
            && let Ok(mut v) = serde_json::from_str::<serde_json::Value>(text)
        {
            redact_json(&mut v, &mut sum.redacted, null_note);
            escape_json(&mut v);
            s.json = Some(v);
        } else if mime.contains("x-www-form-urlencoded") {
            let pairs: Vec<(String, String)> = if pd.params.is_empty() {
                url::form_urlencoded::parse(text.as_bytes()).map(|(k, v)| (k.into_owned(), v.into_owned())).collect()
            } else {
                pd.params.iter().map(|p| (p.name.clone(), p.value.clone().unwrap_or_default())).collect()
            };
            let mut form = BTreeMap::new();
            for (k, v) in pairs {
                let v = if is_secret_key(&k) {
                    sum.redacted += 1;
                    REDACTED.to_owned()
                } else {
                    escape_template(&v)
                };
                form.insert(escape_template(&k), v);
            }
            s.form = Some(form);
        } else if !text.is_empty() {
            s.body = Some(escape_template(text));
            if !pd.mime.is_empty() && http::HeaderValue::from_str(&pd.mime).is_ok() {
                s.headers.insert("content-type".to_owned(), escape_template(&pd.mime));
            }
        }
    }
    if let Some(r) = &e.response
        && (400..=599).contains(&r.status)
    {
        s.expect = Some(StepExpect::One(r.status as u16));
    }
    Some(s)
}

const MIN_CORRELATE_LEN: usize = 8;
const MAX_RESPONSE_JSON: usize = 2 * 1024 * 1024;

fn response_json(e: &Entry) -> Option<serde_json::Value> {
    let c = e.response.as_ref()?.content.as_ref()?;
    if !c.mime.to_ascii_lowercase().contains("json") || c.encoding.as_deref().is_some_and(|x| x != "utf-8") {
        return None;
    }
    let text = c.text.as_deref()?;
    if text.len() > MAX_RESPONSE_JSON {
        return None;
    }
    serde_json::from_str(text).ok()
}

/// Dotted path of the first string leaf equal to `needle`.
fn find_path(v: &serde_json::Value, needle: &str, depth: usize) -> Option<String> {
    if depth > 16 {
        return None;
    }
    match v {
        serde_json::Value::String(s) if s == needle => Some(String::new()),
        serde_json::Value::Object(m) => m.iter().find_map(|(k, x)| {
            let valid = !k.is_empty() && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') && k.len() <= 64;
            if !valid {
                return None;
            }
            find_path(x, needle, depth + 1).map(|p| if p.is_empty() { k.clone() } else { format!("{k}.{p}") })
        }),
        serde_json::Value::Array(a) => a
            .iter()
            .enumerate()
            .take(1000)
            .find_map(|(i, x)| find_path(x, needle, depth + 1).map(|p| if p.is_empty() { i.to_string() } else { format!("{i}.{p}") })),
        _ => None,
    }
}

fn var_name(path: &str, taken: &mut std::collections::HashSet<String>) -> String {
    let last = path.rsplit('.').find(|s| s.bytes().next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')).unwrap_or("value");
    let base: String = last.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').take(40).collect();
    let base = if base.is_empty() { "value".to_owned() } else { base };
    let mut name = base.clone();
    let mut n = 2;
    while !taken.insert(name.clone()) {
        name = format!("{base}_{n}");
        n += 1;
    }
    name
}

/// Replaces redacted secrets that an earlier response returned with a capture of that value.
fn correlate(
    e: &Entry,
    step: &mut Step,
    steps: &mut [Step],
    responses: &[(usize, serde_json::Value)],
    vars: &mut std::collections::HashSet<String>,
    sum: &mut ImportSummary,
) {
    let mut link = |raw: &str| -> Option<String> {
        let (prefix, secret) = match raw.split_once(' ') {
            Some((scheme, rest)) if scheme.len() <= 16 && scheme.bytes().all(|b| b.is_ascii_alphabetic()) => (format!("{scheme} "), rest),
            _ => (String::new(), raw),
        };
        if secret.len() < MIN_CORRELATE_LEN {
            return None;
        }
        let (idx, path) = responses.iter().rev().find_map(|(i, json)| find_path(json, secret, 0).map(|p| (*i, p)))?;
        let source = format!("json.{path}");
        let existing = steps[idx].capture.iter().find(|(_, v)| **v == source).map(|(k, _)| k.clone());
        let var = existing.unwrap_or_else(|| {
            let v = var_name(&path, vars);
            steps[idx].capture.insert(v.clone(), source);
            v
        });
        Some(format!("{}${{{var}}}", escape_template(&prefix)))
    };
    for h in &e.request.headers {
        let name = h.name.to_ascii_lowercase();
        if step.headers.get(&name).is_some_and(|v| v == REDACTED)
            && let Some(t) = h.value.as_deref().and_then(&mut link)
        {
            step.headers.insert(name, t);
            sum.redacted -= 1;
            sum.correlated += 1;
        }
    }
    if let Ok(u) = Url::parse(&e.request.url) {
        for (k, v) in u.query_pairs() {
            let key = escape_template(&k);
            if step.query.get(&key).is_some_and(|x| x == REDACTED)
                && let Some(t) = link(&v)
            {
                step.query.insert(key, t);
                sum.redacted -= 1;
                sum.correlated += 1;
            }
        }
    }
}

fn think_step(gap_ms: i64, opts: &ImportOptions) -> Option<Step> {
    let gap = Duration::from_millis(u64::try_from(gap_ms).ok()?);
    if gap < opts.think_min {
        return None;
    }
    let ms = gap.min(opts.think_max).as_millis();
    Some(Step { think: Some(Think::Fixed(format!("{ms}ms"))), ..Step::default() })
}

/// Parses a HAR document and returns the steps and the origin they target.
pub fn convert(raw: &[u8], opts: &ImportOptions) -> Result<(Vec<Step>, Url, ImportSummary)> {
    let har: Har =
        serde_json::from_slice(raw).map_err(|e| Error::config(format!("invalid HAR file: {}", sanitize(&e.to_string(), 200))))?;
    if har.log.entries.len() > MAX_ENTRIES {
        return Err(Error::config(format!("HAR holds more than {MAX_ENTRIES} entries")));
    }
    let mut entries: Vec<(&Entry, Url, Option<i64>)> = har
        .log
        .entries
        .iter()
        .filter_map(|e| {
            let u = Url::parse(&e.request.url).ok()?;
            matches!(u.scheme(), "http" | "https").then(|| (e, u, epoch_ms(&e.started)))
        })
        .collect();
    entries.sort_by_key(|(_, _, t)| t.unwrap_or(i64::MAX));
    let mut sum = ImportSummary::default();
    let host = match &opts.host {
        Some(h) => h.to_ascii_lowercase(),
        None => entries
            .iter()
            .find(|(e, u, _)| !is_static(e, u))
            .and_then(|(_, u, _)| u.host_str())
            .ok_or_else(|| Error::config("HAR contains no http(s) page requests"))?
            .to_ascii_lowercase(),
    };
    let base = entries
        .iter()
        .find(|(_, u, _)| u.host_str().is_some_and(|h| h.eq_ignore_ascii_case(&host)))
        .map(|(_, u, _)| origin(u))
        .ok_or_else(|| Error::config(format!("no requests to host '{}' in HAR", sanitize(&host, 80))))?;
    let base = Url::parse(&base).map_err(|e| Error::config(format!("origin: {e}")))?;
    let mut steps = Vec::new();
    let mut last_end: Option<i64> = None;
    let mut null_note = false;
    let mut responses: Vec<(usize, serde_json::Value)> = Vec::new();
    let mut vars: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (e, u, t) in &entries {
        if !u.host_str().is_some_and(|h| h.eq_ignore_ascii_case(&host)) || u.port_or_known_default() != base.port_or_known_default() {
            sum.skipped_foreign += 1;
            continue;
        }
        if !opts.include_static && is_static(e, u) {
            sum.skipped_static += 1;
            continue;
        }
        if e.response.as_ref().is_some_and(|r| r.status == 0) {
            sum.skipped_other += 1;
            continue;
        }
        if let (Some(end), Some(start)) = (last_end, t)
            && let Some(th) = think_step(start - end, opts)
        {
            steps.push(th);
        }
        match entry_step(e, u, &mut sum, &mut null_note) {
            Some(mut s) => {
                correlate(e, &mut s, &mut steps, &responses, &mut vars, &mut sum);
                if let Some(json) = response_json(e) {
                    responses.push((steps.len(), json));
                }
                steps.push(s);
                sum.requests += 1;
            }
            None => {
                sum.skipped_other += 1;
                continue;
            }
        }
        if let Some(start) = t {
            let end = start + e.time.max(0.0) as i64;
            last_end = Some(last_end.map_or(end, |l| l.max(end)));
        }
    }
    if null_note {
        sum.notes.push("json null values were dropped (TOML has no null)".to_owned());
    }
    if sum.requests == 0 {
        return Err(Error::config("no requests left to import after filtering"));
    }
    if steps.len() > 500 {
        return Err(Error::config("recording yields more than 500 steps; narrow it with --host or split the HAR"));
    }
    Ok((steps, base, sum))
}

#[derive(Serialize)]
struct ScenarioOut<'a> {
    name: &'a str,
    step: &'a [Step],
}

#[derive(Serialize)]
struct PlanTail<'a> {
    scenario: Vec<ScenarioOut<'a>>,
}

fn toml_str(s: &str) -> String {
    toml::Value::String(s.to_owned()).to_string()
}

/// Renders the import as a runnable plan (TOML with steps) or a script.
pub fn render(steps: &[Step], base: &Url, sum: &ImportSummary, opts: &ImportOptions, source: &str) -> Result<String> {
    let comment = if opts.format == OutputFormat::Plan { "#" } else { "//" };
    let mut head = format!(
        "{comment} Imported from {}: {} requests, {} static and {} foreign skipped, {} values redacted, {} correlated.\n",
        sanitize(source, 120),
        sum.requests,
        sum.skipped_static,
        sum.skipped_foreign,
        sum.redacted,
        sum.correlated
    );
    if sum.redacted > 0 {
        head.push_str(&format!("{comment} Replace REDACTED values with captures, feeders or ${{env.NAME}} templates.\n"));
    }
    for n in &sum.notes {
        head.push_str(&format!("{comment} Note: {}\n", sanitize(n, 200)));
    }
    match opts.format {
        OutputFormat::Script => {
            let src = compile(steps, &[], &format!("scenario '{}'", opts.scenario))?;
            Ok(format!("{head}\n{src}"))
        }
        OutputFormat::Plan => {
            let tail = toml::to_string(&PlanTail { scenario: vec![ScenarioOut { name: &opts.scenario, step: steps }] })
                .map_err(|e| Error::config(format!("render plan: {e}")))?;
            Ok(format!(
                "{head}name = {}\n\n[target]\nbase_url = {}\n\n[load]\nmode = \"users\"\nstages = [{{ duration = \"1m\", target = 1 }}]\n\n{tail}",
                toml_str(&opts.scenario),
                toml_str(base.as_str()),
            ))
        }
    }
}

/// Reads a HAR file, converts it and returns the rendered output.
pub fn import_file(path: &Path, opts: &ImportOptions) -> Result<(String, ImportSummary)> {
    let meta = std::fs::metadata(path).map_err(|e| Error::config(format!("{}: {e}", path.display())))?;
    if !meta.is_file() || meta.len() > MAX_HAR_BYTES {
        return Err(Error::config("HAR input must be a regular file up to 256 MiB"));
    }
    let raw = std::fs::read(path)?;
    let (steps, base, sum) = convert(&raw, opts)?;
    let source = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let out = render(&steps, &base, &sum, opts, &source)?;
    Ok((out, sum))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HAR: &str = r#"{"log":{"entries":[
      {"startedDateTime":"2026-01-01T10:00:00.000Z","time":120,"request":{"method":"GET","url":"https://shop.test/items?q=a&token=abc","headers":[{"name":":authority","value":"shop.test"},{"name":"Accept","value":"application/json"},{"name":"Cookie","value":"sid=1"}]},"response":{"status":200,"content":{"mimeType":"application/json","text":"{\"session\":{\"jwt\":\"eyJhbGciOi.abc\"}}"}}},
      {"startedDateTime":"2026-01-01T10:00:00.050Z","time":10,"request":{"method":"GET","url":"https://shop.test/app.js","headers":[]},"response":{"status":200,"content":{"mimeType":"text/javascript"}}},
      {"startedDateTime":"2026-01-01T10:00:00.060Z","time":10,"request":{"method":"GET","url":"https://cdn.other/x","headers":[]},"response":{"status":200}},
      {"startedDateTime":"2026-01-01T12:00:03.500+02:00","time":80,"request":{"method":"POST","url":"https://shop.test/login","headers":[{"name":"Authorization","value":"Bearer eyJhbGciOi.abc"},{"name":"X-Api-Key","value":"short"}],"postData":{"mimeType":"application/json","text":"{\"username\":\"u${x}\",\"password\":\"p\",\"note\":null}"}},"response":{"status":401}}
    ]}}"#;

    #[test]
    fn converts_filters_and_redacts() {
        let (steps, base, sum) = convert(HAR.as_bytes(), &ImportOptions::default()).expect("convert");
        assert_eq!(base.as_str(), "https://shop.test/");
        assert_eq!((sum.requests, sum.skipped_static, sum.skipped_foreign), (2, 1, 1));
        assert_eq!(steps.len(), 3, "request, think, request");
        assert_eq!(steps[0].query.get("token").map(String::as_str), Some(REDACTED));
        assert!(!steps[0].headers.contains_key("cookie"));
        assert_eq!(steps[1].think, Some(Think::Fixed("3380ms".into())));
        let login = &steps[2];
        assert_eq!(login.headers.get("authorization").map(String::as_str), Some("Bearer ${jwt}"));
        assert_eq!(login.headers.get("x-api-key").map(String::as_str), Some(REDACTED));
        assert_eq!(steps[0].capture.get("jwt").map(String::as_str), Some("json.session.jwt"));
        assert_eq!(sum.correlated, 1);
        let json = login.json.as_ref().expect("json");
        assert_eq!(json["password"], REDACTED);
        assert_eq!(json["username"], "u$${x}");
        assert_eq!(login.expect, Some(StepExpect::One(401)));
        let plan = render(&steps, &base, &sum, &ImportOptions::default(), "x.har").expect("render");
        assert!(!plan.contains("eyJhbGciOi") && !plan.contains("sid=1") && !plan.contains("short"));
        let parsed: toml::Value = toml::from_str(&plan).expect("plan is valid toml");
        assert!(parsed.get("scenario").is_some());
        let script = render(&steps, &base, &sum, &ImportOptions { format: OutputFormat::Script, ..ImportOptions::default() }, "x.har")
            .expect("script");
        assert!(script.contains("fn steps(step, res)"));
    }

    #[test]
    fn parses_timestamps_and_names() {
        assert_eq!(epoch_ms("1970-01-01T00:00:01.5Z"), Some(1500));
        assert_eq!(epoch_ms("1970-01-01T01:00:00+01:00"), Some(0));
        assert_eq!(stat_name("/items/42/a1b2c3d4e5f60718"), "/items/{id}/{id}");
    }
}
