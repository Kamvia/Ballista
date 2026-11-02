// File: ballista/src/scenario/steps.rs
//! Declarative scenario steps (`[[scenario.step]]`), compiled into a sandboxed script.
//! Every value from the plan is emitted as an escaped literal; only validated variable
//! names reach the generated code, so plan text cannot inject script statements.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

const MAX_STEPS: usize = 500;
const MAX_BATCH: usize = 100;
const MAX_VAR_SEGMENTS: usize = 8;
const MAX_THINK: Duration = Duration::from_secs(600);
const MAX_DEPTH: usize = 32;

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum StepExpect {
    One(u16),
    Many(Vec<u16>),
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Think {
    Fixed(String),
    Range(Vec<String>),
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// One declarative step: a request, a pause, a feeder read, variable assignments,
/// a transaction boundary or a batch of parallel requests.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub get: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub put: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub query: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub form: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<StepExpect>,
    /// Response body must contain this text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,
    /// Variables to extract: `json.<path>`, `header.<name>`, `status`, `body` or `between:<left>|<right>`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub capture: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries: Option<u32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub continue_on_failure: bool,
    /// Read the response as server-sent events, up to this many.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sse: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconnects: Option<u32>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub context: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub think: Option<Think>,
    /// Reads the next row of this feeder into the variable of the same name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feed: Option<String>,
    /// Assigns variables from templates.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub set: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub txn_begin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub txn_end: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub batch: Vec<Step>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<u32>,
}

fn err(ctx: &str, msg: impl std::fmt::Display) -> Error {
    Error::config(format!("{ctx}: {msg}"))
}

/// Script string literal with every special and control character escaped.
pub(crate) fn lit(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(o, "\\u{:04X}", u32::from(c));
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn is_ident(s: &str) -> bool {
    let mut b = s.bytes();
    s.len() <= 64 && b.next().is_some_and(|c| c.is_ascii_alphabetic() || c == b'_') && b.all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

fn valid_var_path(p: &str) -> bool {
    let segs: Vec<&str> = p.split('.').collect();
    !segs.is_empty()
        && segs.len() <= MAX_VAR_SEGMENTS
        && is_ident(segs[0])
        && segs[1..].iter().all(|s| is_ident(s) || (!s.is_empty() && s.len() <= 9 && s.bytes().all(|b| b.is_ascii_digit())))
}

enum Piece<'a> {
    Text(String),
    Var(&'a str),
}

/// Splits `a ${x.y} b` into literal text and variable references. `$${` escapes a literal `${`.
fn pieces<'a>(s: &'a str, ctx: &str) -> Result<Vec<Piece<'a>>> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("${") {
        if i > 0 && rest.as_bytes()[i - 1] == b'$' {
            text.push_str(&rest[..i - 1]);
            text.push_str("${");
            rest = &rest[i + 2..];
            continue;
        }
        text.push_str(&rest[..i]);
        let tail = &rest[i + 2..];
        let end = tail.find('}').ok_or_else(|| err(ctx, "unterminated ${ in template"))?;
        let var = tail[..end].trim();
        if !(valid_var_path(var) || matches!(var, "uuid()" | "now_ms()")) {
            return Err(err(ctx, format!("invalid template variable '{}'", crate::error::sanitize(var, 64))));
        }
        if !text.is_empty() {
            out.push(Piece::Text(std::mem::take(&mut text)));
        }
        out.push(Piece::Var(var));
        rest = &tail[end + 1..];
    }
    text.push_str(rest);
    if !text.is_empty() || out.is_empty() {
        out.push(Piece::Text(text));
    }
    Ok(out)
}

fn var_expr(v: &str) -> String {
    match v {
        "uuid()" => "uuid()".to_owned(),
        "now_ms()" => "now_ms()".to_owned(),
        path if path.starts_with("env.") => format!("step_env({})", lit(&path[4..])),
        path => format!("step_var(this, {})", lit(path)),
    }
}

/// A template string as an expression. A template made of one variable keeps the value's type.
fn template(s: &str, ctx: &str) -> Result<String> {
    let parts = pieces(s, ctx)?;
    if let [Piece::Var(v)] = parts.as_slice() {
        return Ok(var_expr(v));
    }
    if let [Piece::Text(t)] = parts.as_slice() {
        return Ok(lit(t));
    }
    let items: Vec<String> = parts
        .iter()
        .map(|p| match p {
            Piece::Text(t) => lit(t),
            Piece::Var(v) => var_expr(v),
        })
        .collect();
    Ok(format!("concat([{}])", items.join(", ")))
}

/// Statistics name for a templated path: placeholders become `{var}` so names stay bounded.
fn stat_name(path: &str) -> String {
    let mut o = String::new();
    let mut rest = path.split(['?', '#']).next().unwrap_or("");
    while let Some(i) = rest.find("${") {
        o.push_str(&rest[..i]);
        match rest[i..].find('}') {
            Some(j) => {
                let _ = write!(o, "{{{}}}", rest[i + 2..i + j].trim());
                rest = &rest[i + j + 1..];
            }
            None => {
                rest = "";
            }
        }
    }
    o.push_str(rest);
    o
}

fn value(v: &serde_json::Value, ctx: &str, depth: usize) -> Result<String> {
    if depth > MAX_DEPTH {
        return Err(err(ctx, "json nesting too deep"));
    }
    Ok(match v {
        serde_json::Value::Null => "()".to_owned(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => i.to_string(),
            (None, Some(f)) if f.is_finite() => {
                let s = format!("{f:?}");
                if s.contains(['.', 'e', 'E']) { s } else { format!("{s}.0") }
            }
            _ => return Err(err(ctx, "unsupported number")),
        },
        serde_json::Value::String(s) => template(s, ctx)?,
        serde_json::Value::Array(a) => {
            let items = a.iter().map(|x| value(x, ctx, depth + 1)).collect::<Result<Vec<_>>>()?;
            format!("[{}]", items.join(", "))
        }
        serde_json::Value::Object(m) => {
            let items = m.iter().map(|(k, x)| Ok(format!("{}: {}", lit(k), value(x, ctx, depth + 1)?))).collect::<Result<Vec<_>>>()?;
            format!("#{{{}}}", items.join(", "))
        }
    })
}

fn string_map(m: &BTreeMap<String, String>, ctx: &str) -> Result<String> {
    let items = m.iter().map(|(k, v)| Ok(format!("{}: {}", lit(k), template(v, ctx)?))).collect::<Result<Vec<_>>>()?;
    Ok(format!("#{{{}}}", items.join(", ")))
}

fn duration_ms(s: &str, ctx: &str) -> Result<u64> {
    let d =
        humantime::parse_duration(s.trim()).map_err(|e| err(ctx, format!("invalid duration '{}': {e}", crate::error::sanitize(s, 32))))?;
    if d > MAX_THINK {
        return Err(err(ctx, "duration must be at most 10m"));
    }
    Ok(d.as_millis() as u64)
}

fn capture_expr(src: &str, ctx: &str) -> Result<String> {
    if src == "status" {
        return Ok("res.status".to_owned());
    }
    if src == "body" {
        return Ok("res.body".to_owned());
    }
    if src == "json" {
        return Ok("capture_json(res, \"\")".to_owned());
    }
    if let Some(p) = src.strip_prefix("json.") {
        return Ok(format!("capture_json(res, {})", lit(p)));
    }
    if let Some(h) = src.strip_prefix("header.") {
        if h.is_empty() || !h.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            return Err(err(ctx, "capture header name is invalid"));
        }
        return Ok(format!("capture_header(res, {})", lit(h)));
    }
    if let Some(b) = src.strip_prefix("between:") {
        let (l, r) = b.split_once('|').ok_or_else(|| err(ctx, "between capture must be 'between:<left>|<right>'"))?;
        return Ok(format!("capture_between(res, {}, {})", lit(l), lit(r)));
    }
    Err(err(ctx, format!("unknown capture source '{}'", crate::error::sanitize(src, 64))))
}

fn assign(var: &str, expr: &str) -> String {
    format!("this[{}] = {expr}; ", lit(var))
}

struct Request {
    method: &'static str,
    path: String,
}

fn request_target(s: &Step, ctx: &str) -> Result<Option<Request>> {
    let shortcuts = [("GET", &s.get), ("POST", &s.post), ("PUT", &s.put), ("PATCH", &s.patch), ("DELETE", &s.delete), ("HEAD", &s.head)];
    let set: Vec<(&'static str, &String)> = shortcuts.iter().filter_map(|(m, p)| p.as_ref().map(|p| (*m, p))).collect();
    match (set.as_slice(), &s.method, &s.path) {
        ([], None, None) => Ok(None),
        ([(m, p)], None, None) => Ok(Some(Request { method: m, path: (*p).clone() })),
        ([], Some(m), Some(p)) => {
            let up = m.to_ascii_uppercase();
            let method = match up.as_str() {
                "GET" => "GET",
                "POST" => "POST",
                "PUT" => "PUT",
                "PATCH" => "PATCH",
                "DELETE" => "DELETE",
                "HEAD" => "HEAD",
                "OPTIONS" => "OPTIONS",
                _ => return Err(err(ctx, "unsupported method")),
            };
            Ok(Some(Request { method, path: p.clone() }))
        }
        _ => Err(err(ctx, "a request step sets exactly one of get/post/put/patch/delete/head, or method with path")),
    }
}

fn request_map(s: &Step, r: &Request, ctx: &str) -> Result<String> {
    if r.path.is_empty() || r.path.len() > 2048 {
        return Err(err(ctx, "request path must be 1..2048 characters"));
    }
    let mut f: Vec<String> = vec![format!("\"method\": {}", lit(r.method))];
    let key = if r.path.starts_with("http://") || r.path.starts_with("https://") { "url" } else { "path" };
    f.push(format!("\"{key}\": {}", template(&r.path, ctx)?));
    let name = s.name.clone().unwrap_or_else(|| stat_name(&r.path));
    f.push(format!("\"name\": {}", lit(&crate::error::sanitize(&name, 128))));
    if !s.headers.is_empty() {
        f.push(format!("\"headers\": {}", string_map(&s.headers, ctx)?));
    }
    if !s.query.is_empty() {
        f.push(format!("\"query\": {}", string_map(&s.query, ctx)?));
    }
    let bodies = u8::from(s.json.is_some()) + u8::from(s.body.is_some()) + u8::from(s.form.is_some());
    if bodies > 1 {
        return Err(err(ctx, "set only one of json, body or form"));
    }
    if let Some(j) = &s.json {
        f.push(format!("\"json\": {}", value(j, ctx, 0)?));
    }
    if let Some(b) = &s.body {
        f.push(format!("\"body\": {}", template(b, ctx)?));
    }
    if let Some(m) = &s.form {
        f.push(format!("\"form\": {}", string_map(m, ctx)?));
    }
    match &s.expect {
        Some(StepExpect::One(c)) => f.push(format!("\"expect\": {c}")),
        Some(StepExpect::Many(cs)) if !cs.is_empty() => {
            f.push(format!("\"expect\": [{}]", cs.iter().map(u16::to_string).collect::<Vec<_>>().join(", ")));
        }
        _ => {}
    }
    if let Some(t) = &s.timeout {
        f.push(format!("\"timeout_ms\": {}", duration_ms(t, ctx)?.max(1)));
    }
    if let Some(n) = s.retries {
        f.push(format!("\"retries\": {}", n.min(10)));
    }
    if s.continue_on_failure {
        f.push("\"continue_on_failure\": true".to_owned());
    }
    if let Some(n) = s.sse {
        f.push(format!("\"sse\": {}", n.clamp(1, 100_000)));
    }
    if let Some(n) = s.reconnects {
        f.push(format!("\"reconnects\": {}", n.min(100)));
    }
    if !s.context.is_empty() {
        f.push(format!("\"context\": {}", string_map(&s.context, ctx)?));
    }
    if let Some(c) = &s.contains {
        let msg = lit(&format!("body does not contain {}", crate::error::sanitize(c, 60)));
        f.push(format!("\"check\": |r| if r.body.contains({}) {{ true }} else {{ {msg} }}", lit(c)));
    }
    Ok(format!("#{{{}}}", f.join(", ")))
}

fn check_vars<'a>(names: impl Iterator<Item = &'a String>, ctx: &str) -> Result<()> {
    for n in names {
        if !is_ident(n) {
            return Err(err(ctx, format!("variable name '{}' must be an identifier", crate::error::sanitize(n, 64))));
        }
    }
    Ok(())
}

fn txn_name(n: &str, ctx: &str) -> Result<String> {
    if crate::config::is_valid_name(n) { Ok(lit(n)) } else { Err(err(ctx, "transaction names must be simple names")) }
}

/// Compiles a step list into a `fn <func>(step, res)` body returning one action per step.
fn compile_fn(func: &str, steps: &[Step], scope: &str) -> Result<String> {
    if steps.len() > MAX_STEPS {
        return Err(Error::config(format!("{scope}: at most {MAX_STEPS} steps")));
    }
    let mut arms: Vec<String> = Vec::new();
    let mut pre = String::new();
    for (i, s) in steps.iter().enumerate() {
        let ctx = format!("{scope} step {}", i + 1);
        check_vars(s.capture.keys().chain(s.set.keys()), &ctx)?;
        let req = request_target(s, &ctx)?;
        let actions = u8::from(req.is_some())
            + u8::from(s.think.is_some())
            + u8::from(s.feed.is_some())
            + u8::from(!s.set.is_empty())
            + u8::from(s.txn_begin.is_some())
            + u8::from(s.txn_end.is_some())
            + u8::from(!s.batch.is_empty());
        if actions != 1 {
            return Err(err(&ctx, "each step does exactly one of: request, think, feed, set, txn_begin, txn_end, batch"));
        }
        if req.is_none() && s.batch.is_empty() && !s.capture.is_empty() {
            return Err(err(&ctx, "capture applies to request steps only"));
        }
        let action = if let Some(r) = &req {
            Some(request_map(s, r, &ctx)?)
        } else if let Some(t) = &s.think {
            Some(match t {
                Think::Fixed(d) => format!("sleep({})", duration_ms(d, &ctx)?),
                Think::Range(r) if r.len() == 2 => {
                    let (a, b) = (duration_ms(&r[0], &ctx)?, duration_ms(&r[1], &ctx)?);
                    if a > b {
                        return Err(err(&ctx, "think range min exceeds max"));
                    }
                    format!("sleep(rand_int({a}, {b}))")
                }
                Think::Range(_) => return Err(err(&ctx, "think range must be [min, max]")),
            })
        } else if let Some(feeder) = &s.feed {
            if !is_ident(feeder) {
                return Err(err(&ctx, "feeder names used in steps must be identifiers"));
            }
            let _ = write!(
                pre,
                "{}if this[{}] == () {{ throw {}; }} ",
                assign(feeder, &format!("feed({})", lit(feeder))),
                lit(feeder),
                lit(&format!("feeder '{feeder}' is exhausted or unknown"))
            );
            None
        } else if !s.set.is_empty() {
            for (k, v) in &s.set {
                pre.push_str(&assign(k, &template(v, &ctx)?));
            }
            None
        } else if let Some(n) = &s.txn_begin {
            Some(format!("txn_begin({})", txn_name(n, &ctx)?))
        } else if let Some(n) = &s.txn_end {
            Some(format!("txn_end({})", txn_name(n, &ctx)?))
        } else {
            if s.batch.len() > MAX_BATCH {
                return Err(err(&ctx, format!("batch holds at most {MAX_BATCH} requests")));
            }
            let mut items = Vec::with_capacity(s.batch.len());
            for (j, b) in s.batch.iter().enumerate() {
                let bctx = format!("{ctx} batch[{j}]");
                let r = request_target(b, &bctx)?.ok_or_else(|| err(&bctx, "batch items must be requests"))?;
                if !b.capture.is_empty() || !b.batch.is_empty() || b.sse.is_some() {
                    return Err(err(&bctx, "batch items cannot capture, nest batches or read events"));
                }
                items.push(request_map(b, &r, &bctx)?);
            }
            let name = s.name.as_deref().unwrap_or("batch");
            Some(format!(
                "#{{\"batch\": [{}], \"concurrency\": {}, \"name\": {}}}",
                items.join(", "),
                s.concurrency.unwrap_or(6).clamp(1, 64),
                txn_name(name, &ctx)?
            ))
        };
        if let Some(a) = action {
            let idx = arms.len();
            arms.push(format!("        {idx} => {{ {pre}{a} }}"));
            pre.clear();
            for (var, src) in &s.capture {
                let _ = write!(
                    pre,
                    "{}if this[{}] == () {{ throw {}; }} ",
                    assign(var, &capture_expr(src, &ctx)?),
                    lit(var),
                    lit(&format!("capture '{var}' found nothing"))
                );
            }
        }
    }
    if arms.is_empty() {
        return Err(Error::config(format!("{scope}: steps must include at least one request, think or transaction")));
    }
    let mut o = format!("fn {func}(step, res) {{\n    switch step {{\n");
    for a in &arms {
        o.push_str(a);
        o.push('\n');
    }
    let _ = writeln!(o, "        _ => {{ {pre}() }}\n    }}\n}}");
    Ok(o)
}

/// Compiles iteration steps (and optional start steps) into script source.
pub fn compile(steps: &[Step], on_start: &[Step], scope: &str) -> Result<String> {
    let mut src = String::from("fn tasks() { [\"steps\"] }\n");
    src.push_str(&compile_fn("steps", steps, scope)?);
    if !on_start.is_empty() {
        src.push_str(&compile_fn("on_start", on_start, &format!("{scope} on_start"))?);
    }
    Ok(src)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(toml_src: &str) -> Vec<Step> {
        #[derive(Deserialize)]
        struct W {
            step: Vec<Step>,
        }
        toml::from_str::<W>(toml_src).expect("steps").step
    }

    #[test]
    fn escapes_literals_and_rejects_bad_templates() {
        assert_eq!(lit("a\"b\\c\n"), "\"a\\\"b\\\\c\\n\"");
        assert!(template("x ${a.b} y", "t").expect("ok").contains("step_var(this, \"a.b\")"));
        assert!(template("${a; throw 1}", "t").is_err());
        assert!(template("$${literal}", "t").expect("ok").contains("${literal}"));
        assert_eq!(stat_name("/items/${id}?q=1"), "/items/{id}");
    }

    #[test]
    fn compiles_steps_into_script() {
        let steps = step(
            r#"
            [[step]]
            feed = "users"
            [[step]]
            post = "/login"
            json = { username = "${users.username}", n = 2 }
            capture = { token = "json.token" }
            [[step]]
            think = ["100ms", "300ms"]
            [[step]]
            get = "/items/${token}"
            headers = { authorization = "Bearer ${token}" }
            contains = "ok\"); throw(\"x"
            "#,
        );
        let src = compile(&steps, &[], "s").expect("compile");
        assert!(src.contains("0 => { this[\"users\"] = feed(\"users\");"));
        assert!(src.contains("1 => { this[\"token\"] = capture_json(res, \"token\");"));
        assert!(src.contains("\"name\": \"/items/{token}\""));
        let mut engine = rhai::Engine::new();
        engine.set_max_expr_depths(96, 48);
        engine.compile(&src).expect("generated script parses");
    }

    #[test]
    fn rejects_mixed_actions() {
        let steps = step("[[step]]\nget = \"/a\"\nthink = \"1s\"\n");
        assert!(compile(&steps, &[], "s").is_err());
    }
}
