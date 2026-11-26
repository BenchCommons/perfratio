//! Bootstrap confidence intervals and a two-sample bootstrap test for
//! `--deep-stats`.
//!
//! This replaces the unmaintained `criterion-stats` crate (#31) with the same
//! methods: percentile bootstrap intervals, and a Welch t statistic compared
//! against its distribution under the null hypothesis, obtained by resampling
//! the pooled ("mixed") samples. A fixed seed makes the results reproducible:
//! the same measurements always give the same intervals and p-value.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use crate::stats::basic::{mean, median, standard_deviation};
use crate::stats::settings::{self, Estimator, StatSettings};
use crate::stats::summary::percentile;

#[derive(Debug, Clone, PartialEq)]
pub struct DeepStats {
    pub mean_ci_lower: f64,
    pub mean_ci_upper: f64,
    pub median_ci_lower: f64,
    pub median_ci_upper: f64,
    pub std_dev_ci_lower: f64,
    pub std_dev_ci_upper: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ComparisonStats {
    pub p_value: f64,
    pub t_statistic: f64,
    pub is_significant_05: bool,
    pub is_significant_01: bool,
}

/// Draw `n` values from `pool` with replacement into `out`.
fn resample_into(rng: &mut StdRng, pool: &[f64], n: usize, out: &mut Vec<f64>) {
    out.clear();
    out.extend((0..n).map(|_| pool[rng.gen_range(0..pool.len())]));
}

/// Lower and upper bound of the central `confidence` interval of a
/// bootstrap distribution (percentile method).
fn confidence_interval(mut distribution: Vec<f64>, confidence: f64) -> (f64, f64) {
    distribution.sort_by(f64::total_cmp);
    let tail = 50.0 * (1.0 - confidence);
    (
        percentile(&distribution, tail).unwrap_or(f64::NAN),
        percentile(&distribution, 100.0 - tail).unwrap_or(f64::NAN),
    )
}

/// Welch's t statistic (sample variances, n - 1), incorporating optional baseline
/// mean variances from `--subtract`.
fn welch_t_with_variance(a: &[f64], b: &[f64], var_base_a: f64, var_base_b: f64) -> f64 {
    let (mean_a, mean_b) = (mean(a), mean(b));
    let var_a = standard_deviation(a, Some(mean_a)).powi(2);
    let var_b = standard_deviation(b, Some(mean_b)).powi(2);
    let se_sq = var_a / a.len() as f64 + var_base_a + var_b / b.len() as f64 + var_base_b;
    (mean_a - mean_b) / se_sq.sqrt()
}

/// Welch's t statistic (sample variances, n - 1).
#[cfg(test)]
fn welch_t(a: &[f64], b: &[f64]) -> f64 {
    welch_t_with_variance(a, b, 0.0, 0.0)
}

/// Two-tailed p-value of `t` in the null distribution (the same definition as
/// criterion-stats: twice the smaller tail fraction).
fn two_tailed_p_value(distribution: &[f64], t: f64) -> f64 {
    let n = distribution.len();
    let below = distribution.iter().filter(|&&x| x < t).count();
    below.min(n - below) as f64 / n as f64 * 2.0
}

/// Compute bootstrap confidence intervals for mean, median, and std_dev with
/// the installed settings (95% and 5000 resamples unless configured).
pub fn compute_deep_stats(data: &[f64]) -> Option<DeepStats> {
    compute_deep_stats_with(data, &settings::current())
}

/// [`compute_deep_stats`] with explicit settings.
pub fn compute_deep_stats_with(data: &[f64], settings: &StatSettings) -> Option<DeepStats> {
    if data.len() < 3 {
        return None;
    }

    let n_resamples = settings.bootstrap_resamples;
    let mut rng = StdRng::seed_from_u64(settings.seed);
    let mut resample = Vec::with_capacity(data.len());
    let mut means = Vec::with_capacity(n_resamples);
    let mut medians = Vec::with_capacity(n_resamples);
    let mut std_devs = Vec::with_capacity(n_resamples);

    for _ in 0..n_resamples {
        resample_into(&mut rng, data, data.len(), &mut resample);
        let m = mean(&resample);
        means.push(m);
        medians.push(median(&resample));
        std_devs.push(standard_deviation(&resample, Some(m)));
    }

    let level = settings.confidence_level;
    let (mean_ci_lower, mean_ci_upper) = confidence_interval(means, level);
    let (median_ci_lower, median_ci_upper) = confidence_interval(medians, level);
    let (std_dev_ci_lower, std_dev_ci_upper) = confidence_interval(std_devs, level);

    Some(DeepStats {
        mean_ci_lower,
        mean_ci_upper,
        median_ci_lower,
        median_ci_upper,
        std_dev_ci_lower,
        std_dev_ci_upper,
    })
}

/// Perform a two-sample bootstrap hypothesis test comparing sample `a` and sample `b`,
/// incorporating optional baseline mean variances from `--subtract`.
pub fn compare_samples_with_baseline_variance(
    a: &[f64],
    b: &[f64],
    var_base_a: f64,
    var_base_b: f64,
) -> Option<ComparisonStats> {
    compare_samples_with_settings(a, b, var_base_a, var_base_b, &settings::current())
}

/// [`compare_samples_with_baseline_variance`] with explicit settings. For the
/// mean the statistic is Welch's t; for the median and percentiles it is the
/// difference of the estimates (the baseline variance of `--subtract` only
/// applies to the mean and is ignored there).
pub fn compare_samples_with_settings(
    a: &[f64],
    b: &[f64],
    var_base_a: f64,
    var_base_b: f64,
    settings: &StatSettings,
) -> Option<ComparisonStats> {
    if a.len() < 3 || b.len() < 3 {
        return None;
    }
    if !settings.estimator.is_mean() {
        return compare_estimates(a, b, settings);
    }

    let t_stat = welch_t_with_variance(a, b, var_base_a, var_base_b);
    if !t_stat.is_finite() {
        // Zero variance in both samples:
        // If means are equal, the samples are identical -> p = 1.0, no difference
        let same = (mean(a) - mean(b)).abs() <= f64::EPSILON * mean(a).abs().max(1.0);
        return if same {
            Some(ComparisonStats {
                p_value: 1.0,
                t_statistic: 0.0,
                is_significant_05: false,
                is_significant_01: false,
            })
        } else {
            // Constant but different samples without variance: t-test is undefined
            None
        };
    }

    // Null distribution of t: resample both groups from the pooled sample.
    let pooled: Vec<f64> = a.iter().chain(b).copied().collect();
    let mut rng = StdRng::seed_from_u64(settings.seed);
    let (mut resample_a, mut resample_b) = (Vec::new(), Vec::new());
    let distribution: Vec<f64> = (0..settings.bootstrap_resamples)
        .map(|_| {
            resample_into(&mut rng, &pooled, a.len(), &mut resample_a);
            resample_into(&mut rng, &pooled, b.len(), &mut resample_b);
            welch_t_with_variance(&resample_a, &resample_b, var_base_a, var_base_b)
        })
        .collect();
    let p_val = two_tailed_p_value(&distribution, t_stat);

    if !p_val.is_finite() {
        return None;
    }

    Some(ComparisonStats {
        p_value: p_val,
        t_statistic: t_stat,
        is_significant_05: p_val < 0.05,
        is_significant_01: p_val < 0.01,
    })
}

/// Bootstrap test of the difference of two estimates (median or percentile):
/// the null distribution comes from resampling both groups out of the pooled
/// sample. `t_statistic` is the difference in units of the null distribution's
/// standard deviation.
fn compare_estimates(a: &[f64], b: &[f64], settings: &StatSettings) -> Option<ComparisonStats> {
    let estimator = settings.estimator;
    let observed = estimator.estimate(a) - estimator.estimate(b);
    let constant = |xs: &[f64]| xs.iter().all(|&x| x == xs[0]);
    if constant(a) && constant(b) {
        // no variance in either sample: identical or certainly different
        return (observed == 0.0).then_some(ComparisonStats {
            p_value: 1.0,
            t_statistic: 0.0,
            is_significant_05: false,
            is_significant_01: false,
        });
    }
    let pooled: Vec<f64> = a.iter().chain(b).copied().collect();
    let mut rng = StdRng::seed_from_u64(settings.seed);
    let (mut resample_a, mut resample_b) = (Vec::new(), Vec::new());
    let distribution: Vec<f64> = (0..settings.bootstrap_resamples)
        .map(|_| {
            resample_into(&mut rng, &pooled, a.len(), &mut resample_a);
            resample_into(&mut rng, &pooled, b.len(), &mut resample_b);
            estimator.estimate(&resample_a) - estimator.estimate(&resample_b)
        })
        .collect();
    // A median or percentile of resamples takes few distinct values, so ties
    // with the observed difference must count for both tails.
    let n = distribution.len() as f64;
    let at_most = distribution.iter().filter(|&&x| x <= observed).count() as f64 / n;
    let at_least = distribution.iter().filter(|&&x| x >= observed).count() as f64 / n;
    let p_val = (2.0 * at_most.min(at_least)).min(1.0);
    let sd = standard_deviation(&distribution, None);
    Some(ComparisonStats {
        p_value: p_val,
        t_statistic: if sd > 0.0 { observed / sd } else { 0.0 },
        is_significant_05: p_val < 0.05,
        is_significant_01: p_val < 0.01,
    })
}

/// Bootstrap confidence interval of `estimator(other) / estimator(reference)`
/// (resampling both groups independently), for the ratio uncertainty of
/// estimators whose ratio has no simple error propagation (median, percentiles).
pub fn ratio_confidence_interval(
    reference: &[f64],
    other: &[f64],
    settings: &StatSettings,
) -> Option<(f64, f64)> {
    if reference.len() < 3 || other.len() < 3 {
        return None;
    }
    let estimator: Estimator = settings.estimator;
    let mut rng = StdRng::seed_from_u64(settings.seed);
    let (mut resample_ref, mut resample_other) = (Vec::new(), Vec::new());
    let ratios: Vec<f64> = (0..settings.bootstrap_resamples)
        .map(|_| {
            resample_into(&mut rng, reference, reference.len(), &mut resample_ref);
            resample_into(&mut rng, other, other.len(), &mut resample_other);
            estimator.estimate(&resample_other) / estimator.estimate(&resample_ref)
        })
        .filter(|r| r.is_finite())
        .collect();
    if ratios.len() < 100 {
        return None;
    }
    Some(confidence_interval(ratios, settings.confidence_level))
}

/// Bootstrap confidence interval of `estimator(b) - estimator(a)` (both
/// groups resampled independently).
pub fn difference_confidence_interval(
    a: &[f64],
    b: &[f64],
    settings: &StatSettings,
) -> Option<(f64, f64)> {
    if a.len() < 3 || b.len() < 3 {
        return None;
    }
    let mut rng = StdRng::seed_from_u64(settings.seed);
    let (mut resample_a, mut resample_b) = (Vec::new(), Vec::new());
    let differences: Vec<f64> = (0..settings.bootstrap_resamples)
        .map(|_| {
            resample_into(&mut rng, a, a.len(), &mut resample_a);
            resample_into(&mut rng, b, b.len(), &mut resample_b);
            settings.estimator.estimate(&resample_b) - settings.estimator.estimate(&resample_a)
        })
        .collect();
    Some(confidence_interval(differences, settings.confidence_level))
}

/// Perform a two-sample bootstrap hypothesis test comparing sample `a` and sample `b`
pub fn compare_samples(a: &[f64], b: &[f64]) -> Option<ComparisonStats> {
    compare_samples_with_baseline_variance(a, b, 0.0, 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_deep_stats() {
        let data = vec![0.10, 0.11, 0.10, 0.12, 0.09, 0.11];
        let stats = compute_deep_stats(&data).unwrap();
        assert!(stats.mean_ci_lower <= stats.mean_ci_upper);
        assert!(stats.median_ci_lower <= stats.median_ci_upper);
        assert!(stats.std_dev_ci_lower <= stats.std_dev_ci_upper);
    }

    #[test]
    fn test_compare_samples_significance() {
        let sample_fast = vec![0.01, 0.011, 0.01, 0.012, 0.009, 0.011];
        let sample_slow = vec![0.50, 0.51, 0.49, 0.52, 0.48, 0.51];
        let cmp = compare_samples(&sample_fast, &sample_slow).unwrap();
        assert!(cmp.is_significant_05);
        assert!(cmp.is_significant_01);
    }

    #[test]
    fn test_compare_samples_identical_constant() {
        let sample1 = vec![1.0, 1.0, 1.0, 1.0];
        let sample2 = vec![1.0, 1.0, 1.0, 1.0];
        let cmp = compare_samples(&sample1, &sample2).unwrap();
        assert_eq!(cmp.p_value, 1.0);
        assert_eq!(cmp.t_statistic, 0.0);
        assert!(!cmp.is_significant_05);
        assert!(!cmp.is_significant_01);
    }

    #[test]
    fn test_compare_samples_different_constant() {
        let sample1 = vec![1.0, 1.0, 1.0, 1.0];
        let sample2 = vec![2.0, 2.0, 2.0, 2.0];
        assert!(compare_samples(&sample1, &sample2).is_none());
    }

    #[test]
    fn test_compare_samples_insufficient() {
        let sample1 = vec![1.0, 1.0];
        let sample2 = vec![1.0, 1.0];
        assert!(compare_samples(&sample1, &sample2).is_none());
    }

    #[test]
    fn test_welch_t_matches_the_textbook_formula() {
        // means 2 and 5, sample variances 1 and 1, n = 3 each
        let t = welch_t(&[1.0, 2.0, 3.0], &[4.0, 5.0, 6.0]);
        assert!((t - (-3.0 / (2.0f64 / 3.0).sqrt())).abs() < 1e-12);
    }

    #[test]
    fn test_two_tailed_p_value() {
        let distribution: Vec<f64> = (0..100).map(f64::from).collect();
        assert_eq!(two_tailed_p_value(&distribution, 50.0), 1.0);
        assert_eq!(two_tailed_p_value(&distribution, 5.0), 0.1);
        assert_eq!(two_tailed_p_value(&distribution, 1000.0), 0.0);
    }

    #[test]
    fn test_confidence_intervals_contain_the_point_estimates() {
        let data: Vec<f64> = (0..40).map(|i| 1.0 + f64::from(i % 7) * 0.01).collect();
        let stats = compute_deep_stats(&data).unwrap();
        let m = mean(&data);
        assert!(stats.mean_ci_lower <= m && m <= stats.mean_ci_upper);
        let md = median(&data);
        assert!(stats.median_ci_lower <= md && md <= stats.median_ci_upper);
        let sd = standard_deviation(&data, None);
        assert!(stats.std_dev_ci_lower <= sd && sd <= stats.std_dev_ci_upper);
    }

    #[test]
    fn test_results_are_reproducible() {
        let data = vec![0.10, 0.11, 0.10, 0.12, 0.09, 0.11, 0.13, 0.10];
        assert_eq!(compute_deep_stats(&data), compute_deep_stats(&data));
        let other = vec![0.12, 0.13, 0.11, 0.14, 0.12, 0.13, 0.12, 0.15];
        assert_eq!(
            compare_samples(&data, &other),
            compare_samples(&data, &other)
        );
    }

    #[test]
    fn test_no_significance_for_samples_from_the_same_distribution() {
        let a = vec![0.100, 0.102, 0.099, 0.101, 0.100, 0.103, 0.098, 0.101];
        let b = vec![0.101, 0.099, 0.100, 0.102, 0.100, 0.098, 0.101, 0.100];
        let cmp = compare_samples(&a, &b).unwrap();
        assert!(!cmp.is_significant_05, "{cmp:?}");
    }

    #[test]
    fn test_baseline_variance_increases_p_value() {
        let a = vec![0.100, 0.102, 0.101, 0.103, 0.100];
        let b = vec![0.105, 0.107, 0.106, 0.108, 0.105];
        let without_base = compare_samples(&a, &b).unwrap();
        // With a noisy baseline subtracted, the difference is less certain
        let with_base = compare_samples_with_baseline_variance(&a, &b, 0.0001, 0.0001).unwrap();
        assert!(with_base.p_value >= without_base.p_value);
        assert!(with_base.t_statistic.abs() < without_base.t_statistic.abs());
    }

    fn noisy() -> Vec<f64> {
        vec![
            1.0, 1.1, 0.9, 1.05, 0.95, 1.2, 0.8, 1.0, 1.02, 0.98, 1.3, 0.7, 1.01, 0.99, 1.04,
        ]
    }

    #[test]
    fn intervals_widen_with_the_confidence_level() {
        let data = noisy();
        let width = |level: f64| {
            let settings = StatSettings {
                confidence_level: level,
                ..Default::default()
            };
            let ci = compute_deep_stats_with(&data, &settings).unwrap();
            (
                ci.mean_ci_upper - ci.mean_ci_lower,
                ci.median_ci_upper - ci.median_ci_lower,
            )
        };
        let (m80, d80) = width(0.80);
        let (m95, d95) = width(0.95);
        let (m99, d99) = width(0.99);
        assert!(m80 < m95 && m95 < m99, "{m80} {m95} {m99}");
        assert!(d80 <= d95 && d95 <= d99, "{d80} {d95} {d99}");
    }

    #[test]
    fn default_settings_reproduce_the_historical_intervals() {
        let data = noisy();
        assert_eq!(
            compute_deep_stats_with(&data, &StatSettings::default()),
            compute_deep_stats(&data)
        );
    }

    #[test]
    fn the_seed_makes_the_output_deterministic() {
        let data = noisy();
        let with_seed = |seed| {
            compute_deep_stats_with(
                &data,
                &StatSettings {
                    seed,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        assert_eq!(with_seed(7), with_seed(7));
        assert_ne!(with_seed(7), with_seed(8));
        let (a, b) = (noisy(), noisy().iter().map(|x| x * 1.1).collect::<Vec<_>>());
        let p = |seed| {
            compare_samples_with_settings(
                &a,
                &b,
                0.0,
                0.0,
                &StatSettings {
                    seed,
                    ..Default::default()
                },
            )
            .unwrap()
            .p_value
        };
        assert_eq!(p(3), p(3));
    }

    #[test]
    fn median_test_is_not_fooled_by_one_outlier() {
        // the means differ by ~0.19 because of one slow run, the medians are equal
        let a = vec![
            1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.01,
        ];
        let mut b = a.clone();
        b[0] = 4.0;
        let median = StatSettings {
            estimator: Estimator::Median,
            ..Default::default()
        };
        let p_median = compare_samples_with_settings(&a, &b, 0.0, 0.0, &median)
            .unwrap()
            .p_value;
        assert!(p_median > 0.5, "p = {p_median}");
    }

    #[test]
    fn ratio_interval_contains_the_point_estimate() {
        let reference = noisy();
        let other: Vec<f64> = noisy().iter().map(|x| x * 2.0).collect();
        let settings = StatSettings {
            estimator: Estimator::Median,
            ..Default::default()
        };
        let (lo, hi) = ratio_confidence_interval(&reference, &other, &settings).unwrap();
        assert!(lo <= 2.0 + 1e-9 && 2.0 - 1e-9 <= hi, "[{lo}, {hi}]");
        assert!(hi - lo < 0.5);
        assert!(ratio_confidence_interval(&reference[..2], &other, &settings).is_none());
    }
}
