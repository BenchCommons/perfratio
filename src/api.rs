//! Pre-stable public API: measure a command, sample energy, summarize samples.
//!
//! Everything reachable from this module is documented; it may change in any
//! `0.x` release.
#![warn(missing_docs)]

use std::process::{Command, Stdio};

use anyhow::{bail, Result};

use crate::energy::{get_energy_sampler, EnergySampler};
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
