// File: ballista/src/manager/policy.rs
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::Plan;
use crate::error::{Error, Result, sanitize};

/// A target host pattern: `*` (any), `*.example.com` (subdomains) or an exact host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostRule {
    Any,
    Suffix(String),
    Exact(String),
}

impl HostRule {
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim().to_ascii_lowercase();
        if s == "*" {
            return Ok(Self::Any);
        }
        let valid = |h: &str| {
            !h.is_empty() && h.len() <= 253 && h.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'))
        };
        match s.strip_prefix("*.") {
            Some(rest) if valid(rest) => Ok(Self::Suffix(format!(".{rest}"))),
            None if valid(&s) => Ok(Self::Exact(s)),
            _ => Err(Error::config(format!("invalid target rule '{}'", sanitize(&s, 64)))),
        }
    }

    fn covers(&self, host: &str) -> bool {
        match self {
            Self::Any => true,
            Self::Exact(h) => h == host,
            Self::Suffix(sfx) => {
                if let Some(pattern) = host.strip_prefix('*') {
                    pattern.ends_with(sfx.as_str())
                } else {
                    host.ends_with(sfx.as_str()) && host.len() > sfx.len()
                }
            }
        }
    }
}

/// Operator limits applied to every submitted plan.
#[derive(Debug, Clone)]
pub struct Policy {
    pub targets: Vec<HostRule>,
    pub env: HashSet<String>,
    pub max_users: f64,
    pub max_run_time: Duration,
}

fn host_of(addr: &str) -> Option<String> {
    let h = if addr.contains("://") {
        url::Url::parse(addr).ok()?.host_str()?.to_owned()
    } else if let Some(rest) = addr.strip_prefix('[') {
        rest.split(']').next()?.to_owned()
    } else {
        addr.rsplit_once(':').map_or(addr, |(h, _)| h).to_owned()
    };
    Some(h.trim_matches(['[', ']']).to_ascii_lowercase())
}

impl Policy {
    fn host_ok(&self, host: &str) -> bool {
        self.targets.iter().any(|r| r.covers(host))
    }

    fn check_host(&self, what: &str, addr: &str) -> Result<()> {
        let host = host_of(addr).ok_or_else(|| Error::config(format!("{what}: cannot determine host")))?;
        if self.host_ok(&host) {
            Ok(())
        } else {
            Err(Error::config(format!("{what} host '{}' is not permitted on this server", sanitize(&host, 80))))
        }
    }

    fn check_env(&self, what: &str, name: Option<&str>) -> Result<()> {
        match name {
            Some(n) if !self.env.contains(n) => Err(Error::config(format!(
                "{what} references environment variable '{}' which this server does not expose",
                sanitize(n, 64)
            ))),
            _ => Ok(()),
        }
    }

    /// Verifies files, environment references and targets of a plan rooted at `root`, and its size
    /// limits when `limits` is set (runs are checked with their overrides applied).
    pub fn check(&self, plan: &Plan, root: &Path, limits: bool) -> Result<()> {
        let root = root.canonicalize()?;
        for (what, p) in input_paths(plan) {
            let full = p.canonicalize().map_err(|_| Error::config(format!("{what}: file not found in bundle")))?;
            if !full.starts_with(&root) {
                return Err(Error::config(format!("{what}: files must be inside the bundle")));
            }
        }
        self.check_targets(plan)?;
        if limits { self.check_limits(plan) } else { Ok(()) }
    }

    /// Verifies environment references and target hosts (no file access).
    pub fn check_targets(&self, plan: &Plan) -> Result<()> {
        let t = &plan.target;
        self.check_env("target.proxy_auth_env", t.proxy_auth_env.as_deref())?;
        if let Some(o) = &plan.report.otlp {
            self.check_env("report.otlp.headers_env", o.headers_env.as_deref())?;
            self.check_host("report.otlp", &o.endpoint)?;
        }
        if let Some(i) = &plan.report.influx {
            self.check_env("report.influx.token_env", Some(&i.token_env))?;
            self.check_host("report.influx", i.url.as_str())?;
        }
        let p = &plan.protocols;
        if let Some(m) = &p.mqtt {
            self.check_env("protocols.mqtt.password_env", m.password_env.as_deref())?;
            self.check_host("protocols.mqtt", &m.host)?;
        }
        if let Some(s) = &p.sql {
            self.check_env("protocols.sql.url_env", Some(&s.url_env))?;
        }
        if let Some(g) = &p.grpc {
            self.check_host("protocols.grpc", &g.endpoint)?;
        }
        if let Some(k) = &p.kafka {
            for b in &k.brokers {
                self.check_host("protocols.kafka", b)?;
            }
        }
        self.check_host("target.base_url", t.base_url.as_str())?;
        for h in &t.allowed_hosts {
            self.check_host("target.allowed_hosts", h)?;
        }
        if let Some(proxy) = &t.proxy {
            self.check_host("target.proxy", proxy)?;
        }
        Ok(())
    }

    /// Verifies peak load and duration against the server limits.
    pub fn check_limits(&self, plan: &Plan) -> Result<()> {
        if plan.load.shape_script.is_some() {
            return Ok(());
        }
        let shape = crate::engine::build_shape(plan)?;
        if let Some(peak) = shape.peak()
            && peak > self.max_users
        {
            return Err(Error::config(format!("plan peak {peak} exceeds this server's limit of {}", self.max_users)));
        }
        if let Some(total) = shape.total()
            && total > self.max_run_time
        {
            return Err(Error::config(format!(
                "plan duration {} exceeds this server's limit of {}",
                humantime::format_duration(total),
                humantime::format_duration(self.max_run_time)
            )));
        }
        Ok(())
    }
}

/// Every file a plan reads.
pub(crate) fn input_paths(plan: &Plan) -> Vec<(&'static str, PathBuf)> {
    let mut out: Vec<(&'static str, PathBuf)> = Vec::new();
    for s in &plan.scenarios {
        if let Some(p) = &s.script {
            out.push(("scenario script", p.clone()));
        }
    }
    for f in &plan.feeders {
        out.push(("feeder", f.path.clone()));
    }
    if let Some(p) = &plan.load.shape_script {
        out.push(("shape_script", p.clone()));
    }
    let t = &plan.target;
    for p in [&t.ca_cert, &t.client_cert, &t.client_key].into_iter().flatten() {
        out.push(("target tls file", p.clone()));
    }
    let pr = &plan.protocols;
    if let Some(g) = &pr.grpc {
        out.push(("grpc descriptor_set", g.descriptor_set.clone()));
        for p in [&g.ca_cert, &g.client_cert, &g.client_key].into_iter().flatten() {
            out.push(("grpc tls file", p.clone()));
        }
    }
    if let Some(p) = pr.mqtt.as_ref().and_then(|m| m.ca_cert.as_ref()) {
        out.push(("mqtt ca_cert", p.clone()));
    }
    if let Some(p) = pr.kafka.as_ref().and_then(|k| k.ca_cert.as_ref()) {
        out.push(("kafka ca_cert", p.clone()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_rules() {
        let any = HostRule::parse("*").expect("any");
        let sfx = HostRule::parse("*.example.com").expect("suffix");
        let exact = HostRule::parse("api.test").expect("exact");
        assert!(any.covers("x"));
        assert!(sfx.covers("a.example.com") && !sfx.covers("example.com") && !sfx.covers("evil-example.com"));
        assert!(exact.covers("api.test") && !exact.covers("x.api.test"));
        assert!(HostRule::parse("a b").is_err());
        assert_eq!(host_of("http://[::1]:8080/x").as_deref(), Some("::1"));
        assert_eq!(host_of("broker:9092").as_deref(), Some("broker"));
    }
}
