// File: ballista/src/protocols/mod.rs
#[cfg(feature = "grpc")]
pub mod grpc;
#[cfg(feature = "kafka")]
pub mod kafka;
#[cfg(feature = "mqtt")]
pub mod mqtt;
pub mod net;
pub mod script;
#[cfg(feature = "sql")]
pub mod sql;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use serde::{Deserialize, Deserializer};

use crate::config::{Plan, is_valid_env_name};
use crate::error::{Error, Result};

fn de_duration<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Duration, D::Error> {
    let raw = String::deserialize(d)?;
    humantime::parse_duration(raw.trim()).map_err(serde::de::Error::custom)
}

fn default_timeout() -> Duration {
    Duration::from_secs(10)
}

/// Optional non-HTTP protocol settings. Each section requires the matching build feature.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolsConfig {
    #[serde(default)]
    pub grpc: Option<GrpcConfig>,
    #[serde(default)]
    pub mqtt: Option<MqttConfig>,
    #[serde(default)]
    pub kafka: Option<KafkaConfig>,
    #[serde(default)]
    pub sql: Option<SqlConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrpcConfig {
    /// http://host:port or https://host:port
    pub endpoint: String,
    /// FileDescriptorSet produced by `protoc --include_imports --descriptor_set_out=api.pb`.
    pub descriptor_set: PathBuf,
    #[serde(default = "default_timeout", deserialize_with = "de_duration")]
    pub timeout: Duration,
    #[serde(default)]
    pub ca_cert: Option<PathBuf>,
    #[serde(default)]
    pub client_cert: Option<PathBuf>,
    #[serde(default)]
    pub client_key: Option<PathBuf>,
    /// TLS server name override.
    #[serde(default)]
    pub domain: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MqttConfig {
    pub host: String,
    #[serde(default = "default_mqtt_port")]
    pub port: u16,
    #[serde(default)]
    pub tls: bool,
    #[serde(default)]
    pub ca_cert: Option<PathBuf>,
    #[serde(default)]
    pub username: Option<String>,
    /// Environment variable holding the MQTT password.
    #[serde(default)]
    pub password_env: Option<String>,
    #[serde(default = "default_keep_alive", deserialize_with = "de_duration")]
    pub keep_alive: Duration,
    #[serde(default = "default_timeout", deserialize_with = "de_duration")]
    pub timeout: Duration,
    #[serde(default = "default_client_prefix")]
    pub client_id_prefix: String,
}

fn default_mqtt_port() -> u16 {
    1883
}
fn default_keep_alive() -> Duration {
    Duration::from_secs(30)
}
fn default_client_prefix() -> String {
    "ballista".to_owned()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KafkaConfig {
    pub brokers: Vec<String>,
    #[serde(default)]
    pub tls: bool,
    #[serde(default)]
    pub ca_cert: Option<PathBuf>,
    #[serde(default = "default_timeout", deserialize_with = "de_duration")]
    pub timeout: Duration,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqlConfig {
    /// Environment variable holding the connection URL (postgres:// or mysql://). Never put it in the plan.
    pub url_env: String,
    #[serde(default = "default_sql_conns")]
    pub max_connections: u32,
    #[serde(default = "default_timeout", deserialize_with = "de_duration")]
    pub timeout: Duration,
    #[serde(default = "default_max_rows")]
    pub max_rows: usize,
}

fn default_sql_conns() -> u32 {
    20
}
fn default_max_rows() -> usize {
    1000
}

fn host_of(addr: &str) -> Option<&str> {
    let trimmed = addr.trim();
    if let Some(rest) = trimmed.strip_prefix('[') {
        return rest.split_once(']').map(|(h, _)| h);
    }
    Some(trimmed.rsplit_once(':').map_or(trimmed, |(h, _)| h))
}

/// Checks that a raw host or host:port belongs to the target host or `allowed_hosts`.
pub fn host_allowed(plan: &Plan, addr: &str) -> bool {
    let Some(host) = host_of(addr) else {
        return false;
    };
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    let base = plan.target.base_url.host_str().map(|h| h.trim_matches(['[', ']']));
    base.is_some_and(|b| b.eq_ignore_ascii_case(host))
        || plan.target.allowed_hosts.iter().any(|h| h.trim_matches(['[', ']']).eq_ignore_ascii_case(host))
}

impl ProtocolsConfig {
    pub fn resolve_paths(&mut self, base: &Path) {
        let fix = |p: &mut PathBuf| {
            if p.is_relative() {
                *p = base.join(&*p);
            }
        };
        if let Some(g) = self.grpc.as_mut() {
            fix(&mut g.descriptor_set);
            for p in [g.ca_cert.as_mut(), g.client_cert.as_mut(), g.client_key.as_mut()].into_iter().flatten() {
                fix(p);
            }
        }
        if let Some(p) = self.mqtt.as_mut().and_then(|m| m.ca_cert.as_mut()) {
            fix(p);
        }
        if let Some(p) = self.kafka.as_mut().and_then(|k| k.ca_cert.as_mut()) {
            fix(p);
        }
    }

    /// Validates settings and that every configured protocol was compiled in.
    pub fn validate(&self, plan: &Plan) -> Result<()> {
        let missing = |name: &str| Error::config(format!("plan uses {name} but this binary was built without the '{name}' feature"));
        if let Some(g) = &self.grpc {
            if !cfg!(feature = "grpc") {
                return Err(missing("grpc"));
            }
            let u = url::Url::parse(&g.endpoint).map_err(|e| Error::config(format!("grpc endpoint: {e}")))?;
            if !matches!(u.scheme(), "http" | "https") || !plan.host_allowed(&u) {
                return Err(Error::config("grpc endpoint must be http(s) and its host must be allowed by the target"));
            }
            if g.client_cert.is_some() != g.client_key.is_some() {
                return Err(Error::config("grpc client_cert and client_key must be set together"));
            }
        }
        if let Some(m) = &self.mqtt {
            if !cfg!(feature = "mqtt") {
                return Err(missing("mqtt"));
            }
            if !host_allowed(plan, &m.host) {
                return Err(Error::config("mqtt host must be the target host or in target.allowed_hosts"));
            }
            if let Some(n) = &m.password_env
                && !is_valid_env_name(n)
            {
                return Err(Error::config("mqtt password_env must be an environment variable name"));
            }
            if m.client_id_prefix.is_empty() || m.client_id_prefix.len() > 32 || !crate::config::is_valid_name(&m.client_id_prefix) {
                return Err(Error::config("mqtt client_id_prefix must be 1..32 name characters"));
            }
        }
        if let Some(k) = &self.kafka {
            if !cfg!(feature = "kafka") {
                return Err(missing("kafka"));
            }
            if k.brokers.is_empty() || k.brokers.len() > 64 || k.brokers.iter().any(|b| !host_allowed(plan, b)) {
                return Err(Error::config("kafka brokers must be 1..64 hosts allowed by the target"));
            }
        }
        if let Some(s) = &self.sql {
            if !cfg!(feature = "sql") {
                return Err(missing("sql"));
            }
            if !is_valid_env_name(&s.url_env) {
                return Err(Error::config("sql url_env must be an environment variable name"));
            }
            if s.max_connections == 0 || s.max_connections > 1000 || s.max_rows == 0 || s.max_rows > 100_000 {
                return Err(Error::config("sql max_connections must be 1..1000 and max_rows 1..100000"));
            }
        }
        Ok(())
    }
}

/// Builds a rustls client configuration with an optional private CA and optional client identity.
pub fn tls_client_config(ca: Option<&Path>, identity: Option<(&Path, &Path)>) -> Result<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    match ca {
        Some(path) => {
            for c in CertificateDer::pem_file_iter(path).map_err(|e| Error::config(format!("cannot read ca {}: {e}", path.display())))? {
                let c = c.map_err(|e| Error::config(format!("invalid ca certificate: {e}")))?;
                roots.add(c).map_err(|e| Error::config(format!("invalid ca certificate: {e}")))?;
            }
        }
        None => roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()),
    }
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::config(format!("tls configuration: {e}")))?
        .with_root_certificates(roots);
    match identity {
        Some((cert, key)) => {
            let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert)
                .map_err(|e| Error::config(format!("cannot read client certificate: {e}")))?
                .collect::<std::result::Result<_, _>>()
                .map_err(|e| Error::config(format!("invalid client certificate: {e}")))?;
            let key = PrivateKeyDer::from_pem_file(key).map_err(|e| Error::config(format!("invalid client key: {e}")))?;
            builder.with_client_auth_cert(chain, key).map_err(|e| Error::config(format!("client identity: {e}")))
        }
        None => Ok(builder.with_no_client_auth()),
    }
}

/// Shared protocol clients for one run.
#[derive(Default)]
pub struct Protocols {
    #[cfg(feature = "grpc")]
    pub grpc: Option<grpc::GrpcClient>,
    #[cfg(feature = "kafka")]
    pub kafka: Option<kafka::KafkaClient>,
    #[cfg(feature = "sql")]
    pub sql: Option<sql::SqlPool>,
    #[cfg(feature = "mqtt")]
    pub mqtt: Option<MqttConfig>,
}

impl Protocols {
    /// Creates shared clients. Connections are lazy where the client library allows it.
    pub async fn connect(plan: &Plan) -> Result<Self> {
        #[allow(unused_mut)]
        let mut p = Self::default();
        let cfg = &plan.protocols;
        #[cfg(feature = "grpc")]
        if let Some(g) = &cfg.grpc {
            p.grpc = Some(grpc::GrpcClient::connect(g).await?);
        }
        #[cfg(feature = "kafka")]
        if let Some(k) = &cfg.kafka {
            p.kafka = Some(kafka::KafkaClient::connect(k).await?);
        }
        #[cfg(feature = "sql")]
        if let Some(s) = &cfg.sql {
            p.sql = Some(sql::SqlPool::connect(s, plan)?);
        }
        #[cfg(feature = "mqtt")]
        {
            p.mqtt.clone_from(&cfg.mqtt);
        }
        let _ = cfg;
        Ok(p)
    }
}

#[cfg(test)]
mod tests {
    use super::host_of;

    #[test]
    fn parses_hosts() {
        assert_eq!(host_of("broker:9092"), Some("broker"));
        assert_eq!(host_of("[::1]:9092"), Some("::1"));
        assert_eq!(host_of("plainhost"), Some("plainhost"));
    }
}
