// File: ballista/src/shape.rs
use std::path::Path;
use std::time::Duration;

use rhai::{AST, Dynamic, Engine, Scope};

use crate::config::Stage;
use crate::error::{Error, Result, sanitize};

const MAX_SHAPE_SCRIPT: u64 = 256 * 1024;

/// Load profile: piecewise-linear stages or a scripted `fn shape(t)`.
pub enum Shape {
    Stages { stages: Vec<(Duration, f64)>, total: Duration },
    Script { engine: Box<Engine>, ast: Box<AST>, cap: f64 },
}

impl std::fmt::Debug for Shape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stages { total, .. } => write!(f, "Shape::Stages({total:?})"),
            Self::Script { .. } => write!(f, "Shape::Script"),
        }
    }
}

impl Shape {
    pub fn new(stages: &[Stage]) -> Self {
        let stages: Vec<(Duration, f64)> = stages.iter().map(|s| (s.duration, s.target)).collect();
        let total = stages.iter().map(|(d, _)| *d).sum();
        Self::Stages { stages, total }
    }

    pub fn from_script(path: &Path, cap: f64) -> Result<Self> {
        let meta = std::fs::metadata(path).map_err(|e| Error::config(format!("shape script {}: {e}", path.display())))?;
        if !meta.is_file() || meta.len() > MAX_SHAPE_SCRIPT {
            return Err(Error::config("shape script must be a regular file up to 256 KiB"));
        }
        let src = std::fs::read_to_string(path)?;
        Self::from_source(&src, cap)
    }

    pub fn from_source(src: &str, cap: f64) -> Result<Self> {
        let mut engine = Engine::new();
        engine
            .set_max_operations(200_000)
            .set_max_call_levels(32)
            .set_max_expr_depths(64, 32)
            .set_max_modules(0)
            .set_strict_variables(true);
        engine.disable_symbol("eval");
        engine.on_print(|s| tracing::info!(target: "shape", "{}", sanitize(s, 1000)));
        let ast = engine.compile(src).map_err(|e| Error::config(format!("shape script: {}", sanitize(&e.to_string(), 400))))?;
        if !ast.iter_functions().any(|f| f.name == "shape" && f.params.len() == 1) {
            return Err(Error::config("shape script must define fn shape(t)"));
        }
        let shape = Self::Script { engine: Box::new(engine), ast: Box::new(ast), cap };
        shape.eval(0.0)?;
        Ok(shape)
    }

    fn eval(&self, t: f64) -> Result<Option<f64>> {
        let Self::Script { engine, ast, cap } = self else {
            return Ok(None);
        };
        let out: Dynamic = engine
            .call_fn(&mut Scope::new(), ast, "shape", (t,))
            .map_err(|e| Error::Script(format!("shape({t:.1}): {}", sanitize(&e.to_string(), 400))))?;
        if out.is_unit() {
            return Ok(None);
        }
        let v = out
            .as_float()
            .or_else(|_| out.as_int().map(|i| i as f64))
            .map_err(|_| Error::Script("shape(t) must return a number or ()".into()))?;
        if !v.is_finite() {
            return Err(Error::Script("shape(t) returned a non-finite value".into()));
        }
        Ok(Some(v.clamp(0.0, *cap)))
    }

    /// Known total duration (stages only).
    pub fn total(&self) -> Option<Duration> {
        match self {
            Self::Stages { total, .. } => Some(*total),
            Self::Script { .. } => None,
        }
    }

    /// Known peak target (stages only).
    pub fn peak(&self) -> Option<f64> {
        match self {
            Self::Stages { stages, .. } => Some(stages.iter().map(|(_, t)| *t).fold(0.0, f64::max)),
            Self::Script { .. } => None,
        }
    }

    /// Target at `elapsed`, or `None` once the profile has completed (or the script failed).
    pub fn target_at(&self, elapsed: Duration) -> Option<f64> {
        match self {
            Self::Stages { stages, .. } => {
                let mut start = Duration::ZERO;
                let mut from = 0.0_f64;
                for &(dur, to) in stages {
                    let end = start + dur;
                    if elapsed < end {
                        let frac = (elapsed - start).as_secs_f64() / dur.as_secs_f64();
                        return Some(from + (to - from) * frac);
                    }
                    start = end;
                    from = to;
                }
                None
            }
            Self::Script { .. } => match self.eval(elapsed.as_secs_f64()) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(error = %e, "shape script failed, ending run");
                    None
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolates_linearly() {
        let s = Shape::new(&[
            Stage { duration: Duration::from_secs(10), target: 100.0 },
            Stage { duration: Duration::from_secs(10), target: 100.0 },
        ]);
        assert_eq!(s.target_at(Duration::from_secs(5)), Some(50.0));
        assert_eq!(s.target_at(Duration::from_secs(15)), Some(100.0));
        assert_eq!(s.target_at(Duration::from_secs(20)), None);
        assert_eq!(s.peak(), Some(100.0));
    }

    #[test]
    fn scripted_shape() {
        let s = Shape::from_source("fn shape(t) { if t < 10.0 { t * 2.0 } else { () } }", 1000.0).expect("shape");
        assert_eq!(s.target_at(Duration::from_secs(4)), Some(8.0));
        assert_eq!(s.target_at(Duration::from_secs(11)), None);
    }
}
