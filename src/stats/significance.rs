//! Statistical significance of a difference between two benchmarks (#215).
//!
//! A difference counts as significant only if the bootstrap Welch test
//! (`crate::compare::p_value`, the one behind `--deep-stats` and `--compare`)
//! gives `p < 0.05` **and** the change in mean is at least 1%. Everything else
//! is noise.

use serde::Serialize;

use crate::benchmark::benchmark_result::BenchmarkResult;

/// Smallest relative change of the mean that is worth reporting (1%).
pub const MIN_EFFECT: f64 = 0.01;
/// Significance level of the test.
pub const MAX_P_VALUE: f64 = 0.05;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Significance {
    SignificantSpeedup,
    SignificantRegression,
    Noise,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Assessment {
    pub significance: Significance,
    pub p_value: f64,
}

/// Classify a change of the mean (`rel_change` = other / reference − 1) with test result `p_value`.
pub fn classify(rel_change: f64, p_value: f64) -> Significance {
    // NaN comparisons are false, so undefined input falls through to `Noise`.
    let significant = p_value < MAX_P_VALUE && rel_change.abs() >= MIN_EFFECT;
    if !significant {
        Significance::Noise
    } else if rel_change < 0.0 {
        Significance::SignificantSpeedup
    } else {
        Significance::SignificantRegression
    }
}

/// Assess `other` against `reference`. `None` if either has no per-run times,
/// fewer than three runs, or the test is undefined.
pub fn assess(reference: &BenchmarkResult, other: &BenchmarkResult) -> Option<Assessment> {
    if reference.timed_out || other.timed_out || reference.mean <= 0.0 {
        return None;
    }
    let p_value = crate::compare::p_value(reference, other)?;
    Some(Assessment {
        significance: classify(other.mean / reference.mean - 1.0, p_value),
        p_value,
    })
}

fn format_p(p: f64) -> String {
    if p < 0.001 {
        "p < 0.001".to_string()
    } else {
        format!("p = {p:.3}")
    }
}

impl Assessment {
    /// Marker and label for the terminal, e.g. `[+] significant speedup (p < 0.001)`.
    /// `fancy` selects symbols over the plain-text markers `[+]`, `[-]`, `[~]`
    /// (used for `NO_COLOR`, non-TTY output and dumb terminals).
    pub fn label(&self, fancy: bool) -> String {
        let (marker, text) = match (self.significance, fancy) {
            (Significance::SignificantSpeedup, true) => ("⚡", "significant speedup"),
            (Significance::SignificantSpeedup, false) => ("[+]", "significant speedup"),
            (Significance::SignificantRegression, true) => ("🔻", "significant regression"),
            (Significance::SignificantRegression, false) => ("[-]", "significant regression"),
            (Significance::Noise, true) => ("~", "indistinguishable from noise"),
            (Significance::Noise, false) => ("[~]", "indistinguishable from noise"),
        };
        format!("{marker} {text} ({})", format_p(self.p_value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_speedup() {
        assert_eq!(classify(-0.3, 0.0001), Significance::SignificantSpeedup);
    }

    #[test]
    fn clear_regression() {
        assert_eq!(classify(0.15, 0.0001), Significance::SignificantRegression);
    }

    #[test]
    fn small_effect_is_noise_even_if_p_is_tiny() {
        assert_eq!(classify(0.005, 0.0001), Significance::Noise);
        assert_eq!(classify(-0.005, 0.0001), Significance::Noise);
    }

    #[test]
    fn large_effect_with_high_p_is_noise() {
        assert_eq!(classify(0.2, 0.42), Significance::Noise);
        assert_eq!(classify(0.2, 0.05), Significance::Noise);
        assert_eq!(classify(0.2, f64::NAN), Significance::Noise);
    }

    #[test]
    fn effect_boundary_is_inclusive() {
        assert_eq!(classify(0.01, 0.001), Significance::SignificantRegression);
        assert_eq!(classify(-0.01, 0.001), Significance::SignificantSpeedup);
    }

    #[test]
    fn labels_use_plain_markers_without_fancy_output() {
        let a = |significance, p_value| Assessment {
            significance,
            p_value,
        };
        assert_eq!(
            a(Significance::SignificantSpeedup, 0.0).label(false),
            "[+] significant speedup (p < 0.001)"
        );
        assert_eq!(
            a(Significance::SignificantRegression, 0.0).label(false),
            "[-] significant regression (p < 0.001)"
        );
        assert_eq!(
            a(Significance::Noise, 0.4213).label(false),
            "[~] indistinguishable from noise (p = 0.421)"
        );
        assert!(a(Significance::SignificantSpeedup, 0.0)
            .label(true)
            .starts_with('⚡'));
    }

    #[test]
    fn json_names() {
        assert_eq!(
            serde_json::to_string(&Significance::SignificantSpeedup).unwrap(),
            "\"significant_speedup\""
        );
        assert_eq!(
            serde_json::to_string(&Significance::SignificantRegression).unwrap(),
            "\"significant_regression\""
        );
        assert_eq!(
            serde_json::to_string(&Significance::Noise).unwrap(),
            "\"noise\""
        );
    }
}
