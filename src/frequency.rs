//! Effective CPU frequency and throttling detection (#222).
//!
//! The frequency of one run is `cycles / time on CPU`. The time source must
//! match the privilege scope of `cycles` (see `docs/specs/effective-frequency.md`):
//! `cycles` over user + kernel is divided by `task-clock`, user-only `cycles`
//! by the user CPU time of the run. Dividing `cycles:u` by `task-clock` would
//! be low by the kernel fraction and is never done.
//!
//! Everything here is pure and platform independent except
//! [`max_frequency_hz`], which reads `/sys/devices/system/cpu/cpu*/cpufreq`
//! on Linux.

use crate::stats::basic;
use std::path::Path;

/// Runs whose time on CPU is below this are shown in the series but excluded
/// from the mean, the standard deviation and the warnings: the clock ramp and
/// the exec path dominate there.
pub const MIN_RUN_SECONDS: f64 = 0.005;
/// Fewest eligible runs needed for a warning.
pub const MIN_RUNS_FOR_WARNING: usize = 10;
/// Coefficient of variation above which the frequency is called unstable.
pub const CV_THRESHOLD: f64 = 0.05;
/// A fitted decline of at least this fraction (and `DRIFT_MIN_R`) is throttling.
pub const DRIFT_THRESHOLD: f64 = 0.05;
pub const DRIFT_MIN_R: f64 = 0.5;
/// The mean is called low when it is under this fraction of the CPU maximum.
pub const BELOW_MAX_FRACTION: f64 = 0.7;

/// Which time divides the `cycles` of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Basis {
    /// `cycles` (user + kernel) / `task-clock`
    TaskClock,
    /// `cycles:u` / user CPU time (coarser, tick resolution possible)
    UserTime,
    /// `cycles:k` / system CPU time
    SystemTime,
}

impl Basis {
    pub fn as_str(self) -> &'static str {
        match self {
            Basis::TaskClock => "task_clock",
            Basis::UserTime => "user_time",
            Basis::SystemTime => "system_time",
        }
    }
}

/// Picks the time source for the scope `cycles` was counted in.
/// `all_scope`: user + kernel. `None`: the frequency is not available
/// (user + kernel cycles without `task-clock`).
pub fn choose_basis(all_scope: bool, task_clock_available: bool) -> Option<Basis> {
    match (all_scope, task_clock_available) {
        (true, true) => Some(Basis::TaskClock),
        (true, false) => None,
        (false, _) => Some(Basis::UserTime),
    }
}

/// Frequency of one run and the time it was computed over.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RunFrequency {
    pub hz: f64,
    pub seconds: f64,
}

impl RunFrequency {
    /// Long enough to enter the mean and the warnings.
    pub fn is_eligible(&self) -> bool {
        self.seconds >= MIN_RUN_SECONDS
    }
}

/// Frequency of one run. `None` when `cycles` or the time source is missing or
/// the time is zero (no frequency is ever invented).
pub fn run_frequency(
    basis: Basis,
    cycles: Option<u64>,
    task_clock_ns: Option<u64>,
    user_seconds: f64,
    system_seconds: f64,
) -> Option<RunFrequency> {
    let cycles = cycles?;
    let seconds = match basis {
        Basis::TaskClock => task_clock_ns? as f64 / 1e9,
        Basis::UserTime => user_seconds,
        Basis::SystemTime => system_seconds,
    };
    (seconds > 0.0 && seconds.is_finite()).then(|| RunFrequency {
        hz: cycles as f64 / seconds,
        seconds,
    })
}

/// `3.42 GHz`, or `850 MHz` below 1 GHz.
pub fn format_hz(hz: f64) -> String {
    if hz >= 1e9 {
        format!("{:.2} GHz", hz / 1e9)
    } else {
        format!("{:.0} MHz", hz / 1e6)
    }
}

/// Ordinary least squares of `y` on the index `0..n`: the fitted values at the
/// first and the last index and the Pearson correlation. `None` for fewer than
/// 3 points or a constant series (no correlation).
pub fn linear_fit(y: &[f64]) -> Option<(f64, f64, f64)> {
    let n = y.len();
    if n < 3 {
        return None;
    }
    let nf = n as f64;
    let mx = (nf - 1.0) / 2.0;
    let my = basic::mean(y);
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for (i, &v) in y.iter().enumerate() {
        let dx = i as f64 - mx;
        let dy = v - my;
        sxy += dx * dy;
        sxx += dx * dx;
        syy += dy * dy;
    }
    if sxx == 0.0 || syy == 0.0 {
        return None;
    }
    let slope = sxy / sxx;
    let first = my - slope * mx;
    let last = first + slope * (nf - 1.0);
    Some((first, last, sxy / (sxx * syy).sqrt()))
}

/// Warnings for the per-run frequencies (Hz) of the eligible runs, in run
/// order. Empty with fewer than [`MIN_RUNS_FOR_WARNING`] runs.
/// `max_hz`: `cpuinfo_max_freq` of the CPUs the command may run on.
pub fn warnings(freqs: &[f64], max_hz: Option<f64>) -> Vec<String> {
    let n = freqs.len();
    if n < MIN_RUNS_FOR_WARNING {
        return vec![];
    }
    let mean = basic::mean(freqs);
    if mean <= 0.0 || mean.is_nan() {
        return vec![];
    }
    let mut out = Vec::new();
    let mut tip = false;
    let cv = basic::standard_deviation(freqs, Some(mean)) / mean;
    if cv > CV_THRESHOLD {
        out.push(format!(
            "CPU frequency varied by {:.1}% (CV) across {n} runs (mean {}).",
            cv * 100.0,
            format_hz(mean)
        ));
        tip = true;
    }
    if let Some((first, last, r)) = linear_fit(freqs) {
        if first > 0.0 && (last - first) / first <= -DRIFT_THRESHOLD && r <= -DRIFT_MIN_R {
            out.push(format!(
                "CPU frequency fell from {} (run 1) to {} (run {n}): likely thermal throttling.",
                format_hz(first),
                format_hz(last)
            ));
            tip = true;
        }
    }
    if let Some(max) = max_hz.filter(|&m| m > 0.0) {
        if mean < BELOW_MAX_FRACTION * max {
            out.push(format!(
                "mean CPU frequency {} is {:.0}% of the CPU maximum ({}): the CPU may be throttled \
                 or in a power-saving state.",
                format_hz(mean),
                mean / max * 100.0,
                format_hz(max)
            ));
            tip = true;
        }
    }
    if tip {
        out.push(
            "Tip: let the CPU cool down between runs, or fix the frequency:\n       \
             sudo cpupower frequency-set -g performance"
                .to_string(),
        );
    }
    out
}

/// Highest `cpuinfo_max_freq` (kHz in sysfs, returned in Hz) of the given CPUs
/// under `root` (`/sys/devices/system/cpu`), or of all CPUs with `cpus = None`.
pub fn max_frequency_hz_in(root: &Path, cpus: Option<&[usize]>) -> Option<f64> {
    let mut best: Option<f64> = None;
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let name = entry.file_name();
        let Some(index) = name
            .to_str()
            .and_then(|n| n.strip_prefix("cpu"))
            .and_then(|n| n.parse::<usize>().ok())
        else {
            continue;
        };
        if cpus.is_some_and(|c| !c.contains(&index)) {
            continue;
        }
        let khz = std::fs::read_to_string(entry.path().join("cpufreq/cpuinfo_max_freq"))
            .ok()
            .and_then(|s| s.trim().parse::<f64>().ok())
            .filter(|&k| k > 0.0);
        if let Some(khz) = khz {
            best = Some(best.map_or(khz * 1e3, |b| b.max(khz * 1e3)));
        }
    }
    best
}

/// [`max_frequency_hz_in`] on the real sysfs (Linux only, `None` elsewhere).
pub fn max_frequency_hz(cpus: Option<&[usize]>) -> Option<f64> {
    if cfg!(target_os = "linux") {
        max_frequency_hz_in(Path::new("/sys/devices/system/cpu"), cpus)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basis_follows_the_scope() {
        assert_eq!(choose_basis(true, true), Some(Basis::TaskClock));
        assert_eq!(choose_basis(true, false), None);
        assert_eq!(choose_basis(false, true), Some(Basis::UserTime));
        assert_eq!(choose_basis(false, false), Some(Basis::UserTime));
        assert_eq!(Basis::TaskClock.as_str(), "task_clock");
        assert_eq!(Basis::UserTime.as_str(), "user_time");
        assert_eq!(Basis::SystemTime.as_str(), "system_time");
    }

    #[test]
    fn quotient_per_basis() {
        let f = run_frequency(
            Basis::TaskClock,
            Some(3_400_000_000),
            Some(1_000_000_000),
            9.0,
            9.0,
        )
        .unwrap();
        assert_eq!(f.hz, 3.4e9);
        assert!(f.is_eligible());
        // user-only cycles use the user time, never the task clock
        let f = run_frequency(Basis::UserTime, Some(2_000_000_000), Some(5), 0.5, 0.25).unwrap();
        assert_eq!(f.hz, 4e9);
        let f = run_frequency(Basis::SystemTime, Some(1_000_000_000), None, 0.5, 0.25).unwrap();
        assert_eq!(f.hz, 4e9);
    }

    #[test]
    fn missing_inputs_give_no_frequency() {
        assert!(run_frequency(Basis::TaskClock, None, Some(1), 0.0, 0.0).is_none());
        assert!(run_frequency(Basis::TaskClock, Some(1), None, 1.0, 1.0).is_none());
        assert!(run_frequency(Basis::TaskClock, Some(1), Some(0), 1.0, 1.0).is_none());
        assert!(run_frequency(Basis::UserTime, Some(1), None, 0.0, 1.0).is_none());
    }

    #[test]
    fn short_runs_are_not_eligible() {
        let f = run_frequency(
            Basis::TaskClock,
            Some(10_000_000),
            Some(4_000_000),
            0.0,
            0.0,
        )
        .unwrap();
        assert!(!f.is_eligible());
        let f = run_frequency(
            Basis::TaskClock,
            Some(10_000_000),
            Some(5_000_000),
            0.0,
            0.0,
        )
        .unwrap();
        assert!(f.is_eligible());
    }

    #[test]
    fn formats_ghz_and_mhz() {
        assert_eq!(format_hz(3.421e9), "3.42 GHz");
        assert_eq!(format_hz(1e9), "1.00 GHz");
        assert_eq!(format_hz(8.5e8), "850 MHz");
    }

    #[test]
    fn fit_of_a_line() {
        let y: Vec<f64> = (0..10).map(|i| 10.0 - i as f64).collect();
        let (first, last, r) = linear_fit(&y).unwrap();
        assert!((first - 10.0).abs() < 1e-9 && (last - 1.0).abs() < 1e-9);
        assert!((r + 1.0).abs() < 1e-9);
        assert!(linear_fit(&[1.0, 2.0]).is_none());
        assert!(linear_fit(&[2.0; 5]).is_none());
    }

    fn flat(n: usize) -> Vec<f64> {
        vec![3.4e9; n]
    }

    #[test]
    fn flat_and_short_series_do_not_warn() {
        assert!(warnings(&flat(30), None).is_empty());
        // fewer than 10 runs: nothing, even for a wild series
        let wild: Vec<f64> = (0..9).map(|i| if i % 2 == 0 { 1e9 } else { 4e9 }).collect();
        assert!(warnings(&wild, None).is_empty());
        // 1% noise stays under the CV threshold
        let quiet: Vec<f64> = (0..30)
            .map(|i| 3.4e9 * (1.0 + if i % 2 == 0 { 0.01 } else { -0.01 }))
            .collect();
        assert!(warnings(&quiet, None).is_empty());
    }

    #[test]
    fn noisy_series_warns_about_variation_only() {
        let noisy: Vec<f64> = (0..30)
            .map(|i| if i % 2 == 0 { 3.0e9 } else { 3.8e9 })
            .collect();
        let w = warnings(&noisy, None);
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(
            w[0].starts_with("CPU frequency varied by 12.0% (CV) across 30 runs (mean 3.40 GHz)"),
            "{w:?}"
        );
        assert!(
            w[1].starts_with("Tip: ") && w[1].contains("cpupower frequency-set -g performance")
        );
        assert!(!w.iter().any(|m| m.contains("throttling")));
    }

    #[test]
    fn steady_decline_warns_about_throttling() {
        // 4.2 GHz falling to 3.1 GHz over 30 runs, little noise around the line
        let y: Vec<f64> = (0..30).map(|i| 4.2e9 - 1.1e9 * i as f64 / 29.0).collect();
        let w = warnings(&y, None);
        assert!(
            w.iter().any(|m| m.contains(
                "fell from 4.20 GHz (run 1) to 3.10 GHz (run 30): likely thermal throttling"
            )),
            "{w:?}"
        );
        assert!(w.last().unwrap().starts_with("Tip: "));
    }

    #[test]
    fn step_and_outlier_do_not_fake_a_drift() {
        // one slow run among 30: CV < 5%, correlation near 0
        let mut y = flat(30);
        y[12] = 2.9e9;
        let w = warnings(&y, None);
        assert!(w.is_empty(), "{w:?}");
        // a step is not a steady decline but it is variation (CV > 5%)
        let step: Vec<f64> = (0..30)
            .map(|i| if i < 15 { 4.0e9 } else { 3.0e9 })
            .collect();
        let w = warnings(&step, None);
        assert!(w[0].contains("varied"), "{w:?}");
    }

    #[test]
    fn low_mean_against_the_cpu_maximum() {
        let w = warnings(&flat(12), Some(5.0e9));
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(
            w[0].contains("3.40 GHz is 68% of the CPU maximum (5.00 GHz)"),
            "{w:?}"
        );
        assert!(warnings(&flat(12), Some(4.0e9)).is_empty());
        assert!(warnings(&flat(12), None).is_empty());
    }

    #[test]
    fn reads_the_maximum_from_a_sysfs_tree() {
        let dir = tempfile::tempdir().unwrap();
        for (cpu, khz) in [(0, "3000000"), (1, "4500000\n"), (2, "2000000")] {
            let d = dir.path().join(format!("cpu{cpu}/cpufreq"));
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("cpuinfo_max_freq"), khz).unwrap();
        }
        std::fs::create_dir_all(dir.path().join("cpufreq")).unwrap(); // not a cpu
        std::fs::create_dir_all(dir.path().join("cpu3")).unwrap(); // no cpufreq
        assert_eq!(max_frequency_hz_in(dir.path(), None), Some(4.5e9));
        assert_eq!(max_frequency_hz_in(dir.path(), Some(&[0, 2])), Some(3.0e9));
        assert_eq!(max_frequency_hz_in(dir.path(), Some(&[3])), None);
        assert_eq!(max_frequency_hz_in(&dir.path().join("missing"), None), None);
    }
}
