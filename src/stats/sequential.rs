//! Sequentially valid stopping for `--target-precision` (#322,
//! `docs/specs/sequential-stopping.md`).
//!
//! The fixed rule re-checks a 95% Student-t interval after every run and stops
//! the first time it is narrow enough. That interval is valid for one,
//! pre-chosen sample size; picking the moment to stop from the data ("peeking")
//! makes the real error rate higher than 5%. A *confidence sequence* is an
//! interval for the mean that holds for all sample sizes at once with
//! probability at least 1 - alpha, so it stays valid whatever the stopping
//! rule is, including "stop when it is narrow enough".
//!
//! The boundary is the normal-mixture confidence sequence of Robbins (1970),
//! in the form of Howard, Ramdas, McAuliffe and Sekhon (2021, "Time-uniform,
//! nonparametric, nonasymptotic confidence sequences", Annals of Statistics)
//! and, with the sample standard deviation plugged in, the asymptotic
//! confidence sequence of Waudby-Smith, Arbour, Sinha, Kennedy and Ramdas
//! (2021, "Time-uniform central limit theory and asymptotic confidence
//! sequences"):
//!
//! ```text
//! mean_n ± s_n * sqrt( 2 (n rho² + 1) / (n² rho²) * ln( sqrt(n rho² + 1) / alpha ) )
//! ```
//!
//! Assumptions: independent runs with finite variance. With the plugged-in
//! `s_n` the guarantee is asymptotic: it is exact for known variance and
//! normal data, and approximate for small `n`, heavy tails and strongly
//! skewed data (see the coverage study in the spec). Dependent runs
//! (autocorrelation, drift) break it, as they break the fixed interval.

/// How `--target-precision` decides that the interval is narrow enough.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoppingRule {
    /// Student-t interval of the mean, re-checked after every run (default)
    #[default]
    FixedCi,
    /// Anytime-valid confidence sequence of the mean
    ConfidenceSequence,
}

impl StoppingRule {
    /// Value of `perfratio.results[].precision.stopping_rule`
    pub fn name(self) -> &'static str {
        match self {
            StoppingRule::FixedCi => "fixed_ci",
            StoppingRule::ConfidenceSequence => "confidence_sequence",
        }
    }
}

/// The run count at which the mixing parameter is tuned (the boundary is the
/// narrowest there). A few hundred runs is where most `--target-precision`
/// stops happen; the spec reports the cost of other choices.
pub const TUNED_AT_RUNS: f64 = 100.0;

/// `rho²` for a boundary that is narrowest at `n0` runs: the solution `x` of
/// `ln((x + 1) / alpha²) = x`, divided by `n0` (Howard et al. 2021, eq. 9).
pub fn rho_squared(alpha: f64, n0: f64) -> f64 {
    let mut x = 1.0;
    for _ in 0..100 {
        let next = ((x + 1.0) / (alpha * alpha)).ln();
        if (next - x).abs() < 1e-13 {
            x = next;
            break;
        }
        x = next;
    }
    x / n0
}

/// Half-width of the confidence sequence of the mean after `n` runs with
/// sample standard deviation `sd`, at error probability `alpha`.
pub fn half_width(n: usize, sd: f64, alpha: f64, rho2: f64) -> f64 {
    if n < 2 {
        return f64::INFINITY;
    }
    let n = n as f64;
    let inside = 2.0 * (n * rho2 + 1.0) / (n * n * rho2) * ((n * rho2 + 1.0).sqrt() / alpha).ln();
    sd * inside.sqrt()
}

/// Half-width of the confidence sequence of the mean of `xs`, relative to the
/// mean (0.01 = ±1%) at `confidence` (0.95: alpha = 0.05). `None` for fewer
/// than 2 values or a mean of 0 or less.
pub fn relative_half_width(xs: &[f64], confidence: f64) -> Option<f64> {
    let n = xs.len();
    if n < 2 {
        return None;
    }
    let mean = xs.iter().sum::<f64>() / n as f64;
    if mean.is_nan() || mean <= 0.0 {
        return None;
    }
    if xs.iter().all(|&x| x == xs[0]) {
        return Some(0.0);
    }
    let variance = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1) as f64;
    let alpha = 1.0 - confidence;
    let rho2 = rho_squared(alpha, TUNED_AT_RUNS);
    Some(half_width(n, variance.sqrt(), alpha, rho2) / mean)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rho_solves_its_equation() {
        let alpha = 0.05;
        let x = rho_squared(alpha, 1.0);
        assert!(((x + 1.0) / (alpha * alpha)).ln() - x < 1e-9);
        assert!((x - 8.2).abs() < 0.1, "{x}");
        assert!((rho_squared(alpha, 100.0) - x / 100.0).abs() < 1e-15);
    }

    #[test]
    fn boundary_is_wider_than_the_fixed_interval_and_shrinks() {
        let alpha = 0.05;
        let rho2 = rho_squared(alpha, TUNED_AT_RUNS);
        // reference values of the formula, sd = 1
        let at = |n| half_width(n, 1.0, alpha, rho2);
        assert!(at(10) > at(100) && at(100) > at(1000) && at(1000) > at(10000));
        for n in [10usize, 50, 100, 400, 2000] {
            let fixed = crate::stats::precision::t_critical(0.95, n - 1) / (n as f64).sqrt();
            assert!(at(n) > fixed, "n = {n}");
        }
        // at the tuned run count the boundary is within 1.7x of the fixed interval
        let ratio = at(100) / (crate::stats::precision::t_critical(0.95, 99) / 10.0);
        assert!(ratio > 1.0 && ratio < 1.7, "{ratio}");
        // value computed by hand: n = 100, rho2 = x / 100, x = 8.2..
        let x = rho_squared(alpha, 1.0);
        let manual = (2.0 * (x + 1.0) / (100.0 * x) * ((x + 1.0).sqrt() / alpha).ln()).sqrt();
        assert!((at(100) - manual).abs() < 1e-12);
    }

    #[test]
    fn boundary_minimum_is_at_the_tuned_run_count() {
        let alpha = 0.05;
        let rho2 = rho_squared(alpha, 100.0);
        // the standardized width n * hw(n)^2 / ln(...) is flat at the optimum: compare
        // the width relative to the sqrt(n) rate
        let scaled = |n: usize| half_width(n, 1.0, alpha, rho2) * (n as f64).sqrt();
        assert!(scaled(100) < scaled(10));
        assert!(scaled(100) < scaled(1000));
    }

    #[test]
    fn relative_width_edge_cases() {
        assert_eq!(relative_half_width(&[1.0], 0.95), None);
        assert_eq!(relative_half_width(&[0.0, 0.0, 0.0], 0.95), None);
        assert_eq!(relative_half_width(&[2.0; 5], 0.95), Some(0.0));
        let w = relative_half_width(&[9.0, 11.0, 9.0, 11.0], 0.95).unwrap();
        let fixed =
            crate::stats::precision::relative_ci_half_width(&[9.0, 11.0, 9.0, 11.0]).unwrap();
        assert!(w > fixed);
        // a higher confidence needs a wider sequence
        assert!(relative_half_width(&[9.0, 11.0, 9.0, 11.0], 0.99).unwrap() > w);
    }
}
