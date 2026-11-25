//! A module for statistical outlier detection.
//!
//! References:
//! - Boris Iglewicz and David Hoaglin (1993), "Volume 16: How to Detect and Handle Outliers",
//!   The ASQC Basic References in Quality Control: Statistical Techniques, Edward F. Mykytka,
//!   Ph.D., Editor.
//! - John W. Tukey (1977), "Exploratory Data Analysis" (Tukey's fences).

use serde::{Deserialize, Serialize};

use crate::stats::basic::median;
use crate::stats::summary::percentile;

/// Minimum modified Z-score for a datapoint to be an outlier. Here, 1.4826 is a factor that
/// converts the MAD to an estimator for the standard deviation. The second factor is the number
/// of standard deviations.
pub const OUTLIER_THRESHOLD: f64 = 1.4826 * 10.0;

/// Compute modified Z-scores for a given sample. A (unmodified) Z-score is defined by
/// `(x_i - x_mean)/x_stddev` whereas the modified Z-score is defined by `(x_i - x_median)/MAD`
/// where MAD is the median absolute deviation.
///
/// References:
/// - <https://en.wikipedia.org/wiki/Median_absolute_deviation>
pub fn modified_zscores(xs: &[f64]) -> Vec<f64> {
    assert!(!xs.is_empty());

    // Compute sample median:
    let x_median = median(xs);

    // Compute the absolute deviations from the median:
    let deviations: Vec<f64> = xs.iter().map(|x| (x - x_median).abs()).collect();

    // Compute median absolute deviation:
    let mad = median(&deviations);

    // Handle MAD == 0 case
    let mad = if mad > 0.0 { mad } else { f64::EPSILON };

    // Compute modified Z-scores (x_i - x_median) / MAD
    xs.iter().map(|&x| (x - x_median) / mad).collect()
}

/// Factor `k` of Tukey's fences: `[Q1 - k * IQR, Q3 + k * IQR]`.
pub const TUKEY_FENCE_FACTOR: f64 = 1.5;

/// Smallest sample for which quartiles (and thus fences) are meaningful.
pub const MIN_OUTLIER_SAMPLE: usize = 4;

/// Outlier share (in percent) from which the warning is considered severe.
pub const SEVERE_OUTLIER_PERCENT: f64 = 10.0;

/// How outliers are detected for the report and the warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum OutlierMethod {
    /// Tukey's fences (1.5 × IQR), opt-in
    #[serde(rename = "iqr")]
    Iqr,
    /// Modified Z-score (median and MAD), the default
    #[default]
    #[serde(rename = "modified-z")]
    ModifiedZ,
}

impl OutlierMethod {
    pub fn name(self) -> &'static str {
        match self {
            OutlierMethod::Iqr => "iqr",
            OutlierMethod::ModifiedZ => "modified-z",
        }
    }
}

/// Number and share of outliers of one measured series, with the bounds
/// outside of which a value counts as an outlier.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OutlierStats {
    pub count: usize,
    /// `count` as a percentage of the sample size (0–100)
    pub percent: f64,
    pub lower_fence: f64,
    pub upper_fence: f64,
}

impl OutlierStats {
    pub fn is_severe(&self) -> bool {
        self.percent >= SEVERE_OUTLIER_PERCENT
    }
}

/// Outlier statistics for the exported result: the wall-clock time (flattened,
/// in seconds) and, if measured, memory (bytes) and energy (Joules).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutlierReport {
    pub method: OutlierMethod,
    #[serde(flatten)]
    pub time: OutlierStats,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_usage_byte: Option<OutlierStats>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub energy_joules: Option<OutlierStats>,
}

/// Outliers of `xs` outside Tukey's fences, with Q1 and Q3 by linear
/// interpolation. None for fewer than `MIN_OUTLIER_SAMPLE` finite values. With
/// IQR = 0, any value different from the quartile is an outlier.
pub fn tukey_outliers(xs: &[f64]) -> Option<OutlierStats> {
    if xs.len() < MIN_OUTLIER_SAMPLE || xs.iter().any(|x| !x.is_finite()) {
        return None;
    }
    let mut sorted = xs.to_vec();
    sorted.sort_by(f64::total_cmp);
    let q1 = percentile(&sorted, 25.0)?;
    let q3 = percentile(&sorted, 75.0)?;
    let iqr = q3 - q1;
    Some(count_outside(
        xs,
        q1 - TUKEY_FENCE_FACTOR * iqr,
        q3 + TUKEY_FENCE_FACTOR * iqr,
    ))
}

/// Outliers of `xs` by modified Z-score above `OUTLIER_THRESHOLD`, reported as
/// the equivalent interval `median ± OUTLIER_THRESHOLD * MAD`.
pub fn modified_z_outliers(xs: &[f64]) -> Option<OutlierStats> {
    if xs.len() < MIN_OUTLIER_SAMPLE || xs.iter().any(|x| !x.is_finite()) {
        return None;
    }
    let x_median = median(xs);
    let deviations: Vec<f64> = xs.iter().map(|x| (x - x_median).abs()).collect();
    let mad = median(&deviations);
    let mad = if mad > 0.0 { mad } else { f64::EPSILON };
    let half_width = OUTLIER_THRESHOLD * mad;
    Some(count_outside(
        xs,
        x_median - half_width,
        x_median + half_width,
    ))
}

/// Outlier statistics of `xs` by the chosen method.
pub fn detect_outliers(xs: &[f64], method: OutlierMethod) -> Option<OutlierStats> {
    match method {
        OutlierMethod::Iqr => tukey_outliers(xs),
        OutlierMethod::ModifiedZ => modified_z_outliers(xs),
    }
}

fn count_outside(xs: &[f64], lower_fence: f64, upper_fence: f64) -> OutlierStats {
    let count = xs
        .iter()
        .filter(|&&x| x < lower_fence || x > upper_fence)
        .count();
    OutlierStats {
        count,
        percent: count as f64 / xs.len() as f64 * 100.0,
        lower_fence,
        upper_fence,
    }
}

/// Largest fraction of runs that `--discard-outliers` may remove. If more runs
/// look like outliers, the distribution is probably multimodal, and silently
/// dropping them would hide that.
pub const MAX_DISCARD_FRACTION: f64 = 0.05;

/// Indices of the data points whose modified Z-score exceeds `threshold`, or
/// None if there are more of them than allowed: `max_fraction` of all points,
/// but at least one (otherwise a single outlier could never be discarded with
/// the default of 10 runs). Samples with fewer than 5 points never have
/// outliers.
pub fn outlier_indices(xs: &[f64], threshold: f64, max_fraction: f64) -> Option<Vec<usize>> {
    if xs.len() < 5 {
        return Some(vec![]);
    }
    let indices: Vec<usize> = modified_zscores(xs)
        .iter()
        .enumerate()
        .filter(|(_, z)| z.abs() > threshold)
        .map(|(i, _)| i)
        .collect();
    let allowed = ((max_fraction * xs.len() as f64).floor() as usize).max(1);
    (indices.len() <= allowed).then_some(indices)
}

/// Return the number of outliers in a given sample. Outliers are defined as data points with a
/// modified Z-score that is larger than `OUTLIER_THRESHOLD`.
#[cfg(test)]
pub fn num_outliers(xs: &[f64]) -> usize {
    if xs.is_empty() {
        return 0;
    }

    let scores = modified_zscores(xs);
    scores
        .iter()
        .filter(|&&s| s.abs() > OUTLIER_THRESHOLD)
        .count()
}

#[test]
fn test_detect_outliers() {
    // Should not detect outliers in small samples
    assert_eq!(0, num_outliers(&[]));
    assert_eq!(0, num_outliers(&[50.0]));
    assert_eq!(0, num_outliers(&[1000.0, 0.0]));

    // Should not detect outliers in low-variance samples
    let xs = [-0.2, 0.0, 0.2];
    assert_eq!(0, num_outliers(&xs));

    // Should detect a single outlier
    let xs = [-0.2, 0.0, 0.2, 4.0];
    assert_eq!(1, num_outliers(&xs));

    // Should detect a single outlier
    let xs = [0.5, 0.30, 0.29, 0.31, 0.30];
    assert_eq!(1, num_outliers(&xs));

    // Should detect no outliers in sample drawn from normal distribution
    let xs = [
        2.33269488,
        1.42195907,
        -0.57527698,
        -0.31293437,
        2.2948158,
        0.75813273,
        -1.0712388,
        -0.96394741,
        -1.15897446,
        1.10976285,
    ];
    assert_eq!(0, num_outliers(&xs));

    // Should detect two outliers that were manually added
    let xs = [
        2.33269488,
        1.42195907,
        -0.57527698,
        -0.31293437,
        2.2948158,
        0.75813273,
        -1.0712388,
        -0.96394741,
        -1.15897446,
        1.10976285,
        20.0,
        -500.0,
    ];
    assert_eq!(2, num_outliers(&xs));
}

#[test]
fn test_detect_outliers_if_mad_becomes_0() {
    // See https://stats.stackexchange.com/q/339932
    let xs = [10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 100.0];
    assert_eq!(1, num_outliers(&xs));

    let xs = [10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 100.0, 100.0];
    assert_eq!(2, num_outliers(&xs));
}

#[test]
fn test_outlier_indices() {
    let mut xs = vec![1.0; 19];
    xs.push(50.0);
    assert_eq!(
        outlier_indices(&xs, OUTLIER_THRESHOLD, 0.05),
        Some(vec![19])
    );

    // too few points to judge
    assert_eq!(
        outlier_indices(&[1.0, 1.0, 1.0, 50.0], OUTLIER_THRESHOLD, 0.05),
        Some(vec![])
    );

    // a single outlier is always allowed (5% of 10 runs would be 0)
    let mut ten = vec![1.0; 9];
    ten.push(50.0);
    assert_eq!(
        outlier_indices(&ten, OUTLIER_THRESHOLD, 0.05),
        Some(vec![9])
    );

    // 3 of 20 points (15%) exceed the cap of max(1, 5%): refuse to discard anything
    let mut bimodal = vec![1.0; 17];
    bimodal.extend([50.0, 51.0, 52.0]);
    assert_eq!(outlier_indices(&bimodal, OUTLIER_THRESHOLD, 0.05), None);

    // no outliers
    assert_eq!(
        outlier_indices(&[1.0, 1.1, 0.9, 1.0, 1.05], OUTLIER_THRESHOLD, 0.05),
        Some(vec![])
    );
}

#[cfg(test)]
mod tukey_tests {
    use super::*;

    #[test]
    fn tukey_reference_vector() {
        // numpy: Q1 = 3.25, Q3 = 7.75 -> IQR 4.5, fences [-3.5, 14.5]
        let xs = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 100.0];
        let s = tukey_outliers(&xs).unwrap();
        assert_eq!(s.count, 1);
        assert!((s.percent - 10.0).abs() < 1e-12);
        assert!((s.lower_fence - (-3.5)).abs() < 1e-12);
        assert!((s.upper_fence - 14.5).abs() < 1e-12);
        assert!(s.is_severe());
    }

    #[test]
    fn tukey_low_and_high_outliers() {
        let xs = [-50.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 90.0, 12.5];
        assert_eq!(tukey_outliers(&xs).unwrap().count, 2);
    }

    #[test]
    fn tukey_values_on_the_fence_are_not_outliers() {
        // Q1 = 1, Q3 = 3 (linear interpolation), fences [-2, 6]
        let xs = [1.0, 1.0, 3.0, 3.0, 6.0, -2.0];
        let s = tukey_outliers(&xs).unwrap();
        assert_eq!((s.lower_fence, s.upper_fence), (-2.0, 6.0));
        assert_eq!(s.count, 0);
    }

    #[test]
    fn tukey_needs_four_samples() {
        assert!(tukey_outliers(&[]).is_none());
        assert!(tukey_outliers(&[1.0, 2.0, 100.0]).is_none());
        assert!(tukey_outliers(&[1.0, 2.0, 3.0, 100.0]).is_some());
    }

    #[test]
    fn tukey_identical_samples_have_no_outliers() {
        let s = tukey_outliers(&[5.0; 20]).unwrap();
        assert_eq!(s.count, 0);
        assert_eq!(s.percent, 0.0);
        assert_eq!((s.lower_fence, s.upper_fence), (5.0, 5.0));
    }

    #[test]
    fn tukey_zero_iqr_flags_any_deviation() {
        let mut xs = vec![10.0; 9];
        xs.push(10.5);
        let s = tukey_outliers(&xs).unwrap();
        assert_eq!(s.count, 1);
        assert!((s.percent - 10.0).abs() < 1e-12);
    }

    #[test]
    fn tukey_rejects_non_finite_values() {
        assert!(tukey_outliers(&[1.0, 2.0, 3.0, f64::NAN]).is_none());
    }

    #[test]
    fn modified_z_matches_num_outliers() {
        let xs = [10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 100.0, 100.0];
        assert_eq!(modified_z_outliers(&xs).unwrap().count, num_outliers(&xs));
        assert!(modified_z_outliers(&[1.0, 2.0]).is_none());
    }

    #[test]
    fn method_names_serialize() {
        assert_eq!(
            serde_json::to_string(&OutlierMethod::Iqr).unwrap(),
            "\"iqr\""
        );
        assert_eq!(OutlierMethod::ModifiedZ.name(), "modified-z");
    }

    #[test]
    fn report_serializes_flat_and_skips_absent_metrics() {
        let report = OutlierReport {
            method: OutlierMethod::Iqr,
            time: tukey_outliers(&[1.0, 2.0, 3.0, 4.0, 100.0]).unwrap(),
            memory_usage_byte: None,
            energy_joules: None,
        };
        let v = serde_json::to_value(&report).unwrap();
        assert_eq!(v["method"], "iqr");
        assert_eq!(v["count"], 1);
        assert!(v.get("memory_usage_byte").is_none());
    }
}
