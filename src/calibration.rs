//! Measurement floor (`--calibrate`, `--below-floor`): what the runner itself
//! costs, measured with a trivial command through the same spawn path as the
//! benchmark. A command whose time is within a small multiple of that floor is
//! dominated by process creation and timer resolution, so its numbers are
//! flagged. See `docs/specs/measurement-floor.md`.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::benchmark::executor::{BenchmarkIteration, Executor};
use crate::command::Command;
use crate::options::{CmdFailureAction, CommandOutputPolicy, Options};
use crate::stats::basic::median;
use crate::util::units::Second;

/// A command is below resolution when its mean is less than this many floors
pub const BELOW_FLOOR_FACTOR: f64 = 3.0;
/// Runs of the trivial command (after [`WARMUP_RUNS`])
pub const CALIBRATION_RUNS: usize = 20;
const WARMUP_RUNS: usize = 2;
/// Fewest runs for a floor estimate
const MIN_RUNS: usize = 5;
/// Calibration stops collecting after this long (but keeps at least [`MIN_RUNS`])
pub const CALIBRATION_BUDGET: Duration = Duration::from_millis(1000);

/// `--calibrate`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CalibrateMode {
    /// Measure the floor once per session (per distinct shell)
    #[default]
    Auto,
    Off,
}

impl CalibrateMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(CalibrateMode::Auto),
            "off" => Some(CalibrateMode::Off),
            _ => None,
        }
    }
}

/// `--below-floor`: what a result within the floor does
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BelowFloorAction {
    /// The check is a warning
    #[default]
    Warn,
    /// The check fails and perfratio exits with code 6 after the report
    Fail,
}

impl BelowFloorAction {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "warn" => Some(BelowFloorAction::Warn),
            "fail" => Some(BelowFloorAction::Fail),
            _ => None,
        }
    }
}

/// The measured runner floor; the `perfratio.calibration` object of the JSON
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Floor {
    /// Median wall time of the trivial command in seconds, net of the shell
    /// spawn time the runner subtracts from every run
    pub wall_floor: Second,
    /// Median absolute deviation of the wall times, the uncertainty of the floor
    pub wall_floor_mad: Second,
    /// Median peak RSS of the trivial command in bytes. Omitted when the
    /// platform reports none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rss_floor: Option<u64>,
    /// Number of runs behind the estimate
    pub runs: usize,
}

/// What is known about the floor of an executor
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum FloorState {
    /// No floor was asked for (`--calibrate=off`) or the executor has none (mock)
    #[default]
    Off,
    /// It was asked for but could not be measured
    Unavailable,
    Measured(Floor),
}

impl Floor {
    /// The floor from the raw wall times and peak RSS of the trivial command.
    /// `overhead` is what the runner subtracts from every run of this executor.
    /// `None` below [`MIN_RUNS`] samples.
    pub fn from_samples(wall: &[Second], rss: &[u64], overhead: Second) -> Option<Self> {
        if wall.len() < MIN_RUNS || wall.iter().any(|t| !t.is_finite()) {
            return None;
        }
        let center = median(wall);
        let deviations: Vec<f64> = wall.iter().map(|t| (t - center).abs()).collect();
        let rss_floor = if rss.is_empty() {
            None
        } else {
            let values: Vec<f64> = rss.iter().map(|&b| b as f64).collect();
            // 0 is "not reported", not a floor of zero bytes
            Some(median(&values) as u64).filter(|&b| b > 0)
        };
        Some(Floor {
            wall_floor: (center - overhead).max(0.0),
            wall_floor_mad: median(&deviations),
            rss_floor,
            runs: wall.len(),
        })
    }

    /// Whether a result with this `mean` and relative 95% CI half-width is
    /// within the floor: its mean is below [`BELOW_FLOOR_FACTOR`] floors, or
    /// the lower end of its CI reaches the floor (plus its MAD, so the
    /// uncertainty of the floor is not treated as exact).
    pub fn classify(&self, mean: Second, relative_half_width: Option<f64>) -> FloorVerdict {
        let half_width = relative_half_width.map_or(0.0, |h| h * mean);
        let below = mean < BELOW_FLOOR_FACTOR * self.wall_floor
            || mean - half_width <= self.wall_floor + self.wall_floor_mad;
        FloorVerdict {
            below,
            ratio: (self.wall_floor > 0.0).then(|| mean / self.wall_floor),
        }
    }
}

/// Outcome of [`Floor::classify`]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FloorVerdict {
    pub below: bool,
    /// Mean in floors; `None` when the floor is 0
    pub ratio: Option<f64>,
}

/// The trivial command of the platform for this kind of executor
fn trivial_command(executor: &dyn Executor) -> &'static str {
    if executor.uses_shell() {
        // A builtin of sh, cmd and PowerShell alike
        "exit 0"
    } else if cfg!(windows) {
        "cmd /C exit 0"
    } else {
        "true"
    }
}

/// Measure the floor with `executor`: the same spawn path, shell, affinity,
/// priority and output policy as the benchmarks. Bounded by
/// [`CALIBRATION_BUDGET`]; any failure gives [`FloorState::Unavailable`].
pub fn measure_floor(executor: &dyn Executor, options: &Options) -> FloorState {
    let command = Command::new(None, trivial_command(executor));
    let policy = options
        .command_output_policies
        .first()
        .unwrap_or(&CommandOutputPolicy::Null);
    let started = Instant::now();
    let mut wall = Vec::with_capacity(CALIBRATION_RUNS);
    let mut rss = Vec::with_capacity(CALIBRATION_RUNS);
    for i in 0..WARMUP_RUNS + CALIBRATION_RUNS {
        if crate::util::interrupt::interrupted() {
            return FloorState::Unavailable;
        }
        let Ok((result, _)) = executor.run_command_and_measure(
            &command,
            BenchmarkIteration::NonBenchmarkRun,
            Some(CmdFailureAction::RaiseError),
            policy,
        ) else {
            return FloorState::Unavailable;
        };
        if i >= WARMUP_RUNS {
            wall.push(result.time_real);
            rss.push(result.memory_usage_byte);
            if wall.len() >= MIN_RUNS && started.elapsed() > CALIBRATION_BUDGET {
                break;
            }
        }
    }
    match Floor::from_samples(&wall, &rss, executor.time_overhead()) {
        Some(floor) => FloorState::Measured(floor),
        None => FloorState::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn floor(wall: Second, mad: Second) -> Floor {
        Floor {
            wall_floor: wall,
            wall_floor_mad: mad,
            rss_floor: None,
            runs: 20,
        }
    }

    #[test]
    fn floor_is_the_median_net_of_the_overhead() {
        let wall = [0.0010, 0.0011, 0.0012, 0.0010, 0.0050];
        let f = Floor::from_samples(&wall, &[100, 300, 200, 200, 200], 0.0004).unwrap();
        assert!((f.wall_floor - 0.0007).abs() < 1e-12);
        // |x - 0.0011| = 1e-4, 0, 1e-4, 1e-4, 3.9e-3: median 1e-4
        assert!((f.wall_floor_mad - 0.0001).abs() < 1e-12);
        assert_eq!(f.rss_floor, Some(200));
        assert_eq!(f.runs, 5);
    }

    #[test]
    fn floor_is_never_negative_and_unreported_rss_is_omitted() {
        let f = Floor::from_samples(&[0.001; 6], &[0; 6], 0.002).unwrap();
        assert_eq!(f.wall_floor, 0.0);
        assert_eq!(f.rss_floor, None);
    }

    #[test]
    fn too_few_or_invalid_samples_give_no_floor() {
        assert!(Floor::from_samples(&[0.001; 4], &[], 0.0).is_none());
        assert!(Floor::from_samples(&[0.001, 0.001, 0.001, 0.001, f64::NAN], &[], 0.0).is_none());
    }

    #[test]
    fn mean_below_three_floors_is_below_resolution() {
        let f = floor(0.001, 0.0);
        assert!(f.classify(0.0029, Some(0.001)).below);
        assert!(!f.classify(0.0031, Some(0.001)).below);
        assert!(!f.classify(0.0031, None).below);
    }

    #[test]
    fn a_ci_reaching_the_floor_is_below_resolution() {
        let f = floor(0.001, 0.0);
        // 10 ms, but ±100%: the lower end of the CI is 0
        assert!(f.classify(0.010, Some(1.0)).below);
        // 10 ms ± 5%
        assert!(!f.classify(0.010, Some(0.05)).below);
    }

    #[test]
    fn floor_uncertainty_widens_the_threshold() {
        // 3.5 ms is above 3 floors; its CI lower end 3.4 ms is below floor + MAD
        let f = floor(0.001, 0.0025);
        assert!(f.classify(0.0035, Some(0.02)).below);
        assert!(!floor(0.001, 0.0).classify(0.0035, Some(0.02)).below);
    }

    #[test]
    fn ratio_is_in_floors() {
        let v = floor(0.001, 0.0).classify(0.01, None);
        assert_eq!(v.ratio, Some(10.0));
        assert_eq!(floor(0.0, 0.0).classify(0.01, None).ratio, None);
    }

    #[test]
    fn modes_parse() {
        assert_eq!(CalibrateMode::parse("auto"), Some(CalibrateMode::Auto));
        assert_eq!(CalibrateMode::parse("off"), Some(CalibrateMode::Off));
        assert_eq!(CalibrateMode::parse("x"), None);
        assert_eq!(
            BelowFloorAction::parse("fail"),
            Some(BelowFloorAction::Fail)
        );
        assert_eq!(
            BelowFloorAction::parse("warn"),
            Some(BelowFloorAction::Warn)
        );
    }
}
