// File: ballista/src/protocols/sql.rs
use sqlx::any::{AnyPoolOptions, AnyRow};
use sqlx::{AnyPool, Column, Row, TypeInfo};

use super::SqlConfig;
use crate::config::{Plan, secret_from_env};
use crate::error::{Error, Result, sanitize};
use crate::vu::Vu;

const MAX_SQL: usize = 64 * 1024;
const MAX_PARAMS: usize = 256;

/// Shared connection pool. The URL comes from an environment variable and is never logged.
pub struct SqlPool {
    pool: AnyPool,
    timeout: std::time::Duration,
    max_rows: usize,
}

impl SqlPool {
    pub fn connect(cfg: &SqlConfig, plan: &Plan) -> Result<Self> {
        let raw = secret_from_env(&cfg.url_env)?;
        let url = url::Url::parse(&raw).map_err(|_| Error::config(format!("{} is not a valid database url", cfg.url_env)))?;
        if !matches!(url.scheme(), "postgres" | "postgresql" | "mysql") {
            return Err(Error::config("sql url must use postgres:// or mysql://"));
        }
        let host = url.host_str().unwrap_or_default();
        if !super::host_allowed(plan, host) {
            return Err(Error::config("database host must be the target host or in target.allowed_hosts"));
        }
        sqlx::any::install_default_drivers();
        let pool = AnyPoolOptions::new()
            .max_connections(cfg.max_connections)
            .acquire_timeout(cfg.timeout)
            .connect_lazy(&raw)
            .map_err(|e| Error::config(format!("sql pool: {}", sanitize(&e.to_string(), 120))))?;
        Ok(Self { pool, timeout: cfg.timeout, max_rows: cfg.max_rows })
    }
}

fn row_to_json(row: &AnyRow) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    for (i, col) in row.columns().iter().enumerate() {
        let ty = col.type_info().name().to_ascii_uppercase();
        let v = if ty.contains("BOOL") {
            row.try_get::<Option<bool>, _>(i).ok().flatten().map(serde_json::Value::Bool)
        } else if ty.contains("INT") {
            row.try_get::<Option<i64>, _>(i).ok().flatten().map(serde_json::Value::from)
        } else if ty.contains("FLOAT") || ty.contains("DOUBLE") || ty.contains("REAL") {
            row.try_get::<Option<f64>, _>(i).ok().flatten().map(serde_json::Value::from)
        } else {
            row.try_get::<Option<String>, _>(i).ok().flatten().map(serde_json::Value::String)
        };
        m.insert(col.name().to_owned(), v.unwrap_or(serde_json::Value::Null));
    }
    m
}

impl Vu {
    /// Executes a parameterized statement (`$1`/`?` placeholders) and returns up to `max_rows` rows.
    /// Parameters are always bound, never interpolated into the SQL text.
    pub async fn sql_query(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        name: &str,
    ) -> Result<Vec<serde_json::Map<String, serde_json::Value>>> {
        if sql.is_empty() || sql.len() > MAX_SQL || params.len() > MAX_PARAMS {
            return Err(Error::check("sql statement must be 1..64 KiB with at most 256 parameters"));
        }
        let db = self.ctx.protocols.sql.as_ref().ok_or_else(|| Error::check("sql is not configured in [protocols.sql]"))?;
        // The statement text is operator-authored plan content; all variable data travels as bound parameters.
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()));
        for p in params {
            q = match p {
                serde_json::Value::Null => q.bind(None::<String>),
                serde_json::Value::Bool(b) => q.bind(*b),
                serde_json::Value::Number(n) if n.is_i64() => q.bind(n.as_i64().unwrap_or_default()),
                serde_json::Value::Number(n) => q.bind(n.as_f64().unwrap_or_default()),
                serde_json::Value::String(s) => q.bind(s.clone()),
                other => q.bind(other.to_string()),
            };
        }
        let max_rows = db.max_rows;
        self.measure("SQL", name, async {
            let rows = tokio::time::timeout(db.timeout, q.fetch_all(&db.pool))
                .await
                .map_err(|_| "timeout".to_owned())?
                .map_err(|e| sanitize(&e.to_string(), 200))?;
            Ok::<_, String>(rows.iter().take(max_rows).map(row_to_json).collect())
        })
        .await
    }
}
