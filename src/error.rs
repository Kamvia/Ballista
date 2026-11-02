// File: ballista/src/error.rs
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("configuration error: {0}")]
    Config(String),
    #[error("script error: {0}")]
    Script(String),
    #[error("http error: {0}")]
    Http(String),
    #[error("websocket error: {0}")]
    Ws(String),
    #[error("check failed: {0}")]
    Check(String),
    #[error("assertion failed: {0}")]
    Assertion(String),
    #[error("engine error: {0}")]
    Engine(String),
    #[error("cancelled")]
    Cancelled,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl Error {
    pub fn config(msg: impl Into<String>) -> Self {
        Self::Config(msg.into())
    }

    pub fn check(msg: impl Into<String>) -> Self {
        Self::Check(msg.into())
    }

    pub(crate) fn is_recorded(&self) -> bool {
        matches!(self, Self::Http(_) | Self::Ws(_) | Self::Check(_))
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Removes control characters and bounds length so untrusted text is safe for logs and reports.
pub fn sanitize(input: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(input.len().min(max_chars));
    for (count, ch) in input.chars().enumerate() {
        if count >= max_chars {
            out.push_str("...");
            break;
        }
        if ch.is_control() {
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    out
}
