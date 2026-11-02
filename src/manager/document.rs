// File: ballista/src/manager/document.rs
//! Plans submitted as structured JSON documents (the native SDK format) or as plan file text.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::policy::{Policy, input_paths};
use crate::config::Plan;
use crate::error::{Error, Result};

const MAX_DEPTH: usize = 32;
const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;

/// A plan as a JSON object (same structure as the plan file) or as plan file text.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum PlanSource {
    Document(serde_json::Map<String, serde_json::Value>),
    Text(String),
}

fn check_json(v: &serde_json::Value, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(Error::config("plan document is nested too deeply"));
    }
    match v {
        serde_json::Value::Null => Err(Error::config("plan document must not contain null values; omit the field instead")),
        serde_json::Value::Number(n) if n.as_f64().is_none_or(|f| !f.is_finite()) => Err(Error::config("plan numbers must be finite")),
        serde_json::Value::Array(a) => a.iter().try_for_each(|x| check_json(x, depth + 1)),
        serde_json::Value::Object(m) => m.values().try_for_each(|x| check_json(x, depth + 1)),
        _ => Ok(()),
    }
}

/// Files generated from inline content of a structured plan.
pub type InlineFiles = Vec<(String, Vec<u8>)>;

const MAX_INLINE_ROWS: usize = 1_000_000;

fn csv_field(v: &serde_json::Value, what: &str) -> Result<String> {
    let s = match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        _ => return Err(Error::config(format!("{what}: feeder values must be strings, numbers or booleans"))),
    };
    if s.contains([',', '"', '\n', '\r']) { Ok(format!("\"{}\"", s.replace('"', "\"\""))) } else { Ok(s) }
}

/// Renders inline feeder rows (objects, or arrays with `columns`) as RFC 4180 CSV.
fn feeder_csv(name: &str, columns: Option<&serde_json::Value>, rows: &serde_json::Value) -> Result<Vec<u8>> {
    let what = format!("feeder '{name}'");
    let rows = rows.as_array().ok_or_else(|| Error::config(format!("{what}: rows must be an array")))?;
    if rows.is_empty() || rows.len() > MAX_INLINE_ROWS {
        return Err(Error::config(format!("{what}: rows must hold 1..{MAX_INLINE_ROWS} entries")));
    }
    let columns: Vec<String> = match columns {
        Some(c) => c
            .as_array()
            .ok_or_else(|| Error::config(format!("{what}: columns must be an array of names")))?
            .iter()
            .map(|x| x.as_str().map(str::to_owned).ok_or_else(|| Error::config(format!("{what}: columns must be strings"))))
            .collect::<Result<_>>()?,
        None => {
            rows[0].as_object().ok_or_else(|| Error::config(format!("{what}: rows of arrays require columns")))?.keys().cloned().collect()
        }
    };
    if columns.is_empty() || columns.len() > 256 || columns.iter().any(|c| c.is_empty() || c.len() > 128 || c.chars().any(char::is_control))
    {
        return Err(Error::config(format!("{what}: 1..256 printable column names required")));
    }
    let mut out = String::new();
    let header: Vec<String> = columns.iter().map(|c| csv_field(&serde_json::Value::String(c.clone()), &what)).collect::<Result<_>>()?;
    out.push_str(&header.join(","));
    out.push('\n');
    for (i, row) in rows.iter().enumerate() {
        let cells: Vec<String> = match row {
            serde_json::Value::Object(m) => {
                columns.iter().map(|c| m.get(c).map_or(Ok(String::new()), |v| csv_field(v, &what))).collect::<Result<_>>()?
            }
            serde_json::Value::Array(a) if a.len() == columns.len() => a.iter().map(|v| csv_field(v, &what)).collect::<Result<_>>()?,
            _ => return Err(Error::config(format!("{what}: row {i} does not match the columns"))),
        };
        out.push_str(&cells.join(","));
        out.push('\n');
    }
    Ok(out.into_bytes())
}

fn take_str(m: &mut serde_json::Map<String, serde_json::Value>, key: &str, what: &str) -> Result<Option<String>> {
    match m.remove(key) {
        None => Ok(None),
        Some(serde_json::Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(Error::config(format!("{what}.{key} must be a string"))),
    }
}

fn entry_name(m: &serde_json::Map<String, serde_json::Value>, what: &str) -> Result<String> {
    let n = m.get("name").and_then(serde_json::Value::as_str).unwrap_or_default();
    if crate::config::is_valid_name(n) {
        Ok(n.to_owned())
    } else {
        Err(Error::config(format!("{what} needs a simple name to carry inline content")))
    }
}

/// Moves inline content (scenario `source`, feeder `rows`, load `shape_source`) into generated
/// bundle files and points the plan at them.
fn extract_inline(doc: &mut serde_json::Map<String, serde_json::Value>) -> Result<InlineFiles> {
    let mut files = Vec::new();
    if let Some(serde_json::Value::Array(list)) = doc.get_mut("scenario") {
        for sc in list.iter_mut().filter_map(serde_json::Value::as_object_mut) {
            if let Some(src) = take_str(sc, "source", "scenario")? {
                let name = entry_name(sc, "scenario")?;
                if sc.contains_key("script") || sc.contains_key("native") || sc.contains_key("step") {
                    return Err(Error::config(format!("scenario '{name}': source cannot be combined with script, native or step")));
                }
                let path = format!("inline/scenario-{name}.rhai");
                sc.insert("script".into(), serde_json::Value::String(path.clone()));
                files.push((path, src.into_bytes()));
            }
        }
    }
    if let Some(serde_json::Value::Array(list)) = doc.get_mut("feeder") {
        for f in list.iter_mut().filter_map(serde_json::Value::as_object_mut) {
            if let Some(rows) = f.remove("rows") {
                let name = entry_name(f, "feeder")?;
                if f.contains_key("path") {
                    return Err(Error::config(format!("feeder '{name}': rows cannot be combined with path")));
                }
                let columns = f.remove("columns");
                let path = format!("inline/feeder-{name}.csv");
                files.push((path.clone(), feeder_csv(&name, columns.as_ref(), &rows)?));
                f.insert("path".into(), serde_json::Value::String(path));
            } else if f.contains_key("columns") {
                return Err(Error::config("feeder columns require rows"));
            }
        }
    }
    if let Some(load) = doc.get_mut("load").and_then(serde_json::Value::as_object_mut)
        && let Some(src) = take_str(load, "shape_source", "load")?
    {
        if load.contains_key("shape_script") || load.contains_key("stages") {
            return Err(Error::config("load.shape_source cannot be combined with shape_script or stages"));
        }
        let path = "inline/shape.rhai".to_owned();
        load.insert("shape_script".into(), serde_json::Value::String(path.clone()));
        files.push((path, src.into_bytes()));
    }
    Ok(files)
}

impl PlanSource {
    /// Canonical plan file text plus files generated from inline content.
    /// Documents are rendered deterministically (sorted keys).
    pub fn materialize(&self) -> Result<(String, InlineFiles)> {
        match self {
            Self::Text(t) => Ok((t.clone(), Vec::new())),
            Self::Document(m) => {
                let mut doc = m.clone();
                let inline = extract_inline(&mut doc)?;
                let v = serde_json::Value::Object(doc);
                check_json(&v, 0)?;
                let text = toml::to_string(&v).map_err(|e| Error::config(format!("plan document cannot be represented: {e}")))?;
                if text.len() > MAX_DOCUMENT_BYTES {
                    return Err(Error::config("plan exceeds 1 MiB"));
                }
                Ok((text, inline))
            }
        }
    }

    pub fn to_text(&self) -> Result<String> {
        self.materialize().map(|(t, _)| t)
    }
}

/// Result of a dry validation: what the plan contains and which files a bundle must carry.
#[derive(Debug, Clone, Serialize)]
pub struct PlanCheck {
    pub name: String,
    pub scenarios: Vec<String>,
    /// Relative paths the bundle must still include (inline content is already accounted for).
    pub files: Vec<String>,
    /// The plan as a structured document, with defaults applied by the parser left out.
    pub document: serde_json::Value,
}

/// Parses and validates a plan without files: structure, steps, targets, environment and size limits.
pub fn check(source: &PlanSource, overrides: Option<&super::runs::RunOverrides>, policy: &Policy) -> Result<PlanCheck> {
    let (text, inline) = source.materialize()?;
    let base = Path::new("/nonexistent-bundle-root");
    let mut plan = Plan::from_toml(&text, base.to_path_buf())?;
    if let Some(o) = overrides {
        plan.apply_overrides(&o.to_overrides()?)?;
    }
    for s in &plan.scenarios {
        if !s.steps.is_empty() {
            crate::scenario::steps::compile(&s.steps, &s.on_start, &format!("scenario '{}'", s.name))?;
        }
    }
    policy.check_targets(&plan)?;
    policy.check_limits(&plan)?;
    let mut files: Vec<String> = Vec::new();
    for (what, p) in input_paths(&plan) {
        let rel = p
            .strip_prefix(base)
            .map_err(|_| Error::config(format!("{what}: paths must be relative to the bundle")))?
            .to_string_lossy()
            .replace('\\', "/");
        if !super::bundles::valid_path(&rel) {
            return Err(Error::config(format!("{what}: '{rel}' is not a valid bundle path")));
        }
        if !files.contains(&rel) && !inline.iter().any(|(p, _)| *p == rel) {
            files.push(rel);
        }
    }
    let document: toml::Value = toml::from_str(&text).map_err(|e| Error::config(format!("invalid plan: {e}")))?;
    Ok(PlanCheck {
        name: plan.display_name().to_owned(),
        scenarios: plan.scenarios.iter().map(|s| s.name.clone()).collect(),
        files,
        document: serde_json::to_value(document).map_err(|e| Error::Engine(e.to_string()))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::policy::HostRule;

    fn policy() -> Policy {
        Policy {
            targets: vec![HostRule::parse("127.0.0.1").expect("rule")],
            env: Default::default(),
            max_users: 100.0,
            max_run_time: std::time::Duration::from_secs(600),
        }
    }

    fn doc(v: serde_json::Value) -> PlanSource {
        serde_json::from_value(serde_json::json!({ "plan": v })).map(|w: W| w.plan).expect("source")
    }

    fn check_none(src: &PlanSource, p: &Policy) -> Result<PlanCheck> {
        check(src, None, p)
    }

    #[derive(Deserialize)]
    struct W {
        plan: PlanSource,
    }

    #[test]
    fn structured_plan_round_trips_and_lists_files() {
        let src = doc(serde_json::json!({
            "name": "api",
            "target": { "base_url": "http://127.0.0.1:8080/", "headers": { "accept": "application/json" } },
            "load": { "mode": "users", "stages": [{ "duration": "10s", "target": 5 }] },
            "scenario": [
                { "name": "steps", "step": [{ "get": "/items", "capture": { "id": "json.items.0.id" } }, { "think": ["100ms", "200ms"] }] },
                { "name": "scripted", "script": "scenarios/a.rhai" }
            ],
            "feeder": [{ "name": "users", "path": "data/users.csv" }],
            "threshold": [{ "metric": "p95", "max": 500.0 }]
        }));
        let c = check(&src, None, &policy()).expect("check");
        assert_eq!(c.name, "api");
        assert_eq!(c.scenarios, ["steps", "scripted"]);
        assert_eq!(c.files, ["scenarios/a.rhai", "data/users.csv"]);
        assert_eq!(c.document["scenario"][0]["step"][1]["think"][1], "200ms");
        assert_eq!(src.to_text().expect("text"), src.to_text().expect("again"), "rendering is deterministic");
    }

    #[test]
    fn inline_scripts_and_feeder_rows_become_files() {
        let src = doc(serde_json::json!({
            "target": { "base_url": "http://127.0.0.1/" },
            "load": { "stages": [{ "duration": "1s", "target": 1 }] },
            "scenario": [{ "name": "s", "source": "fn run(step, res) { () }" }],
            "feeder": [
                { "name": "users", "rows": [{ "username": "a,b", "password": "p\"q" }, { "username": "c", "password": "d" }] },
                { "name": "ids", "columns": ["id"], "rows": [[1], [2]] }
            ]
        }));
        let (text, files) = src.materialize().expect("materialize");
        assert!(text.contains("inline/scenario-s.rhai") && text.contains("inline/feeder-users.csv"));
        assert_eq!(files[1].1, b"password,username\n\"p\"\"q\",\"a,b\"\nd,c\n".to_vec(), "object rows use sorted column names");
        assert_eq!(files[2].1, b"id\n1\n2\n".to_vec());
        assert!(check(&src, None, &policy()).expect("check").files.is_empty());
    }

    #[test]
    fn rejects_nulls_foreign_hosts_bad_steps_and_escaping_paths() {
        let base = |extra: serde_json::Value| {
            let mut v = serde_json::json!({
                "target": { "base_url": "http://127.0.0.1/" },
                "load": { "stages": [{ "duration": "1s", "target": 1 }] },
                "scenario": [{ "name": "a", "step": [{ "get": "/" }] }]
            });
            if let (Some(m), Some(e)) = (v.as_object_mut(), extra.as_object()) {
                m.extend(e.clone());
            }
            doc(v)
        };
        assert!(check_none(&base(serde_json::json!({ "name": null })), &policy()).is_err());
        assert!(check_none(&base(serde_json::json!({ "target": { "base_url": "http://10.1.1.1/" } })), &policy()).is_err());
        assert!(
            check_none(&base(serde_json::json!({ "scenario": [{ "name": "a", "step": [{ "get": "/", "think": "1s" }] }] })), &policy())
                .is_err()
        );
        assert!(check_none(&base(serde_json::json!({ "feeder": [{ "name": "u", "path": "../secret.csv" }] })), &policy()).is_err());
        assert!(check_none(&base(serde_json::json!({ "feeder": [{ "name": "u", "path": "/etc/passwd" }] })), &policy()).is_err());
    }
}
