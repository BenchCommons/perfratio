//! Statistical settings of a run (#320): the estimator behind ranking and
//! ratios, the confidence and significance levels and the bootstrap
//! parameters.
//!
//! The settings are chosen once by the command line and installed with
//! [`install`] before benchmarking starts; [`current`] returns them (or the
//! defaults, which reproduce the historical output exactly). Functions that
//! need deterministic behavior in tests take a `&StatSettings` explicitly.

use std::str::FromStr;
use std::sync::OnceLock;

use crate::stats::basic;
use crate::stats::correction::{Correction, Family};
use crate::stats::summary::percentile;

/// Seed of the resampling RNG (fixed by default, for reproducible output)
pub const DEFAULT_SEED: u64 = 0x7065_7266_7261_7469; // "perfrati"
pub const DEFAULT_RESAMPLES: usize = 5000;
pub const DEFAULT_CONFIDENCE_LEVEL: f64 = 0.95;
pub const DEFAULT_SIGNIFICANCE_LEVEL: f64 = 0.05;

/// The central value used for ranking, relative speed, the delta table,
/// `--compare` and `--target-precision`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Estimator {
    #[default]
    Mean,
    Median,
    /// A percentile (0 < p < 100) with linear interpolation (numpy's default)
    Percentile(f64),
}

impl Estimator {
    pub fn is_mean(&self) -> bool {
        matches!(self, Estimator::Mean)
    }

    /// `mean`, `median` or `p95`, as given on the command line and exported.
    pub fn name(&self) -> String {
        match self {
            Estimator::Mean => "mean".to_string(),
            Estimator::Median => "median".to_string(),
            Estimator::Percentile(p) => format!("p{p}"),
        }
    }

    /// The estimate for `values`; NaN for an empty sample.
    pub fn estimate(&self, values: &[f64]) -> f64 {
        if values.is_empty() {
            return f64::NAN;
        }
        match self {
            Estimator::Mean => basic::mean(values),
            Estimator::Median => basic::median(values),
            Estimator::Percentile(p) => {
                let mut sorted = values.to_vec();
                sorted.sort_by(f64::total_cmp);
                percentile(&sorted, *p).unwrap_or(f64::NAN)
            }
        }
    }
}

impl FromStr for Estimator {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let bad = || {
            format!(
                "invalid estimator '{s}': expected 'mean', 'median' or a percentile such as 'p95'"
            )
        };
        match s.trim() {
            "mean" => Ok(Estimator::Mean),
            "median" => Ok(Estimator::Median),
            other => {
                let p = other
                    .strip_prefix('p')
                    .and_then(|n| n.parse::<f64>().ok())
                    .filter(|p| p.is_finite() && *p > 0.0 && *p < 100.0)
                    .ok_or_else(bad)?;
                Ok(Estimator::Percentile(p))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StatSettings {
    pub estimator: Estimator,
    /// Confidence level of intervals, in (0, 1)
    pub confidence_level: f64,
    /// Alpha of hypothesis tests and significance markers, in (0, 1)
    pub significance_level: f64,
    pub bootstrap_resamples: usize,
    pub seed: u64,
    /// Multiple-comparison correction of the p-values (#321)
    pub correction: Correction,
    /// The comparisons the correction is applied to
    pub family: Family,
    /// Relative changes below this are within noise whatever the p-value (0.05 = 5%) (#323)
    pub noise_threshold: Option<f64>,
    /// Relative margin of the equivalence test, TOST (0.05 = 5%) (#323)
    pub equivalence_margin: Option<f64>,
    /// `--ci-method`: flat or hierarchical interval of the mean (#368)
    pub ci_method: crate::stats::hierarchical::CiMethod,
    /// `--paired`: paired analysis over the rounds of an interleaved schedule (#368)
    pub paired: bool,
}

impl Default for StatSettings {
    fn default() -> Self {
        StatSettings {
            estimator: Estimator::Mean,
            confidence_level: DEFAULT_CONFIDENCE_LEVEL,
            significance_level: DEFAULT_SIGNIFICANCE_LEVEL,
            bootstrap_resamples: DEFAULT_RESAMPLES,
            seed: DEFAULT_SEED,
            correction: Correction::None,
            family: Family::Reference,
            noise_threshold: None,
            equivalence_margin: None,
            ci_method: Default::default(),
            paired: false,
        }
    }
}

impl StatSettings {
    /// Fail early with a clear message for out-of-range values.
    pub fn validate(&self) -> Result<(), String> {
        let open_unit = |name: &str, v: f64| {
            if v.is_finite() && v > 0.0 && v < 1.0 {
                Ok(())
            } else {
                Err(format!(
                    "invalid {name} {v}: expected a number strictly between 0 and 1"
                ))
            }
        };
        open_unit("--confidence-level", self.confidence_level)?;
        open_unit("--significance-level", self.significance_level)?;
        if self.bootstrap_resamples < 100 {
            return Err(format!(
                "invalid --bootstrap-resamples {}: expected at least 100",
                self.bootstrap_resamples
            ));
        }
        if let Some(v) = self.noise_threshold {
            if !(v.is_finite() && v >= 0.0) {
                return Err(format!(
                    "invalid --noise-threshold {}%: expected a percentage >= 0",
                    v * 100.0
                ));
            }
        }
        if let Some(v) = self.equivalence_margin {
            if !(v.is_finite() && v > 0.0 && v < 1.0) {
                return Err(format!(
                    "invalid --equivalence-margin {}%: expected a percentage greater than 0 and below 100",
                    v * 100.0
                ));
            }
        }
        Ok(())
    }

    /// Whether the settings are the defaults (output must then be unchanged).
    pub fn is_default(&self) -> bool {
        *self == StatSettings::default()
    }

    /// The confidence level as the percentage shown in labels: `95` or `99.9`.
    pub fn confidence_percent(&self) -> String {
        // avoid 94.99999999999999 from 0.95 * 100
        let pct = (self.confidence_level * 100.0 * 1e9).round() / 1e9;
        format!("{pct}")
    }
}

static CURRENT: OnceLock<StatSettings> = OnceLock::new();

/// Install the settings of this process. Returns `false` if settings were
/// already installed (the first installation wins).
pub fn install(settings: StatSettings) -> bool {
    CURRENT.set(settings).is_ok()
}

/// The installed settings, or the defaults.
pub fn current() -> StatSettings {
    CURRENT.get().cloned().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    #[test]
    fn parses_estimators() {
        assert_eq!("mean".parse(), Ok(Estimator::Mean));
        assert_eq!("median".parse(), Ok(Estimator::Median));
        assert_eq!("p95".parse(), Ok(Estimator::Percentile(95.0)));
        assert_eq!("p99.9".parse(), Ok(Estimator::Percentile(99.9)));
        for bad in ["", "p", "p0", "p100", "p-5", "pxx", "mode", "95"] {
            assert!(bad.parse::<Estimator>().is_err(), "{bad}");
        }
        assert_eq!(Estimator::Percentile(95.0).name(), "p95");
        assert_eq!(Estimator::Percentile(99.9).name(), "p99.9");
    }

    // numpy 2.x on [100, 1, 2, 3, 4, 10]: mean 20.0, median 3.5,
    // percentile(75) 8.5, percentile(95) 77.5 (linear interpolation).
    #[test]
    fn estimates_match_numpy() {
        let xs = [100.0, 1.0, 2.0, 3.0, 4.0, 10.0];
        assert_relative_eq!(Estimator::Mean.estimate(&xs), 20.0);
        assert_relative_eq!(Estimator::Median.estimate(&xs), 3.5);
        assert_relative_eq!(Estimator::Percentile(75.0).estimate(&xs), 8.5);
        assert_relative_eq!(Estimator::Percentile(95.0).estimate(&xs), 77.5);
        assert!(Estimator::Mean.estimate(&[]).is_nan());
    }

    #[test]
    fn validation_rejects_out_of_range_values() {
        let ok = StatSettings::default();
        assert!(ok.validate().is_ok());
        for bad in [0.0, 1.0, -0.1, 1.5, f64::NAN] {
            let s = StatSettings {
                confidence_level: bad,
                ..ok.clone()
            };
            assert!(s.validate().unwrap_err().contains("--confidence-level"));
            let s = StatSettings {
                significance_level: bad,
                ..ok.clone()
            };
            assert!(s.validate().unwrap_err().contains("--significance-level"));
        }
        let s = StatSettings {
            bootstrap_resamples: 10,
            ..ok
        };
        assert!(s.validate().unwrap_err().contains("--bootstrap-resamples"));
    }

    #[test]
    fn confidence_percent_is_clean() {
        assert_eq!(StatSettings::default().confidence_percent(), "95");
        let s = StatSettings {
            confidence_level: 0.999,
            ..Default::default()
        };
        assert_eq!(s.confidence_percent(), "99.9");
    }
}
