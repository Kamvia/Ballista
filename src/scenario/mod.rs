// File: ballista/src/scenario/mod.rs
pub mod feeder;
pub mod script;
pub mod steps;
pub mod tasks;

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use crate::config::{Plan, Wait};
use crate::error::{Error, Result};
use crate::listener::Listener;
use crate::vu::Vu;
use feeder::Feeders;
use script::ScriptScenario;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// User behaviour. Implement for native (compiled) scenarios; scripted scenarios implement it internally.
pub trait Scenario: Send + Sync + 'static {
    /// Called once before the run with the plan and the scenario's tags.
    /// Return `Ok(false)` when nothing in this scenario survives the tag filter.
    fn prepare(&self, plan: &Plan, scenario_tags: &[String]) -> Result<bool> {
        Ok(plan.tags_allowed(scenario_tags.iter().map(String::as_str)))
    }

    fn on_start<'a>(&'a self, vu: &'a mut Vu) -> BoxFuture<'a, Result<()>> {
        let _ = vu;
        Box::pin(async { Ok(()) })
    }

    fn iteration<'a>(&'a self, vu: &'a mut Vu) -> BoxFuture<'a, Result<()>>;

    fn on_stop<'a>(&'a self, vu: &'a mut Vu) -> BoxFuture<'a, Result<()>> {
        let _ = vu;
        Box::pin(async { Ok(()) })
    }
}

/// Native scenarios and listeners available to plans.
#[derive(Default, Clone)]
pub struct Registry {
    map: HashMap<String, Arc<dyn Scenario>>,
    listeners: Vec<Arc<dyn Listener>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes a scenario available as `native = "<name>"`.
    pub fn register(mut self, name: &str, scenario: impl Scenario) -> Self {
        self.map.insert(name.to_owned(), Arc::new(scenario));
        self
    }

    /// Adds an event listener (request, tick, start and stop hooks).
    pub fn listener(mut self, listener: impl Listener) -> Self {
        self.listeners.push(Arc::new(listener));
        self
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.map.keys().map(String::as_str)
    }

    pub(crate) fn listeners(&self) -> &[Arc<dyn Listener>] {
        &self.listeners
    }
}

#[derive(Clone)]
pub(crate) struct ScenarioEntry {
    pub name: Arc<str>,
    pub weight: u64,
    pub fixed: Option<u64>,
    pub wait: Option<Wait>,
    pub imp: Arc<dyn Scenario>,
}

impl ScenarioEntry {
    pub fn delay_after(&self, iteration: Duration) -> Option<Duration> {
        let d = self.wait?.delay(iteration);
        (!d.is_zero()).then_some(d)
    }
}

pub(crate) struct ScenarioSet {
    pub entries: Vec<ScenarioEntry>,
    cumulative: Vec<u64>,
    total: u64,
}

impl ScenarioSet {
    pub fn build(plan: &Plan, registry: &Registry, feeders: &Arc<Feeders>, shared: &Arc<crate::shared::SharedStore>) -> Result<Self> {
        let mut entries = Vec::with_capacity(plan.scenarios.len());
        for s in &plan.scenarios {
            let imp: Arc<dyn Scenario> = match (&s.script, &s.native) {
                (None, None) if !s.steps.is_empty() => {
                    let src = steps::compile(&s.steps, &s.on_start, &format!("scenario '{}'", s.name))?;
                    let allowed = |task_tags: &[String]| plan.tags_allowed(s.tags.iter().chain(task_tags.iter()).map(String::as_str));
                    Arc::new(
                        ScriptScenario::from_source(&s.name, &src, Arc::clone(feeders), Arc::clone(shared), &allowed)
                            .map_err(|e| Error::config(format!("scenario '{}' steps: {e}", s.name)))?,
                    )
                }
                (Some(path), None) => {
                    Arc::new(ScriptScenario::load(&s.name, path, Arc::clone(feeders), Arc::clone(shared), plan, &s.tags)?)
                }
                (None, Some(native)) => registry
                    .map
                    .get(native)
                    .cloned()
                    .ok_or_else(|| Error::config(format!("scenario '{}' references unknown native '{native}'", s.name)))?,
                _ => return Err(Error::config("scenario must define exactly one of script, native or step")),
            };
            if !imp.prepare(plan, &s.tags)? {
                tracing::info!(scenario = %s.name, "scenario excluded by tag filter");
                continue;
            }
            entries.push(ScenarioEntry {
                name: Arc::from(s.name.as_str()),
                weight: u64::from(s.weight),
                fixed: s.fixed_count,
                wait: s.effective_wait(),
                imp,
            });
        }
        if entries.is_empty() {
            return Err(Error::config("no scenario left to run after applying tag filters"));
        }
        Ok(Self::from_entries(entries))
    }

    fn from_entries(entries: Vec<ScenarioEntry>) -> Self {
        let mut cumulative = Vec::with_capacity(entries.len());
        let mut acc = 0;
        for e in &entries {
            acc += e.weight;
            cumulative.push(acc);
        }
        Self { entries, cumulative, total: acc }
    }

    /// Restricts the set to the named scenarios (dashboard scenario picker).
    pub fn subset(&self, names: &[String]) -> Result<Self> {
        if names.is_empty() || names.len() > self.entries.len() {
            return Err(Error::Engine("select at least one available scenario".into()));
        }
        for n in names {
            if !self.entries.iter().any(|e| &*e.name == n.as_str()) {
                return Err(Error::Engine(format!("unknown scenario '{}'", crate::error::sanitize(n, 64))));
            }
        }
        let entries = self.entries.iter().filter(|e| names.iter().any(|n| n.as_str() == &*e.name)).cloned().collect();
        Ok(Self::from_entries(entries))
    }

    pub fn pick_random(&self) -> usize {
        let r = rand::random_range(0..self.total);
        self.cumulative.partition_point(|&c| c <= r)
    }

    /// Picks the scenario for the next spawned user given current per-scenario counts.
    /// Fixed-count scenarios are filled first; the rest track their weights exactly.
    pub fn choose_for_spawn(&self, counts: &[u64]) -> Option<usize> {
        let fixed: Vec<Option<u64>> = self.entries.iter().map(|e| e.fixed).collect();
        self.choose_with_fixed(counts, &fixed)
    }

    /// Fixed counts for one of `count` workers so that the shares add up to the plan's totals.
    pub fn fixed_share(&self, index: u32, count: u32) -> Vec<Option<u64>> {
        self.entries
            .iter()
            .map(|e| {
                e.fixed.map(|f| {
                    let (n, i) = (u64::from(count.max(1)), u64::from(index));
                    f / n + u64::from(i < f % n)
                })
            })
            .collect()
    }

    pub fn choose_with_fixed(&self, counts: &[u64], fixed: &[Option<u64>]) -> Option<usize> {
        if let Some(i) = fixed.iter().enumerate().find(|(i, f)| f.is_some_and(|f| counts[*i] < f)).map(|(i, _)| i) {
            return Some(i);
        }
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.fixed.is_none())
            .min_by(|(a, ea), (b, eb)| {
                let ra = (counts[*a] + 1) as f64 / ea.weight as f64;
                let rb = (counts[*b] + 1) as f64 / eb.weight as f64;
                ra.total_cmp(&rb).then(a.cmp(b))
            })
            .map(|(i, _)| i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Noop;
    impl Scenario for Noop {
        fn iteration<'a>(&'a self, _vu: &'a mut Vu) -> BoxFuture<'a, Result<()>> {
            Box::pin(async { Ok(()) })
        }
    }

    fn entry(weight: u64, fixed: Option<u64>) -> ScenarioEntry {
        ScenarioEntry { name: Arc::from("x"), weight, fixed, wait: None, imp: Arc::new(Noop) }
    }

    #[test]
    fn spawn_choice_respects_fixed_and_weights() {
        let set = ScenarioSet { entries: vec![entry(1, Some(2)), entry(3, None), entry(1, None)], cumulative: vec![1, 4, 5], total: 5 };
        let mut counts = vec![0_u64; 3];
        for _ in 0..10 {
            let i = set.choose_for_spawn(&counts).expect("choice");
            counts[i] += 1;
        }
        assert_eq!(counts, vec![2, 6, 2]);
    }
}
