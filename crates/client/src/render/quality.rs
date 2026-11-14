//! Auto quality: moves between the fixed levels from how long frames take
//! against the display's refresh interval. It is plain arithmetic on a stream
//! of frame costs, so its choices are testable without a GPU.
use super::settings::QualityLevel;

/// Frames judged together when deciding to step down.
const DOWN_FRAMES: usize = 60;
/// Frames judged together, at one level, before stepping up.
const UP_FRAMES: usize = 360;
/// Frames ignored after a change: the new targets and pipelines cost a hitch.
const SETTLE_FRAMES: u32 = 30;
/// A step down happens when the typical frame costs more than this much of the
/// refresh interval. Leaving a tenth lets a frame absorb the CPU's jitter.
const DOWN_AT: f32 = 0.9;
/// A step up needs the typical frame under this much of the interval and the
/// slowest tenth under `UP_SLOW`. The next level costs up to half as much
/// again, so this stays clear of `DOWN_AT` once the cost has grown, and a
/// step up does not bounce straight back.
const UP_AT: f32 = 0.5;
const UP_SLOW: f32 = 0.7;
/// Without a GPU time, the frame interval stands in. Presenting in step with
/// the display makes it equal to the refresh interval whenever the GPU keeps
/// up, so only a missed refresh shows.
const INTERVAL_DOWN_AT: f32 = 1.15;
/// Seen from intervals alone, headroom is invisible, so Auto tries the next
/// level up after this many steady frames, and each try that fails makes the
/// next wait twice as long, up to `MAX_PENALTY` times.
const PROBE_FRAMES: usize = 1800;
/// How long a level reached by stepping up must hold before the step counts as
/// a success.
const SUCCESS_FRAMES: u32 = 480;
const MAX_PENALTY: u32 = 16;
/// Frames dropped at a time from the oldest end of the record.
const TRIM_FRAMES: usize = 512;
/// How often, in frames, each decision is looked at: sorting a window every
/// frame would cost more than the frame saves.
const DOWN_EVERY: u32 = 8;
const UP_EVERY: u32 = 64;

/// What a sample measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Measure {
    /// The GPU's time for the frame: it shows how much room is left.
    GpuTime,
    /// The time between frames, on a display that presents in step: it shows
    /// only whether a refresh was missed.
    Interval,
}

pub struct AutoQuality {
    level: QualityLevel,
    samples: Vec<f32>,
    ignore: u32,
    /// Frames at the current level since the last change.
    held: u32,
    /// The level was reached by stepping up and has not yet proved itself.
    probing: bool,
    /// How much longer than usual to wait before stepping up, after tries
    /// that failed.
    penalty: u32,
}

impl Default for AutoQuality {
    fn default() -> Self {
        Self::new(QualityLevel::START)
    }
}

impl AutoQuality {
    pub fn new(level: QualityLevel) -> Self {
        Self {
            level,
            samples: Vec::new(),
            ignore: SETTLE_FRAMES,
            held: 0,
            probing: false,
            penalty: 1,
        }
    }

    pub fn level(&self) -> QualityLevel {
        self.level
    }

    /// Take the cost of one frame, in milliseconds, against the refresh
    /// interval `budget`. Returns the new level when it changes, which is by
    /// one step at a time.
    pub fn observe(&mut self, sample: f32, budget: f32, measure: Measure) -> Option<QualityLevel> {
        if !sample.is_finite() || !budget.is_finite() || budget <= 0.0 {
            return None;
        }
        if self.ignore > 0 {
            self.ignore -= 1;
            return None;
        }
        self.held = self.held.saturating_add(1);
        if self.probing && self.held >= SUCCESS_FRAMES {
            self.probing = false;
            self.penalty = (self.penalty / 2).max(1);
        }
        self.samples.push(sample);
        // Only the newest frames are ever judged; trim in blocks.
        let keep = PROBE_FRAMES * self.penalty as usize;
        if self.samples.len() > keep + TRIM_FRAMES {
            self.samples.drain(..TRIM_FRAMES);
        }
        let down_at = match measure {
            Measure::GpuTime => DOWN_AT,
            Measure::Interval => INTERVAL_DOWN_AT,
        };
        if self.samples.len() >= DOWN_FRAMES && self.held.is_multiple_of(DOWN_EVERY) {
            let recent = &self.samples[self.samples.len() - DOWN_FRAMES..];
            if median(recent) > budget * down_at && self.level > QualityLevel::Low {
                if self.probing {
                    self.penalty = (self.penalty * 2).min(MAX_PENALTY);
                }
                return Some(self.step(QualityLevel::from_index(self.level.index() - 1), false));
            }
        }
        if self.level < QualityLevel::Ultra && self.held.is_multiple_of(UP_EVERY) {
            let window = match measure {
                Measure::GpuTime => UP_FRAMES,
                Measure::Interval => PROBE_FRAMES,
            } * self.penalty as usize;
            if self.samples.len() >= window {
                let recent = &self.samples[self.samples.len() - window..];
                let steady = match measure {
                    Measure::GpuTime => {
                        median(recent) < budget * UP_AT && quantile(recent, 0.9) < budget * UP_SLOW
                    }
                    Measure::Interval => quantile(recent, 0.9) <= budget * INTERVAL_DOWN_AT,
                };
                if steady {
                    return Some(self.step(QualityLevel::from_index(self.level.index() + 1), true));
                }
            }
        }
        None
    }

    fn step(&mut self, level: QualityLevel, up: bool) -> QualityLevel {
        self.level = level;
        self.samples.clear();
        self.ignore = SETTLE_FRAMES;
        self.held = 0;
        self.probing = up;
        level
    }
}

fn quantile(values: &[f32], fraction: f32) -> f32 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f32::total_cmp);
    sorted[((sorted.len() - 1) as f32 * fraction).round() as usize]
}

fn median(values: &[f32]) -> f32 {
    quantile(values, 0.5)
}

#[cfg(test)]
mod tests {
    use super::*;
    use QualityLevel::*;

    /// 165 Hz.
    const BUDGET: f32 = 6.06;

    fn run(
        auto: &mut AutoQuality,
        frames: usize,
        mut cost: impl FnMut(QualityLevel) -> f32,
    ) -> Vec<(usize, QualityLevel)> {
        let mut changes = Vec::new();
        for frame in 0..frames {
            if let Some(level) = auto.observe(cost(auto.level()), BUDGET, Measure::GpuTime) {
                changes.push((frame, level));
            }
        }
        changes
    }

    #[test]
    fn a_frame_that_fits_stays_where_it_is() {
        let mut auto = AutoQuality::default();
        // Over half the budget, under nine tenths: neither up nor down.
        assert!(run(&mut auto, 5000, |_| BUDGET * 0.7).is_empty());
        assert_eq!(auto.level(), High);
    }

    #[test]
    fn a_slow_frame_steps_down_one_level_at_a_time() {
        let mut auto = AutoQuality::default();
        let changes = run(&mut auto, 5000, |_| BUDGET * 1.3);
        let levels: Vec<_> = changes.iter().map(|c| c.1).collect();
        assert_eq!(levels, [Medium, Low]);
        // Each step waits for a fresh window of frames.
        assert!(changes[0].0 >= SETTLE_FRAMES as usize + DOWN_FRAMES - 1);
        assert!(changes[1].0 - changes[0].0 >= DOWN_FRAMES);
        assert_eq!(auto.level(), Low, "never below the lowest level");
    }

    #[test]
    fn a_few_slow_frames_do_not_step_down() {
        let mut auto = AutoQuality::default();
        let mut frame = 0;
        let changes = run(&mut auto, 5000, |_| {
            frame += 1;
            // One frame in ten hitches to three times the budget.
            if frame % 10 == 0 {
                BUDGET * 3.0
            } else {
                BUDGET * 0.7
            }
        });
        assert!(changes.is_empty(), "{changes:?}");
    }

    #[test]
    fn a_cheap_frame_steps_back_up_but_not_before_a_long_steady_stretch() {
        let mut auto = AutoQuality::new(Low);
        let changes = run(&mut auto, 20_000, |_| BUDGET * 0.3);
        let levels: Vec<_> = changes.iter().map(|c| c.1).collect();
        assert_eq!(levels, [Medium, High, Ultra]);
        assert!(changes[0].0 >= UP_FRAMES, "{changes:?}");
        assert_eq!(auto.level(), Ultra, "never above the highest level");
    }

    #[test]
    fn it_settles_on_the_best_level_that_fits_and_stays_there() {
        // Each level costs half again as much as the one below, and Medium is
        // the best that fits: the frame costs 0.62 of the budget there.
        let cost = |level: QualityLevel| BUDGET * 0.62 * 1.5_f32.powi(level.index() as i32 - 1);
        for start in QualityLevel::ALL {
            let mut auto = AutoQuality::new(start);
            let changes = run(&mut auto, 60_000, cost);
            assert_eq!(auto.level(), Medium, "from {start:?}: {changes:?}");
            assert!(changes.len() <= 3, "from {start:?}: {changes:?}");
            let last = changes.last().map_or(0, |c| c.0);
            assert!(last < 5000, "kept moving until {last}: {changes:?}");
        }
    }

    #[test]
    fn a_step_up_that_misses_is_retried_less_and_less_often() {
        // The level above always costs too much, as seen by the GPU.
        let cost = |level: QualityLevel| {
            if level == Medium {
                BUDGET * 0.3
            } else {
                BUDGET * 1.4
            }
        };
        let mut auto = AutoQuality::new(Medium);
        let changes = run(&mut auto, 200_000, cost);
        let tries: Vec<usize> = changes
            .iter()
            .filter(|c| c.1 == High)
            .map(|c| c.0)
            .collect();
        assert!(tries.len() >= 3, "{changes:?}");
        let gaps: Vec<usize> = tries.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(gaps.windows(2).all(|g| g[1] >= g[0]), "{gaps:?}");
        assert!(gaps.last().unwrap() > &(gaps[0] * 2 - 1), "{gaps:?}");
        // Each try is one short stay, not a lasting change.
        assert_eq!(auto.level(), Medium);
    }

    #[test]
    fn frame_intervals_alone_step_down_on_a_missed_refresh_and_probe_up_slowly() {
        let mut auto = AutoQuality::default();
        // A display that presents in step shows the refresh interval when the
        // GPU keeps up, and a multiple of it when a refresh is missed.
        let interval = |level: QualityLevel| {
            if level >= High {
                BUDGET * 2.0
            } else {
                BUDGET
            }
        };
        let mut changes = Vec::new();
        for frame in 0..60_000 {
            if let Some(level) = auto.observe(interval(auto.level()), BUDGET, Measure::Interval) {
                changes.push((frame, level));
            }
        }
        assert_eq!(changes[0].1, Medium);
        // Probing back up to High fails each time, and the tries thin out.
        let tries: Vec<usize> = changes
            .iter()
            .filter(|c| c.1 == High)
            .map(|c| c.0)
            .collect();
        assert!(tries.len() >= 3, "{changes:?}");
        let gaps: Vec<usize> = tries.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(gaps.windows(2).all(|g| g[1] > g[0]), "{gaps:?}");
        assert_eq!(auto.level(), Medium);
    }

    #[test]
    fn nonsense_samples_change_nothing() {
        let mut auto = AutoQuality::default();
        for _ in 0..1000 {
            assert_eq!(auto.observe(f32::NAN, BUDGET, Measure::GpuTime), None);
            assert_eq!(auto.observe(100.0, 0.0, Measure::GpuTime), None);
        }
        assert_eq!(auto.level(), High);
    }
}
