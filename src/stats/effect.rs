//! Effect size, noise threshold and equivalence testing (#323).
//!
//! For a comparison with the reference this module reports the relative
//! change with a confidence interval, standardized effect sizes, a two
//! one-sided test (TOST, Schuirmann 1987) against an equivalence margin, a
//! classification and the number of runs that would settle an inconclusive
//! result.
//!
//! For the mean the interval and the test use Welch's t distribution on the
//! difference of the means, expressed relative to the reference mean (the
//! uncertainty of the reference mean in the denominator is not propagated;
//! `--subtract` baseline variance is not included). For other estimators the
//! interval is a bootstrap interval of the difference of the estimates.

use serde::Serialize;

use crate::benchmark::benchmark_result::BenchmarkResult;
use crate::metric::Metric;
use crate::stats::basic;
use crate::stats::deep::difference_confidence_interval;
use crate::stats::dist::{t_cdf, t_critical};
use crate::stats::settings::StatSettings;

/// Power of the power calculations (probability of detecting an effect that exists)
pub const POWER: f64 = 0.8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    /// Significantly faster than the reference, beyond the noise threshold
    Faster,
    /// Significantly slower than the reference, beyond the noise threshold
    Slower,
    /// The change is within `--equivalence-margin` (TOST rejects a larger one)
    Equivalent,
    /// Neither a difference nor equivalence is established; more runs are needed
    Inconclusive,
    /// The change is below the noise threshold
    WithinNoise,
}

impl Classification {
    pub fn name(&self) -> &'static str {
        match self {
            Classification::Faster => "faster",
            Classification::Slower => "slower",
            Classification::Equivalent => "equivalent",
            Classification::Inconclusive => "inconclusive",
            Classification::WithinNoise => "within_noise",
        }
    }
}

/// Standardized effect sizes of the per-run times (positive: `other` is slower).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Standardized {
    /// Cohen's d: difference of the means over the pooled standard deviation
    pub cohens_d: f64,
    /// Common-language effect size: probability that a run of `other` is
    /// slower than a run of the reference (ties count half); Mann-Whitney U / (n m)
    pub common_language: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Effect {
    /// `other / reference - 1` of the estimator (0.05 = 5% slower)
    pub relative_change: f64,
    /// Interval of `relative_change` at `--confidence-level`
    pub ci_low: f64,
    pub ci_high: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub standardized: Option<Standardized>,
    /// Smallest relative change the test detects with 80% power at the
    /// current number of runs and variance
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum_detectable_effect: Option<f64>,
    /// Runs per command for the equivalence test to succeed with 80% power
    /// (only for an inconclusive result with `--equivalence-margin`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runs_needed: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Verdict {
    pub effect: Effect,
    pub classification: Classification,
    /// p-value of the TOST (the larger of the two one-sided p-values), with
    /// `--equivalence-margin` and the mean; the bootstrap of other estimators has none
    pub equivalence_p: Option<f64>,
}

/// Standard normal quantile (Acklam's rational approximation, relative error < 1.2e-9).
pub fn normal_quantile(p: f64) -> f64 {
    if !(p > 0.0 && p < 1.0) {
        return f64::NAN;
    }
    const A: [f64; 6] = [
        -3.969683028665376e+01,
        2.209460984245205e+02,
        -2.759285104469687e+02,
        1.38357751867269e+02,
        -3.066479806614716e+01,
        2.506628277459239e+00,
    ];
    const B: [f64; 5] = [
        -5.447609879822406e+01,
        1.615858368580409e+02,
        -1.556989798598866e+02,
        6.680131188771972e+01,
        -1.328068155288572e+01,
    ];
    const C: [f64; 6] = [
        -7.784894002430293e-03,
        -3.223964580411365e-01,
        -2.400758277161838e+00,
        -2.549732539343734e+00,
        4.374664141464968e+00,
        2.938163982698783e+00,
    ];
    const D: [f64; 4] = [
        7.784695709041462e-03,
        3.224671290700398e-01,
        2.445134137142996e+00,
        3.754408661907416e+00,
    ];
    const LOW: f64 = 0.02425;
    if p < LOW {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= 1.0 - LOW {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        -normal_quantile(1.0 - p)
    }
}

/// Runs per command for a TOST at level `alpha` to declare equivalence within
/// ±`margin` with the given `power`, when the true difference is `difference`
/// and the two run-time variances are `variance_sum = σ₁² + σ₂²` (normal
/// approximation, Chow, Shao and Wang 2008):
/// `n = (σ₁² + σ₂²) (z₁₋α + z₁₋β/₂)² / (margin - |difference|)²`.
/// `None` if `|difference| >= margin` (the commands are not equivalent).
pub fn required_runs(
    variance_sum: f64,
    margin: f64,
    difference: f64,
    alpha: f64,
    power: f64,
) -> Option<u64> {
    let room = margin - difference.abs();
    if room.is_nan() || room <= 0.0 || !variance_sum.is_finite() || variance_sum < 0.0 {
        return None;
    }
    let z = normal_quantile(1.0 - alpha) + normal_quantile(1.0 - (1.0 - power) / 2.0);
    let n = (variance_sum * z * z / (room * room)).ceil();
    Some((n as u64).max(2))
}

/// Smallest difference that a two-sided test at level `alpha` detects with
/// the given `power`, for a standard error `se` of the difference:
/// `(z₁₋α/₂ + z_power) se`.
pub fn minimum_detectable_effect(se: f64, alpha: f64, power: f64) -> f64 {
    (normal_quantile(1.0 - alpha / 2.0) + normal_quantile(power)) * se
}

/// Welch's comparison of two samples: difference of the means (`b - a`),
/// its standard error and the Welch-Satterthwaite degrees of freedom.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Welch {
    pub difference: f64,
    pub se: f64,
    pub df: f64,
}

pub fn welch(a: &[f64], b: &[f64]) -> Option<Welch> {
    if a.len() < 2 || b.len() < 2 {
        return None;
    }
    let (n1, n2) = (a.len() as f64, b.len() as f64);
    let (m1, m2) = (basic::mean(a), basic::mean(b));
    let v1 = a.iter().map(|x| (x - m1).powi(2)).sum::<f64>() / (n1 - 1.0);
    let v2 = b.iter().map(|x| (x - m2).powi(2)).sum::<f64>() / (n2 - 1.0);
    let (s1, s2) = (v1 / n1, v2 / n2);
    let se = (s1 + s2).sqrt();
    let df = if se > 0.0 {
        (s1 + s2).powi(2) / (s1 * s1 / (n1 - 1.0) + s2 * s2 / (n2 - 1.0))
    } else {
        n1 + n2 - 2.0
    };
    Some(Welch {
        difference: m2 - m1,
        se,
        df,
    })
}

/// p-value of the TOST of "the difference lies within ±`margin`" (absolute):
/// the larger of the p-values of the two one-sided Welch tests.
/// Equals `statsmodels.stats.weightstats.ttost_ind(..., usevar='unequal')`.
pub fn tost_p_value(w: &Welch, margin: f64) -> f64 {
    if w.se == 0.0 {
        return if w.difference.abs() < margin {
            0.0
        } else {
            1.0
        };
    }
    let lower = 1.0 - t_cdf((w.difference + margin) / w.se, w.df);
    let upper = t_cdf((w.difference - margin) / w.se, w.df);
    lower.max(upper)
}

/// Cohen's d of `b` relative to `a` (pooled standard deviation).
pub fn cohens_d(a: &[f64], b: &[f64]) -> Option<f64> {
    if a.len() < 2 || b.len() < 2 {
        return None;
    }
    let (n1, n2) = (a.len() as f64, b.len() as f64);
    let (m1, m2) = (basic::mean(a), basic::mean(b));
    let ss1: f64 = a.iter().map(|x| (x - m1).powi(2)).sum();
    let ss2: f64 = b.iter().map(|x| (x - m2).powi(2)).sum();
    let pooled = ((ss1 + ss2) / (n1 + n2 - 2.0)).sqrt();
    (pooled > 0.0).then(|| (m2 - m1) / pooled)
}

/// Probability that a value of `b` exceeds a value of `a`, ties counting half
/// (the Mann-Whitney U statistic of `b` over `n m`).
pub fn common_language_effect(a: &[f64], b: &[f64]) -> Option<f64> {
    if a.is_empty() || b.is_empty() {
        return None;
    }
    let mut sorted = a.to_vec();
    sorted.sort_by(f64::total_cmp);
    let wins: f64 = b
        .iter()
        .map(|&x| {
            let below = sorted.partition_point(|&y| y < x);
            let at_most = sorted.partition_point(|&y| y <= x);
            below as f64 + 0.5 * (at_most - below) as f64
        })
        .sum();
    Some(wins / (a.len() as f64 * b.len() as f64))
}

struct Measured {
    effect: Effect,
    /// TOST outcome, with `--equivalence-margin`
    equivalent: Option<bool>,
    /// p-value of the TOST (the mean only; the bootstrap has none)
    equivalence_p: Option<f64>,
}

/// Everything but the classification: the effect, and the TOST outcome if a margin is set.
fn measure(
    reference: &BenchmarkResult,
    other: &BenchmarkResult,
    settings: &StatSettings,
) -> Option<Measured> {
    let (a, b) = (reference.times.as_deref()?, other.times.as_deref()?);
    let center = reference.primary_center_with(Metric::Wall, settings.estimator)?;
    let other_center = other.primary_center_with(Metric::Wall, settings.estimator)?;
    if center <= 0.0 || a.len() < 3 || b.len() < 3 {
        return None;
    }
    let relative_change = other_center / center - 1.0;
    let welch = welch(a, b)?;
    let margin = settings.equivalence_margin.map(|m| m * center);

    let (ci, equivalent, equivalence_p) = if settings.estimator.is_mean() {
        let t = t_critical(settings.confidence_level, welch.df);
        let half = t * welch.se;
        let p = margin.map(|m| tost_p_value(&welch, m));
        (
            (
                (welch.difference - half) / center,
                (welch.difference + half) / center,
            ),
            p.map(|p| p < settings.significance_level),
            p,
        )
    } else {
        let (lo, hi) = difference_confidence_interval(a, b, settings)?;
        // TOST at level alpha is equivalent to the (1 - 2 alpha) interval lying inside the margin
        let tost_settings = StatSettings {
            confidence_level: 1.0 - 2.0 * settings.significance_level,
            ..settings.clone()
        };
        let equivalent = margin.and_then(|m| {
            let (tlo, thi) = difference_confidence_interval(a, b, &tost_settings)?;
            Some(tlo > -m && thi < m)
        });
        ((lo / center, hi / center), equivalent, None)
    };

    let standardized =
        cohens_d(a, b)
            .zip(common_language_effect(a, b))
            .map(|(cohens_d, common_language)| Standardized {
                cohens_d,
                common_language,
            });
    let mde =
        Some(minimum_detectable_effect(welch.se, settings.significance_level, POWER) / center)
            .filter(|v| v.is_finite());
    Some(Measured {
        effect: Effect {
            relative_change,
            ci_low: ci.0,
            ci_high: ci.1,
            standardized,
            minimum_detectable_effect: mde,
            runs_needed: None,
        },
        equivalent,
        equivalence_p,
    })
}

/// Classify the difference of `other` to `reference`. `p_value` is the p-value
/// of the difference (adjusted if `--correction` is active). `None` if the
/// samples are too small, or neither `--noise-threshold` nor
/// `--equivalence-margin` is set.
pub fn evaluate(
    reference: &BenchmarkResult,
    other: &BenchmarkResult,
    p_value: f64,
    settings: &StatSettings,
) -> Option<Verdict> {
    if settings.noise_threshold.is_none() && settings.equivalence_margin.is_none() {
        return None;
    }
    let Measured {
        mut effect,
        equivalent,
        equivalence_p,
    } = measure(reference, other, settings)?;
    let alpha = settings.significance_level;
    let change = effect.relative_change;
    let significant = p_value < alpha;
    let floor = settings
        .noise_threshold
        .unwrap_or(crate::stats::significance::MIN_EFFECT);
    let classification = if equivalent == Some(true) {
        Classification::Equivalent
    } else if change.abs() < floor && (settings.noise_threshold.is_some() || significant) {
        Classification::WithinNoise
    } else if significant {
        if change < 0.0 {
            Classification::Faster
        } else {
            Classification::Slower
        }
    } else {
        Classification::Inconclusive
    };
    if classification == Classification::Inconclusive {
        if let (Some(margin), Some(center), Some(a), Some(b)) = (
            settings.equivalence_margin,
            reference.primary_center_with(Metric::Wall, settings.estimator),
            reference.times.as_deref(),
            other.times.as_deref(),
        ) {
            let variance_sum = variance(a) + variance(b);
            effect.runs_needed =
                required_runs(variance_sum, margin * center, change * center, alpha, POWER);
        }
    }
    Some(Verdict {
        effect,
        classification,
        equivalence_p,
    })
}

fn variance(values: &[f64]) -> f64 {
    let n = values.len() as f64;
    let m = basic::mean(values);
    values.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (n - 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    const REF: [f64; 8] = [10.0, 10.4, 9.8, 10.1, 10.3, 9.9, 10.2, 10.0];
    const OTHER: [f64; 9] = [10.3, 10.6, 10.1, 10.5, 10.2, 10.7, 10.4, 10.35, 10.45];

    // SciPy 1.18.1 `scipy.stats.norm.ppf`
    #[test]
    fn normal_quantiles_match_scipy() {
        for (p, expected) in [
            (0.8, 0.8416212335729143),
            (0.9, 1.2815515655446004),
            (0.95, 1.644853626951472),
            (0.975, 1.959963984540054),
            (0.99, 2.3263478740408408),
            (0.999, 3.090232306167813),
            (0.6, 0.2533471031357997),
            (0.5001, 0.0002506628300880075),
            (0.01, -2.3263478740408408),
        ] {
            assert_relative_eq!(
                normal_quantile(p),
                expected,
                max_relative = 1e-8,
                epsilon = 1e-10
            );
        }
        assert!(normal_quantile(0.0).is_nan());
    }

    // statsmodels 0.15.0 `ttost_ind(OTHER, REF, -m, m, usevar='unequal')` with
    // m = margin * mean(REF) (mean(REF) = 10.1875); df = 14.431167956916864
    #[test]
    fn tost_matches_statsmodels() {
        let w = welch(&REF, &OTHER).unwrap();
        assert_relative_eq!(w.difference, 0.3125, max_relative = 1e-12);
        assert_relative_eq!(w.se, 0.09547032697824663, max_relative = 1e-12);
        assert_relative_eq!(w.df, 14.431167956916864, max_relative = 1e-9);
        let mean_ref = basic::mean(&REF);
        for (margin, expected) in [
            (0.01, 0.9784112172214158),
            (0.03, 0.5404793882103489),
            (0.05, 0.03176655285179594),
            (0.1, 1.6560413333537549e-06),
        ] {
            assert_relative_eq!(
                tost_p_value(&w, margin * mean_ref),
                expected,
                max_relative = 1e-6
            );
        }
    }

    #[test]
    fn welch_interval_matches_scipy() {
        // difference 0.3125 +- t(0.975, df) se with SciPy's t.ppf
        let w = welch(&REF, &OTHER).unwrap();
        let t = t_critical(0.95, w.df);
        assert_relative_eq!(
            w.difference - t * w.se,
            0.10830898126356606,
            max_relative = 1e-9
        );
        assert_relative_eq!(
            w.difference + t * w.se,
            0.516691018736434,
            max_relative = 1e-9
        );
    }

    #[test]
    fn zero_variance_tost_is_decided_by_the_difference() {
        let w = welch(&[1.0, 1.0, 1.0], &[1.02, 1.02, 1.02]).unwrap();
        assert_eq!(tost_p_value(&w, 0.05), 0.0);
        assert_eq!(tost_p_value(&w, 0.01), 1.0);
    }

    // numpy: d / sqrt(((n1-1) v1 + (n2-1) v2) / (n1 + n2 - 2))
    #[test]
    fn cohens_d_matches_the_formula() {
        assert_relative_eq!(
            cohens_d(&REF, &OTHER).unwrap(),
            1.5978437267653556,
            max_relative = 1e-12
        );
        assert!(cohens_d(&[1.0, 1.0], &[1.0, 1.0]).is_none());
    }

    // scipy.stats.mannwhitneyu(other, ref).statistic / (n m)
    #[test]
    fn common_language_effect_matches_mann_whitney() {
        assert_relative_eq!(common_language_effect(&REF, &OTHER).unwrap(), 0.875);
        let (a, b) = ([1.0, 2.0, 2.0, 3.0], [2.0, 2.0, 4.0]);
        assert_relative_eq!(
            common_language_effect(&a, &b).unwrap(),
            2.0 / 3.0,
            epsilon = 1e-15
        );
        assert_relative_eq!(
            common_language_effect(&b, &a).unwrap(),
            1.0 / 3.0,
            epsilon = 1e-15
        );
    }

    // scipy: z = norm.ppf(0.95) + norm.ppf(0.9); n = (v1 + v2) z^2 / (margin - |diff|)^2
    #[test]
    fn required_runs_follow_the_formula() {
        let variance_sum = 0.07687499999999994;
        let n = |margin, diff| required_runs(variance_sum, margin, diff, 0.05, 0.8);
        assert_eq!(n(0.5, 0.0), Some(3)); // 2.6334
        assert_eq!(n(0.5, 0.3125), Some(19)); // 18.7263
        assert_eq!(n(1.0, 0.3125), Some(2)); // 1.3929, at least 2
        assert_eq!(n(0.3, 0.3125), None);
        assert_eq!(n(0.3, 0.3), None);
        let big = required_runs(100.0, 0.5, 0.0, 0.05, 0.8).unwrap();
        let z: f64 = 1.644853626951472 + 1.2815515655446004;
        assert_eq!(big, (100.0 * z * z / 0.25).ceil() as u64);
    }

    // scipy: (norm.ppf(0.975) + norm.ppf(0.8)) * se
    #[test]
    fn minimum_detectable_effect_follows_the_formula() {
        assert_relative_eq!(
            minimum_detectable_effect(0.09547032697824663, 0.05, 0.8),
            0.2674682568306675,
            max_relative = 1e-8
        );
    }

    fn result(times: &[f64]) -> BenchmarkResult {
        BenchmarkResult {
            mean: basic::mean(times),
            times: Some(times.to_vec()),
            ..Default::default()
        }
    }

    fn settings(noise: Option<f64>, margin: Option<f64>) -> StatSettings {
        StatSettings {
            noise_threshold: noise,
            equivalence_margin: margin,
            ..Default::default()
        }
    }

    #[test]
    fn nothing_is_evaluated_without_the_options() {
        assert!(evaluate(&result(&REF), &result(&OTHER), 0.001, &settings(None, None)).is_none());
    }

    #[test]
    fn classification_follows_margin_noise_and_significance() {
        let (r, o) = (result(&REF), result(&OTHER));
        let class = |p, s: &StatSettings| evaluate(&r, &o, p, s).unwrap().classification;
        // change is +3.07%; TOST p = 0.0318 at 5%, 0.54 at 3%
        assert_eq!(
            class(0.001, &settings(None, Some(0.05))),
            Classification::Equivalent
        );
        assert_eq!(
            class(0.001, &settings(None, Some(0.03))),
            Classification::Slower
        );
        assert_eq!(
            class(0.2, &settings(None, Some(0.03))),
            Classification::Inconclusive
        );
        // noise threshold above the change: within noise whatever the p-value
        assert_eq!(
            class(0.001, &settings(Some(0.05), None)),
            Classification::WithinNoise
        );
        assert_eq!(
            class(0.9, &settings(Some(0.05), None)),
            Classification::WithinNoise
        );
        assert_eq!(
            class(0.001, &settings(Some(0.01), None)),
            Classification::Slower
        );
        assert_eq!(
            class(0.4, &settings(Some(0.01), None)),
            Classification::Inconclusive
        );
        // faster: swap the roles
        let swapped = evaluate(&o, &r, 0.001, &settings(Some(0.01), None)).unwrap();
        assert_eq!(swapped.classification, Classification::Faster);
    }

    #[test]
    fn an_inconclusive_result_suggests_a_run_count() {
        let (r, o) = (result(&REF), result(&OTHER));
        // margin 5% of 10.1875 = 0.509; room = 0.509 - 0.3125 = 0.197
        let v = evaluate(&r, &o, 0.2, &settings(None, Some(0.04))).unwrap();
        assert_eq!(v.classification, Classification::Inconclusive);
        let room: f64 = 0.04 * basic::mean(&REF) - 0.3125;
        let z = normal_quantile(0.95) + normal_quantile(0.9);
        let expected = (0.07687499999999994 * z * z / (room * room)).ceil() as u64;
        assert_eq!(v.effect.runs_needed, Some(expected));
        // none when the observed difference already exceeds the margin
        let v = evaluate(&r, &o, 0.2, &settings(None, Some(0.02))).unwrap();
        assert_eq!(v.effect.runs_needed, None);
    }

    #[test]
    fn the_effect_carries_the_interval_and_the_standardized_size() {
        let (r, o) = (result(&REF), result(&OTHER));
        let v = evaluate(&r, &o, 0.001, &settings(Some(0.01), None)).unwrap();
        let mean_ref = basic::mean(&REF);
        assert_relative_eq!(
            v.effect.relative_change,
            0.3125 / mean_ref,
            max_relative = 1e-12
        );
        assert_relative_eq!(
            v.effect.ci_low,
            0.10830898126356606 / mean_ref,
            max_relative = 1e-9
        );
        assert_relative_eq!(
            v.effect.ci_high,
            0.516691018736434 / mean_ref,
            max_relative = 1e-9
        );
        let s = v.effect.standardized.unwrap();
        assert_relative_eq!(s.cohens_d, 1.5978437267653556, max_relative = 1e-12);
        assert_relative_eq!(s.common_language, 0.875);
        assert!(v.effect.minimum_detectable_effect.unwrap() > 0.0);
    }

    #[test]
    fn a_wider_confidence_level_widens_the_interval() {
        let (r, o) = (result(&REF), result(&OTHER));
        let width = |level| {
            let s = StatSettings {
                confidence_level: level,
                ..settings(Some(0.01), None)
            };
            let e = evaluate(&r, &o, 0.001, &s).unwrap().effect;
            e.ci_high - e.ci_low
        };
        assert!(width(0.99) > width(0.95) && width(0.95) > width(0.8));
    }

    #[test]
    fn the_median_estimator_uses_a_bootstrap_interval() {
        let (r, o) = (result(&REF), result(&OTHER));
        let s = StatSettings {
            estimator: crate::stats::settings::Estimator::Median,
            ..settings(Some(0.01), Some(0.05))
        };
        let v = evaluate(&r, &o, 0.001, &s).unwrap();
        assert!(v.effect.ci_low < v.effect.relative_change);
        assert!(v.effect.ci_high > v.effect.relative_change);
        assert!(v.equivalence_p.is_none(), "the bootstrap has no p-value");
    }
}
