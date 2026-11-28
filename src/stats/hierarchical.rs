//! Hierarchical and paired experiments (#368).
//!
//! Repeated measurements inside one process, one worker or one batch are not
//! independent observations. Treating them as such understates the
//! uncertainty of the mean (Kalibera and Jones, "Rigorous benchmarking in
//! reasonable time", ISMM 2013). This module provides
//!
//! - the variance of a two-level sample split into a between-cluster and a
//!   within-cluster part ([`variance_components`], one-way random effects
//!   ANOVA estimators),
//! - the flat interval of the mean (Student's t over every unit) and the
//!   two-level bootstrap interval ([`flat_mean_ci`], [`hierarchical_mean_ci`]),
//! - the paired analysis of two commands measured in the same rounds of an
//!   interleaved schedule ([`paired`]).
//!
//! The functions work on plain slices. [`HierarchyReport`] is the
//! `results[].hierarchy` object of the JSON export and [`PairedAnalysis`] the
//! `results[].paired` object.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::stats::basic::{mean, standard_deviation};
use crate::stats::summary::percentile;

/// Variance of a two-level sample: clusters (workers, runs of a batch)
/// holding units (runs, instances).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct VarianceComponents {
    /// Variance of the cluster means around the grand mean, net of the
    /// within-cluster noise (never negative)
    pub between: f64,
    /// Variance of the units inside a cluster
    pub within: f64,
    /// Share of the total variance that lies between clusters, in [0, 1]
    pub icc: f64,
    /// Factor by which the variance of the mean exceeds the one of
    /// independent units: `1 + (mean cluster size - 1) * icc`
    pub design_effect: f64,
    /// Number of independent units that carry the information of the sample:
    /// units / design effect
    pub effective_n: f64,
}

/// Estimate [`VarianceComponents`] from `groups` (unbalanced one-way random
/// effects model). `None` with fewer than two clusters or without a second
/// unit in some cluster.
pub fn variance_components(groups: &[Vec<f64>]) -> Option<VarianceComponents> {
    let g = groups.len();
    let n: usize = groups.iter().map(Vec::len).sum();
    if g < 2 || n <= g || groups.iter().any(|x| x.is_empty()) {
        return None;
    }
    let nf = n as f64;
    let grand = groups.iter().flatten().sum::<f64>() / nf;
    let mut ssb = 0.0;
    let mut ssw = 0.0;
    let mut sum_sq_sizes = 0.0;
    for group in groups {
        let m = mean(group);
        ssb += group.len() as f64 * (m - grand).powi(2);
        ssw += group.iter().map(|x| (x - m).powi(2)).sum::<f64>();
        sum_sq_sizes += (group.len() as f64).powi(2);
    }
    let msb = ssb / (g - 1) as f64;
    let msw = ssw / (n - g) as f64;
    // effective cluster size of an unbalanced design
    let n0 = (nf - sum_sq_sizes / nf) / (g - 1) as f64;
    let between = ((msb - msw) / n0).max(0.0);
    // float noise around a constant sample is not variance
    let negligible = (between + msw).sqrt() <= 1e-12 * grand.abs();
    let (between, msw) = if negligible {
        (0.0, 0.0)
    } else {
        (between, msw)
    };
    let total = between + msw;
    let icc = if total > 0.0 { between / total } else { 0.0 };
    let design_effect = 1.0 + (nf / g as f64 - 1.0) * icc;
    Some(VarianceComponents {
        between,
        within: msw,
        icc,
        design_effect,
        effective_n: nf / design_effect,
    })
}

/// SplitMix64 with a fixed definition: the same seed gives the same interval
/// on every platform and version of 1.x.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..bound` (`bound` > 0; the modulo bias of a 64-bit draw
    /// is negligible for sample sizes)
    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    /// Uniform in (0, 1]
    #[cfg(test)]
    fn unit(&mut self) -> f64 {
        ((self.next() >> 11) + 1) as f64 / (1u64 << 53) as f64
    }
}

/// Student's t interval of the mean treating every unit as independent: what
/// a plain sample of runs gets.
pub fn flat_mean_ci(groups: &[Vec<f64>], confidence: f64) -> Option<(f64, f64)> {
    let all: Vec<f64> = groups.iter().flatten().copied().collect();
    if all.len() < 2 {
        return None;
    }
    let m = mean(&all);
    let sd = standard_deviation(&all, Some(m));
    let half = crate::stats::precision::t_critical(confidence, all.len() - 1) * sd
        / (all.len() as f64).sqrt();
    Some((m - half, m + half))
}

/// Percentile interval of the mean from a two-level bootstrap: draw the
/// clusters with replacement, then the units inside every drawn cluster with
/// replacement (Kalibera and Jones 2013). Deterministic for a `seed`. `None`
/// with fewer than two clusters.
pub fn hierarchical_mean_ci(
    groups: &[Vec<f64>],
    confidence: f64,
    resamples: usize,
    seed: u64,
) -> Option<(f64, f64)> {
    if groups.len() < 2 || groups.iter().any(|g| g.is_empty()) || resamples == 0 {
        return None;
    }
    let mut rng = SplitMix64(seed);
    let mut means = Vec::with_capacity(resamples);
    for _ in 0..resamples {
        let mut sum = 0.0;
        let mut count = 0usize;
        for _ in 0..groups.len() {
            let cluster = &groups[rng.below(groups.len())];
            for _ in 0..cluster.len() {
                sum += cluster[rng.below(cluster.len())];
            }
            count += cluster.len();
        }
        means.push(sum / count as f64);
    }
    // The percentile interval of a mean of g clusters has the width of a
    // normal interval, too narrow for few clusters: widen the distribution
    // by the ratio of the Student t and the normal quantile with g - 1
    // degrees of freedom (about 1.12 for 12 clusters, 1.03 for 50)
    let center =
        groups.iter().flatten().sum::<f64>() / groups.iter().map(Vec::len).sum::<usize>() as f64;
    let widen = crate::stats::dist::t_critical(confidence, (groups.len() - 1) as f64)
        / crate::stats::dist::t_critical(confidence, 1e9);
    for m in &mut means {
        *m = center + (*m - center) * widen;
    }
    means.sort_by(f64::total_cmp);
    let tail = 50.0 * (1.0 - confidence);
    Some((percentile(&means, tail)?, percentile(&means, 100.0 - tail)?))
}

/// `--ci-method`: how the interval of a mean is computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CiMethod {
    /// Every run is an independent observation
    #[default]
    Flat,
    /// Two-level bootstrap over the sample hierarchy
    Hierarchical,
}

impl CiMethod {
    pub fn name(self) -> &'static str {
        match self {
            CiMethod::Flat => "flat",
            CiMethod::Hierarchical => "hierarchical",
        }
    }
}

impl std::str::FromStr for CiMethod {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim() {
            "flat" => Ok(CiMethod::Flat),
            "hierarchical" => Ok(CiMethod::Hierarchical),
            other => Err(format!(
                "invalid --ci-method '{other}': expected 'flat' or 'hierarchical'"
            )),
        }
    }
}

/// The interval of the mean of the primary time, with both methods.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MeanCi {
    pub level: f64,
    /// The method of `--ci-method`: `flat` or `hierarchical`
    pub method: String,
    /// Lower bound of the selected method, in seconds
    pub lower: f64,
    /// Upper bound of the selected method, in seconds
    pub upper: f64,
    /// `[lower, upper]` of Student's t over all units
    pub flat: [f64; 2],
    /// `[lower, upper]` of the two-level bootstrap
    pub hierarchical: [f64; 2],
}

/// `results[].hierarchy` (new in 1.37, #368): the structure of the sample and
/// the variance at each of its levels.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct HierarchyReport {
    /// Levels of the sample, outermost first: `worker` (`--jobs`), `run`
    /// (one process each), `instance` (`--concurrency`)
    pub levels: Vec<String>,
    /// The level that forms the clusters of the analysis below
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_level: Option<String>,
    /// Number of clusters and of units in them
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clusters: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub units: Option<usize>,
    /// `--jobs`: the worker slot of every run (`null` for the initial
    /// measurement, which runs on the main thread), aligned with `times`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workers: Option<Vec<Option<usize>>>,
    /// `--region`: start/stop pairs per run of every region, the repetitions
    /// inside one process
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub repetitions: BTreeMap<String, Vec<Option<u64>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variance: Option<VarianceComponents>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mean_ci: Option<MeanCi>,
}

impl HierarchyReport {
    /// Fill the analysis of `groups` (clusters of units) at `cluster_level`.
    pub fn analyze(
        &mut self,
        cluster_level: &str,
        groups: &[Vec<f64>],
        method: CiMethod,
        confidence: f64,
        resamples: usize,
        seed: u64,
    ) {
        self.cluster_level = Some(cluster_level.to_string());
        self.clusters = Some(groups.len());
        self.units = Some(groups.iter().map(Vec::len).sum());
        self.variance = variance_components(groups);
        let flat = flat_mean_ci(groups, confidence);
        let boot = hierarchical_mean_ci(groups, confidence, resamples, seed);
        if let (Some(flat), Some(boot)) = (flat, boot) {
            let chosen = match method {
                CiMethod::Flat => flat,
                CiMethod::Hierarchical => boot,
            };
            self.mean_ci = Some(MeanCi {
                level: confidence,
                method: method.name().to_string(),
                lower: chosen.0,
                upper: chosen.1,
                flat: [flat.0, flat.1],
                hierarchical: [boot.0, boot.1],
            });
        }
    }
}

/// Group `values` by `keys` (equal keys form a cluster), in order of first
/// appearance.
pub fn group_by_key<K: PartialEq + Copy>(values: &[f64], keys: &[K]) -> Vec<Vec<f64>> {
    let mut seen: Vec<K> = Vec::new();
    let mut groups: Vec<Vec<f64>> = Vec::new();
    for (value, key) in values.iter().zip(keys) {
        match seen.iter().position(|k| k == key) {
            Some(i) => groups[i].push(*value),
            None => {
                seen.push(*key);
                groups.push(vec![*value]);
            }
        }
    }
    groups
}

/// `results[].paired` (new in 1.37, #368): the paired comparison with the
/// reference over the rounds of an interleaved schedule.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PairedAnalysis {
    /// The command it was compared with
    pub reference: String,
    /// Rounds in which both commands ran
    pub rounds: usize,
    /// Mean of `command - reference` over the rounds, in seconds
    pub mean_difference: f64,
    /// Student's t interval of the mean difference at `level`
    pub difference_ci: [f64; 2],
    pub level: f64,
    /// `mean_difference / mean(reference)`, with the same interval
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative_change: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative_change_ci: Option<[f64; 2]>,
    /// Two-sided p-value of the paired t-test
    pub p_value: f64,
    /// Pearson correlation of the two commands across rounds (the drift they
    /// share); `null` when a command has no variance
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation: Option<f64>,
    /// Variance of the paired differences relative to the sum of the two
    /// variances (the unpaired design), as a reduction: `1 - var(d) /
    /// (var(a) + var(b))`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variance_reduction: Option<f64>,
}

/// Pair the runs of `reference` and `other` by round (index) and test the
/// differences. `None` with fewer than three common rounds.
pub fn paired(
    reference_command: &str,
    reference: &[f64],
    other: &[f64],
    confidence: f64,
) -> Option<PairedAnalysis> {
    let n = reference.len().min(other.len());
    if n < 3 {
        return None;
    }
    let (a, b) = (&reference[..n], &other[..n]);
    let d: Vec<f64> = a.iter().zip(b).map(|(x, y)| y - x).collect();
    let md = mean(&d);
    let sd = standard_deviation(&d, Some(md));
    let se = sd / (n as f64).sqrt();
    let half = crate::stats::precision::t_critical(confidence, n - 1) * se;
    let p_value = if se > 0.0 {
        let t = md / se;
        (2.0 * (1.0 - crate::stats::dist::t_cdf(t.abs(), (n - 1) as f64))).clamp(0.0, 1.0)
    } else if md == 0.0 {
        1.0
    } else {
        0.0
    };
    let mean_ref = mean(a);
    let relative = (mean_ref > 0.0).then(|| md / mean_ref);
    let (ma, mb) = (mean_ref, mean(b));
    let (sa, sb) = (
        standard_deviation(a, Some(ma)),
        standard_deviation(b, Some(mb)),
    );
    // float noise around constant runs is not variance
    let floor = 1e-12 * ma.abs().max(mb.abs());
    let (sa, sb) = (
        if sa <= floor { 0.0 } else { sa },
        if sb <= floor { 0.0 } else { sb },
    );
    let correlation = (sa > 0.0 && sb > 0.0).then(|| {
        let cov = a
            .iter()
            .zip(b)
            .map(|(x, y)| (x - ma) * (y - mb))
            .sum::<f64>()
            / (n - 1) as f64;
        (cov / (sa * sb)).clamp(-1.0, 1.0)
    });
    let variance_reduction =
        (sa > 0.0 || sb > 0.0).then(|| (1.0 - sd.powi(2) / (sa.powi(2) + sb.powi(2))).min(1.0));
    Some(PairedAnalysis {
        reference: reference_command.to_string(),
        rounds: n,
        mean_difference: md,
        difference_ci: [md - half, md + half],
        level: confidence,
        relative_change: relative,
        relative_change_ci: relative.map(|_| [(md - half) / mean_ref, (md + half) / mean_ref]),
        p_value,
        correlation,
        variance_reduction,
    })
}

/// The paired analysis of `other` against `reference` over their `times`,
/// `None` when either has no per-run times, a run was omitted (the rounds no
/// longer line up) or fewer than three rounds are common.
pub fn paired_results(
    reference: &crate::benchmark::benchmark_result::BenchmarkResult,
    other: &crate::benchmark::benchmark_result::BenchmarkResult,
    confidence: f64,
) -> Option<PairedAnalysis> {
    if std::ptr::eq(reference, other)
        || !reference.omitted_failed_runs.is_empty()
        || !other.omitted_failed_runs.is_empty()
    {
        return None;
    }
    paired(
        &reference.command_with_unused_parameters,
        reference.times.as_deref()?,
        other.times.as_deref()?,
        confidence,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    #[test]
    fn variance_components_of_a_balanced_design() {
        // cluster means 10, 20; within variance 1 in both
        let groups = vec![vec![9.0, 10.0, 11.0], vec![19.0, 20.0, 21.0]];
        let v = variance_components(&groups).unwrap();
        assert_relative_eq!(v.within, 1.0);
        // MSB = 3 * 2 * 25 / 1 = 150; between = (150 - 1) / 3
        assert_relative_eq!(v.between, 149.0 / 3.0);
        assert_relative_eq!(v.icc, (149.0 / 3.0) / (149.0 / 3.0 + 1.0));
        assert_relative_eq!(v.design_effect, 1.0 + 2.0 * v.icc);
        assert_relative_eq!(v.effective_n, 6.0 / v.design_effect);
    }

    #[test]
    fn identical_clusters_have_no_between_variance() {
        let groups = vec![vec![1.0, 2.0, 3.0], vec![1.0, 2.0, 3.0]];
        let v = variance_components(&groups).unwrap();
        assert_eq!(v.between, 0.0);
        assert_eq!(v.icc, 0.0);
        assert_relative_eq!(v.design_effect, 1.0);
    }

    #[test]
    fn variance_components_need_two_clusters_and_replicates() {
        assert!(variance_components(&[vec![1.0, 2.0, 3.0]]).is_none());
        assert!(variance_components(&[vec![1.0], vec![2.0]]).is_none());
        assert!(variance_components(&[]).is_none());
    }

    #[test]
    fn the_flat_interval_is_students_t() {
        // mean 3, sd sqrt(2.5), n 5, t(4) = 2.776
        let groups = vec![vec![1.0, 2.0], vec![3.0, 4.0, 5.0]];
        let (lo, hi) = flat_mean_ci(&groups, 0.95).unwrap();
        let half = 2.776 * 2.5f64.sqrt() / 5f64.sqrt();
        assert_relative_eq!(lo, 3.0 - half, epsilon = 1e-9);
        assert_relative_eq!(hi, 3.0 + half, epsilon = 1e-9);
    }

    #[test]
    fn the_bootstrap_is_deterministic_and_wider_for_nested_data() {
        let groups: Vec<Vec<f64>> = (0..6)
            .map(|c| (0..5).map(|i| c as f64 * 10.0 + i as f64 * 0.1).collect())
            .collect();
        let a = hierarchical_mean_ci(&groups, 0.95, 1000, 7).unwrap();
        assert_eq!(a, hierarchical_mean_ci(&groups, 0.95, 1000, 7).unwrap());
        let flat = flat_mean_ci(&groups, 0.95).unwrap();
        // the clusters differ by far more than the units inside them, and
        // both methods see the spread between the cluster means
        assert!(a.0 < 25.0 && a.1 > 25.0, "{a:?}");
        assert!(flat.0 < 25.0 && flat.1 > 25.0, "{flat:?}");
        assert!(hierarchical_mean_ci(&groups[..1], 0.95, 1000, 7).is_none());
    }

    /// A standard normal draw (Box-Muller)
    fn normal(rng: &mut SplitMix64) -> f64 {
        let (u1, u2) = (rng.unit(), rng.unit());
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }

    /// Share of `replications` samples of `clusters` x `units` nested data
    /// (true mean 0) whose interval contains 0, for both methods.
    fn coverage(
        clusters: usize,
        units: usize,
        sd_between: f64,
        sd_within: f64,
        replications: usize,
    ) -> (f64, f64) {
        let mut rng = SplitMix64(368);
        let (mut flat_hits, mut hier_hits) = (0, 0);
        for r in 0..replications {
            let groups: Vec<Vec<f64>> = (0..clusters)
                .map(|_| {
                    let effect = sd_between * normal(&mut rng);
                    (0..units)
                        .map(|_| effect + sd_within * normal(&mut rng))
                        .collect()
                })
                .collect();
            let (lo, hi) = flat_mean_ci(&groups, 0.95).unwrap();
            flat_hits += usize::from(lo <= 0.0 && 0.0 <= hi);
            let (lo, hi) = hierarchical_mean_ci(&groups, 0.95, 400, r as u64).unwrap();
            hier_hits += usize::from(lo <= 0.0 && 0.0 <= hi);
        }
        (
            flat_hits as f64 / replications as f64,
            hier_hits as f64 / replications as f64,
        )
    }

    // Seeded Monte Carlo (#368): 500 samples of nested data with a true mean
    // of 0, between and within standard deviations 1 (ICC 0.5). Treating the
    // runs as independent covers the true mean far below the nominal 95%; the
    // two-level bootstrap is near it, also with few clusters.
    #[test]
    fn monte_carlo_flat_undercovers_nested_data_hierarchical_does_not() {
        for (clusters, units) in [(6, 10), (12, 10), (30, 5)] {
            let (flat, hier) = coverage(clusters, units, 1.0, 1.0, 500);
            assert!(flat < 0.80, "{clusters}x{units}: flat coverage {flat}");
            assert!(
                (0.91..=0.985).contains(&hier),
                "{clusters}x{units}: hierarchical coverage {hier}"
            );
        }
    }

    // Without nesting (all variance inside the clusters) the hierarchical
    // interval is not narrower than it should be.
    #[test]
    fn monte_carlo_independent_data_is_not_undercovered() {
        let (flat, hier) = coverage(12, 10, 0.0, 1.0, 500);
        assert!((0.91..=0.985).contains(&flat), "flat {flat}");
        assert!(hier >= 0.91, "hierarchical {hier}");
    }

    #[test]
    fn groups_keep_first_appearance_order() {
        let g = group_by_key(&[1.0, 2.0, 3.0, 4.0], &[Some(2), Some(0), Some(2), None]);
        assert_eq!(g, vec![vec![1.0, 3.0], vec![2.0], vec![4.0]]);
    }

    #[test]
    fn parses_ci_methods() {
        assert_eq!("flat".parse(), Ok(CiMethod::Flat));
        assert_eq!("hierarchical".parse(), Ok(CiMethod::Hierarchical));
        assert!("deep".parse::<CiMethod>().is_err());
    }

    // a = [10, 11, 12, 13, 14], b = a + [1.0, 1.2, 0.8, 1.1, 0.9]: the
    // differences have mean 1.0 and sd 0.1581139, so t = 14.1421356 with 4
    // degrees of freedom; the exact closed form of the t(4) CDF gives
    // p = 0.00014513
    #[test]
    fn the_paired_test_matches_scipy() {
        let a = [10.0, 11.0, 12.0, 13.0, 14.0];
        let b = [11.0, 12.2, 12.8, 14.1, 14.9];
        let p = paired("ref", &a, &b, 0.95).unwrap();
        assert_eq!(p.rounds, 5);
        assert_relative_eq!(p.mean_difference, 1.0, epsilon = 1e-12);
        assert_relative_eq!(p.p_value, 0.00014513, epsilon = 1e-7);
        let half = 2.776 * 0.1581139 / 5f64.sqrt();
        assert_relative_eq!(p.difference_ci[0], 1.0 - half, epsilon = 1e-4);
        assert_relative_eq!(p.relative_change.unwrap(), 1.0 / 12.0, epsilon = 1e-12);
        // the drift is shared, so the pairing removes almost all variance
        assert!(p.correlation.unwrap() > 0.99);
        assert!(p.variance_reduction.unwrap() > 0.99);
    }

    #[test]
    fn identical_runs_are_not_different() {
        let a = [1.0, 2.0, 3.0, 4.0];
        let p = paired("ref", &a, &a, 0.95).unwrap();
        assert_eq!(p.mean_difference, 0.0);
        assert_eq!(p.p_value, 1.0);
        assert!(paired("ref", &a[..2], &a, 0.95).is_none());
    }
}
