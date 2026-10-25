//! Bounded samples of simulation work (including command handling), not frame time.
use serde::Serialize;
use std::{collections::BTreeMap, time::Duration};

#[derive(Clone, Debug, Default, Serialize)]
pub struct Timing {
    pub samples: u64,
    pub p50_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

#[derive(Default)]
pub(crate) struct Timings {
    values: Vec<u64>,
    count: u64,
    max: u64,
}
impl Timings {
    pub fn record(&mut self, elapsed: Duration) {
        let micros = elapsed.as_micros().min(u64::MAX as u128) as u64;
        self.max = self.max.max(micros);
        if self.values.len() < 36_000 {
            self.values.push(micros);
        } else {
            self.values[self.count as usize % 36_000] = micros;
        }
        self.count += 1;
    }
    pub fn report(&self) -> Timing {
        let mut values = self.values.clone();
        values.sort_unstable();
        let percentile = |percent: usize| {
            values
                .get(values.len().saturating_sub(1) * percent / 100)
                .copied()
                .unwrap_or(0) as f64
                / 1000.0
        };
        Timing {
            samples: self.count,
            p50_ms: percentile(50),
            p99_ms: percentile(99),
            max_ms: self.max as f64 / 1000.0,
        }
    }
}

#[derive(Clone, Serialize)]
pub(crate) struct Report {
    pub ticks: Timing,
    pub max_tick_cause: &'static str,
    pub lateness: Timing,
    pub work: BTreeMap<&'static str, Timing>,
}
#[derive(Default)]
pub(crate) struct Metrics {
    pub ticks: Timings,
    max_tick: Duration,
    max_tick_cause: &'static str,
    pub lateness: Timings,
    pub work: BTreeMap<&'static str, Timings>,
}
impl Metrics {
    pub fn tick(&mut self, elapsed: Duration, cause: &'static str) {
        self.ticks.record(elapsed);
        if elapsed > self.max_tick {
            self.max_tick = elapsed;
            self.max_tick_cause = cause;
        }
    }
    pub fn record(&mut self, cause: &'static str, elapsed: Duration) {
        self.work.entry(cause).or_default().record(elapsed);
    }
    pub fn report(&self) -> Report {
        Report {
            ticks: self.ticks.report(),
            max_tick_cause: self.max_tick_cause,
            lateness: self.lateness.report(),
            work: self
                .work
                .iter()
                .map(|(cause, times)| (*cause, times.report()))
                .collect(),
        }
    }
}
