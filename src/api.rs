//! Pre-stable public API: measure a command, sample energy, summarize samples.
//!
//! Everything reachable from this module is documented; it may change in any
//! `0.x` release.
#![warn(missing_docs)]

use std::process::{Command, Stdio};

use anyhow::{bail, Result};

pub use crate::benchmark::benchmark_result::BenchmarkResult;
pub use crate::stats::deep::DeepStats;

use crate::benchmark::executor::{Executor, MockExecutor, RawExecutor};
use crate::energy::{get_energy_sampler, EnergySampler};
use crate::options::{ExecutorKind, Options, OutputStyleOption, RunBounds};
use crate::timer::execute_and_measure;
use crate::util::priority::Priority;

/// Raw per-run results of [`measure`]. All vectors have one entry per run.
#[derive(Debug, Clone, Default)]
pub struct Measurement {
    /// Wall-clock time of each run, in seconds.
    pub real: Vec<f64>,
    /// User CPU time of each run, in seconds.
    pub user: Vec<f64>,
    /// System CPU time of each run, in seconds.
    pub system: Vec<f64>,
    /// Peak memory of each run, in bytes.
    pub memory_bytes: Vec<u64>,
    /// Energy of each run in joules, or `None` if no sampler is available.
    pub energy_joules: Vec<Option<f64>>,
    /// Exit code of each run, or `None` if the process was killed by a signal.
    pub exit_codes: Vec<Option<i32>>,
}

/// Run `argv` (program followed by arguments, no shell) `runs` times and
/// collect the measurements. The command's output is discarded.
///
/// # Errors
/// Fails if `argv` is empty, `runs` is 0 or the program cannot be started.
/// A non-zero exit code is not an error; see [`Measurement::exit_codes`].
pub fn measure(argv: &[&str], runs: u64) -> Result<Measurement> {
    let Some((program, args)) = argv.split_first() else {
        bail!("argv must contain the program to run");
    };
    if runs == 0 {
        bail!("runs must be at least 1");
    }
    let mut sampler = EnergyMeter::new();
    let mut result = Measurement::default();
    for _ in 0..runs {
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        sampler.start();
        let timing = execute_and_measure(
            command,
            None,
            Priority::Normal,
            false,
            None,
            None,
            None,
            None,
        )?;
        let energy = sampler.stop();
        result.real.push(timing.time_real);
        result.user.push(timing.time_user);
        result.system.push(timing.time_system);
        result.memory_bytes.push(timing.memory_usage_byte);
        result.energy_joules.push(energy);
        result.exit_codes.push(timing.status.code());
    }
    Ok(result)
}

/// Result of [`Benchmark::run`].
#[derive(Debug, Clone)]
pub struct BenchmarkReport {
    /// Per-run samples and summary statistics of the benchmarked command.
    pub result: BenchmarkResult,
    /// Bootstrap confidence intervals of the wall-clock time. `None` unless
    /// requested with [`Benchmark::deep_stats`], or with fewer than 3 runs.
    pub deep_stats: Option<DeepStats>,
    /// Diagnostics such as "outliers detected", as the command-line tool would
    /// print them. Nothing is printed by the library.
    pub warnings: Vec<String>,
}

/// Builder for a full benchmark of one command: warmup, timed runs, energy
/// and statistics, as `perfratio` does it, but without terminal output.
///
/// ```
/// use perfratio::api::Benchmark;
///
/// # fn main() -> anyhow::Result<()> {
/// # if cfg!(unix) {
/// let report = Benchmark::new(["true"]).runs(5).warmup(1).deep_stats(true).run()?;
/// assert_eq!(report.result.times.as_ref().map(Vec::len), Some(5));
/// assert!(report.result.mean > 0.0);
/// # }
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct Benchmark {
    argv: Vec<String>,
    runs: u64,
    warmup: u64,
    energy: bool,
    deep_stats: bool,
    executor_kind: ExecutorKind,
}

impl Benchmark {
    /// Benchmark the program `argv[0]` with the arguments `argv[1..]`. The
    /// command is run without a shell and its output is discarded.
    pub fn new<I, S>(argv: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Benchmark {
            argv: argv.into_iter().map(Into::into).collect(),
            runs: 10,
            warmup: 0,
            energy: false,
            deep_stats: false,
            executor_kind: ExecutorKind::Raw,
        }
    }

    /// Number of timed runs (default 10). Must be at least 1.
    pub fn runs(mut self, runs: u64) -> Self {
        self.runs = runs;
        self
    }

    /// Number of untimed warmup runs before the timed runs (default 0).
    pub fn warmup(mut self, warmup: u64) -> Self {
        self.warmup = warmup;
        self
    }

    /// Measure energy with the best sampler available (RAPL on bare-metal
    /// Linux). Without a sampler the energy fields of the result stay empty.
    pub fn energy(mut self, enabled: bool) -> Self {
        self.energy = enabled;
        self
    }

    /// Compute bootstrap confidence intervals into [`BenchmarkReport::deep_stats`].
    pub fn deep_stats(mut self, enabled: bool) -> Self {
        self.deep_stats = enabled;
        self
    }

    /// Simulate the command instead of running it (deterministic tests).
    #[cfg(test)]
    fn mocked(mut self) -> Self {
        self.executor_kind = ExecutorKind::Mock(None);
        self
    }

    /// Run the benchmark.
    ///
    /// # Errors
    /// Fails if the command is empty, `runs` is 0 or the command cannot be
    /// started. A non-zero exit code is not an error; see
    /// [`BenchmarkResult::exit_codes`].
    pub fn run(&self) -> Result<BenchmarkReport> {
        if self.argv.is_empty() {
            bail!("argv must contain the program to run");
        }
        if self.runs == 0 {
            bail!("runs must be at least 1");
        }
        let mut options = Options {
            run_bounds: RunBounds {
                min: self.runs,
                max: Some(self.runs),
            },
            warmup_count: self.warmup,
            measure_energy: self.energy,
            deep_stats: self.deep_stats,
            output_style: OutputStyleOption::Disabled,
            executor_kind: self.executor_kind.clone(),
            executor_kinds: vec![self.executor_kind.clone()],
            command_failure_action: crate::options::CmdFailureAction::IgnoreAllFailures,
            ..Options::default()
        };
        // Let the number of runs decide, not the minimum benchmarking time
        options.min_benchmarking_time = 0.0;

        let argv: Vec<&str> = self.argv.iter().map(String::as_str).collect();
        let command = crate::command::Command::from_argv(None, argv, Vec::new());
        let mut executor: Box<dyn Executor> = match &self.executor_kind {
            ExecutorKind::Mock(shell) => Box::new(MockExecutor::new(shell.clone(), None)),
            _ => Box::new(RawExecutor::new(&options)),
        };
        executor.calibrate()?;
        let benchmark =
            crate::benchmark::Benchmark::new(0, 0, &command, &options, executor.as_ref());
        let Some((result, warnings)) = benchmark.run_quiet()? else {
            bail!("the benchmark was interrupted before any run completed");
        };
        let deep_stats = if self.deep_stats {
            result
                .times
                .as_deref()
                .and_then(crate::stats::deep::compute_deep_stats)
        } else {
            None
        };
        Ok(BenchmarkReport {
            result,
            deep_stats,
            warnings,
        })
    }
}

/// Energy sampler (RAPL on bare-metal Linux with readable `energy_uj`).
/// On other systems it reports as unavailable and `stop` returns `None`.
pub struct EnergyMeter(Box<dyn EnergySampler>);

impl EnergyMeter {
    /// Pick the best sampler available on this system.
    pub fn new() -> Self {
        EnergyMeter(get_energy_sampler(crate::energy::BackendPreference::Auto))
    }

    /// Whether energy readings are available.
    pub fn is_available(&self) -> bool {
        self.0.is_available()
    }

    /// Start a measurement window.
    pub fn start(&mut self) {
        self.0.start();
    }

    /// End the window and return the energy used in joules, if known.
    pub fn stop(&mut self) -> Option<f64> {
        self.0.stop()
    }
}

impl Default for EnergyMeter {
    fn default() -> Self {
        Self::new()
    }
}

/// Basic statistics on samples.
pub mod stats {
    /// Arithmetic mean. An empty slice gives NaN.
    pub fn mean(values: &[f64]) -> f64 {
        crate::stats::basic::mean(values)
    }

    /// Median (mean of the two middle values for an even count).
    ///
    /// # Panics
    /// If `values` is empty.
    pub fn median(values: &[f64]) -> f64 {
        crate::stats::basic::median(values)
    }

    /// Sample standard deviation (n - 1 denominator).
    pub fn standard_deviation(values: &[f64]) -> f64 {
        crate::stats::basic::standard_deviation(values, None)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn measures_runs() {
        let m = measure(&["true"], 2).unwrap();
        assert_eq!(m.real.len(), 2);
        assert_eq!(m.exit_codes, vec![Some(0), Some(0)]);
    }

    #[test]
    fn non_zero_exit_is_recorded() {
        assert_eq!(measure(&["false"], 1).unwrap().exit_codes, vec![Some(1)]);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(measure(&[], 1).is_err());
        assert!(measure(&["true"], 0).is_err());
        assert!(measure(&["/nonexistent/perfratio-x"], 1).is_err());
    }

    #[test]
    fn basic_stats() {
        assert_eq!(stats::mean(&[1.0, 3.0]), 2.0);
        assert_eq!(stats::median(&[1.0, 2.0, 9.0]), 2.0);
    }
}

#[cfg(test)]
mod benchmark_tests {
    use super::*;

    #[test]
    fn mocked_benchmark_returns_result_and_deep_stats() {
        let report = Benchmark::new(["sleep", "0.01"])
            .mocked()
            .runs(6)
            .warmup(2)
            .deep_stats(true)
            .run()
            .unwrap();
        assert_eq!(report.result.times.as_ref().map(Vec::len), Some(6));
        assert_eq!(report.result.exit_codes.len(), 6);
        assert!(report.result.mean > 0.0);
        let deep = report.deep_stats.expect("6 runs are enough for deep stats");
        assert!(deep.mean_ci_lower <= report.result.mean);
        assert!(report.result.mean <= deep.mean_ci_upper);
    }

    #[test]
    fn deep_stats_are_opt_in() {
        let report = Benchmark::new(["sleep", "0.01"])
            .mocked()
            .runs(5)
            .run()
            .unwrap();
        assert!(report.deep_stats.is_none());
    }

    #[test]
    fn energy_is_collected_when_requested() {
        let report = Benchmark::new(["sleep", "0.01"])
            .mocked()
            .runs(3)
            .energy(true)
            .run()
            .unwrap();
        assert!(report.result.mean_energy_joules.is_some());
    }

    #[test]
    fn rejects_bad_input() {
        assert!(Benchmark::new(Vec::<String>::new()).run().is_err());
        assert!(Benchmark::new(["sleep", "0.01"])
            .mocked()
            .runs(0)
            .run()
            .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn runs_a_real_program() {
        let report = Benchmark::new(["true"]).runs(3).warmup(1).run().unwrap();
        assert_eq!(report.result.times.as_ref().map(Vec::len), Some(3));
        assert_eq!(report.result.exit_codes, vec![Some(0); 3]);
    }

    #[cfg(unix)]
    #[test]
    fn non_zero_exit_is_recorded_not_an_error() {
        let report = Benchmark::new(["false"]).runs(2).run().unwrap();
        assert_eq!(report.result.exit_codes, vec![Some(1); 2]);
    }
}
