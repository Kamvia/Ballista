// File: ballista/src/scenario/tasks.rs
use std::sync::OnceLock;

use super::{BoxFuture, Scenario};
use crate::config::Plan;
use crate::error::{Error, Result};
use crate::vu::Vu;

/// A native task: a plain function taking the virtual user.
pub type TaskFn = for<'a> fn(&'a mut Vu) -> BoxFuture<'a, Result<()>>;

struct TaskDef {
    name: &'static str,
    weight: u64,
    tags: Vec<String>,
    run: TaskFn,
}

struct Selection {
    indices: Vec<usize>,
    cumulative: Vec<u64>,
    total: u64,
}

/// Weighted or sequential task group for compiled scenarios, with tag filtering.
pub struct Tasks {
    defs: Vec<TaskDef>,
    sequential: bool,
    on_start: Option<TaskFn>,
    on_stop: Option<TaskFn>,
    selection: OnceLock<Selection>,
}

impl Default for Tasks {
    fn default() -> Self {
        Self::new()
    }
}

impl Tasks {
    pub fn new() -> Self {
        Self { defs: Vec::new(), sequential: false, on_start: None, on_stop: None, selection: OnceLock::new() }
    }

    /// Runs tasks in declaration order, one per iteration, cycling per user.
    pub fn sequential(mut self) -> Self {
        self.sequential = true;
        self
    }

    pub fn task(self, name: &'static str, weight: u32, run: TaskFn) -> Self {
        self.tagged(name, weight, &[], run)
    }

    pub fn tagged(mut self, name: &'static str, weight: u32, tags: &[&str], run: TaskFn) -> Self {
        self.defs.push(TaskDef {
            name,
            weight: u64::from(weight.clamp(1, 10_000)),
            tags: tags.iter().map(|t| (*t).to_owned()).collect(),
            run,
        });
        self
    }

    pub fn on_start(mut self, run: TaskFn) -> Self {
        self.on_start = Some(run);
        self
    }

    pub fn on_stop(mut self, run: TaskFn) -> Self {
        self.on_stop = Some(run);
        self
    }

    pub fn task_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.defs.iter().map(|d| d.name)
    }

    fn selected(&self) -> Result<&Selection> {
        self.selection.get().ok_or_else(|| Error::Engine("task group used before prepare".into()))
    }
}

impl Scenario for Tasks {
    fn prepare(&self, plan: &Plan, scenario_tags: &[String]) -> Result<bool> {
        for d in &self.defs {
            for t in &d.tags {
                if !crate::config::is_valid_name(t) {
                    return Err(Error::config(format!("task '{}' has invalid tag '{t}'", d.name)));
                }
            }
        }
        let sel = self.selection.get_or_init(|| {
            let mut indices = Vec::new();
            let mut cumulative = Vec::new();
            let mut acc = 0;
            for (i, d) in self.defs.iter().enumerate() {
                let tags = scenario_tags.iter().chain(d.tags.iter()).map(String::as_str);
                if plan.tags_allowed(tags) {
                    acc += d.weight;
                    indices.push(i);
                    cumulative.push(acc);
                }
            }
            Selection { indices, cumulative, total: acc }
        });
        Ok(!sel.indices.is_empty())
    }

    fn on_start<'a>(&'a self, vu: &'a mut Vu) -> BoxFuture<'a, Result<()>> {
        match self.on_start {
            Some(f) => f(vu),
            None => Box::pin(async { Ok(()) }),
        }
    }

    fn iteration<'a>(&'a self, vu: &'a mut Vu) -> BoxFuture<'a, Result<()>> {
        let sel = match self.selected() {
            Ok(s) => s,
            Err(e) => return Box::pin(async move { Err(e) }),
        };
        let pos = if self.sequential {
            let p = vu.task_cursor % sel.indices.len();
            vu.task_cursor = vu.task_cursor.wrapping_add(1);
            p
        } else {
            let r = rand::random_range(0..sel.total);
            sel.cumulative.partition_point(|&c| c <= r)
        };
        let def = &self.defs[sel.indices[pos]];
        let scenario = std::sync::Arc::clone(vu.scenario_arc());
        vu.ctx.count_task(&scenario, def.name);
        (def.run)(vu)
    }

    fn on_stop<'a>(&'a self, vu: &'a mut Vu) -> BoxFuture<'a, Result<()>> {
        match self.on_stop {
            Some(f) => f(vu),
            None => Box::pin(async { Ok(()) }),
        }
    }
}
