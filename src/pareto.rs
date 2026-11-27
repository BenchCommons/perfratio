//! `--pareto`, `--constraint`, `--objective`: Pareto frontier and
//! constraint-aware comparison (#326, `docs/specs/pareto.md`).

use std::fmt::Write as _;

use serde::Serialize;

use crate::benchmark::benchmark_result::BenchmarkResult;
use crate::metric::Metric;
use crate::output::format::format_duration;
use crate::stats::settings::{Estimator, StatSettings};
use crate::stats::summary::percentile;

/// Which way a metric improves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    LowerIsBetter,
    HigherIsBetter,
}

/// Direction of a metric. Every built-in metric improves downwards; a custom metric follows
/// its descriptor (`--output-metric NAME[unit,higher]=REGEX`, #317).
pub fn direction(metric: Metric) -> Direction {
    if metric.higher_is_better() {
        Direction::HigherIsBetter
    } else {
        Direction::LowerIsBetter
    }
}

/// Statistic of the per-run samples that represents a command.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Statistic {
    Mean,
    Median,
    Min,
    Max,
    /// Percentile, 0 to 100
    Percentile(f64),
}

impl Statistic {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "mean" => Some(Self::Mean),
            "median" => Some(Self::Median),
            "min" => Some(Self::Min),
            "max" => Some(Self::Max),
            _ => {
                let p: f64 = s.strip_prefix('p')?.parse().ok()?;
                (0.0..=100.0).contains(&p).then_some(Self::Percentile(p))
            }
        }
    }

    /// The estimator of the tests behind `--pareto-significance`: the one that matches
    /// the statistic being compared
    fn estimator(self) -> Estimator {
        match self {
            Self::Mean => Estimator::Mean,
            Self::Median => Estimator::Median,
            Self::Min => Estimator::Percentile(0.0),
            Self::Max => Estimator::Percentile(100.0),
            Self::Percentile(p) => Estimator::Percentile(p),
        }
    }

    /// The statistic of `samples` (finite values only); `None` if there are none
    fn of(self, samples: &[f64]) -> Option<f64> {
        let mut sorted: Vec<f64> = samples.iter().copied().filter(|v| v.is_finite()).collect();
        if sorted.is_empty() {
            return None;
        }
        if self == Self::Mean {
            return Some(sorted.iter().sum::<f64>() / sorted.len() as f64);
        }
        sorted.sort_by(f64::total_cmp);
        match self {
            Self::Median => percentile(&sorted, 50.0),
            Self::Min => Some(sorted[0]),
            Self::Max => Some(sorted[sorted.len() - 1]),
            Self::Percentile(p) => percentile(&sorted, p),
            Self::Mean => None,
        }
    }

    fn name(self) -> String {
        match self {
            Self::Mean => "mean".into(),
            Self::Median => "median".into(),
            Self::Min => "min".into(),
            Self::Max => "max".into(),
            Self::Percentile(p) => format!("p{p}"),
        }
    }
}

/// A metric with the statistic taken from it, written `wall`, `wall.p95`, `memory.max`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Axis {
    pub metric: Metric,
    pub statistic: Statistic,
}

impl Axis {
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let (metric_part, statistic) = match s.rsplit_once('.') {
            Some((m, stat)) if Statistic::parse(&stat.to_ascii_lowercase()).is_some() => (
                m,
                Statistic::parse(&stat.to_ascii_lowercase()).unwrap_or(Statistic::Mean),
            ),
            Some((_, stat)) if stat.starts_with('p') || stat.starts_with("me") => {
                return Err(format!(
                    "invalid statistic '{stat}' in '{s}': use mean, median, min, max or pNN (0-100)"
                ))
            }
            _ => (s, Statistic::Mean),
        };
        let metric = metric_part.parse::<Metric>()?;
        Ok(Self { metric, statistic })
    }

    /// Name as written, e.g. `wall` or `wall.p95`
    pub fn name(&self) -> String {
        match self.statistic {
            Statistic::Mean => self.metric.to_string(),
            stat => format!("{}.{}", self.metric, stat.name()),
        }
    }

    /// Value of the axis for a result; `None` if the result has no data for it
    /// (an unavailable metric is never treated as 0).
    pub fn value(&self, result: &BenchmarkResult) -> Option<f64> {
        let value = match self.statistic {
            Statistic::Mean => result.primary_mean(self.metric)?,
            stat => {
                let mut samples = result.primary_samples(self.metric)?;
                samples.retain(|v| v.is_finite());
                if samples.is_empty() {
                    return None;
                }
                samples.sort_by(f64::total_cmp);
                match stat {
                    Statistic::Median => percentile(&samples, 50.0)?,
                    Statistic::Min => samples[0],
                    Statistic::Max => samples[samples.len() - 1],
                    Statistic::Percentile(p) => percentile(&samples, p)?,
                    Statistic::Mean => unreachable!("handled above"),
                }
            }
        };
        value.is_finite().then_some(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Le,
    Lt,
    Ge,
    Gt,
}

impl Op {
    fn holds(self, value: f64, limit: f64) -> bool {
        match self {
            Op::Le => value <= limit,
            Op::Lt => value < limit,
            Op::Ge => value >= limit,
            Op::Gt => value > limit,
        }
    }
}

/// `--constraint 'memory <= 256MiB'`
#[derive(Debug, Clone, PartialEq)]
pub struct Constraint {
    pub axis: Axis,
    pub op: Op,
    /// In the base unit of the metric (seconds, bytes, joules, plain count)
    pub value: f64,
    /// As the user wrote it, trimmed
    pub text: String,
}

impl Constraint {
    pub fn parse(s: &str) -> Result<Self, String> {
        let text = s.trim().to_string();
        // two-character operators first
        let (pos, op, len) = [("<=", Op::Le), (">=", Op::Ge), ("<", Op::Lt), (">", Op::Gt)]
            .iter()
            .filter_map(|(sym, op)| text.find(sym).map(|p| (p, *op, sym.len())))
            .min_by_key(|(p, _, len)| (*p, usize::MAX - len))
            .ok_or_else(|| {
                format!("invalid constraint '{text}': expected '<METRIC> <op> <VALUE>' with <=, <, >= or >")
            })?;
        let axis =
            Axis::parse(&text[..pos]).map_err(|e| format!("invalid constraint '{text}': {e}"))?;
        let value = parse_quantity(axis.metric, text[pos + len..].trim())
            .map_err(|e| format!("invalid constraint '{text}': {e}"))?;
        Ok(Self {
            axis,
            op,
            value,
            text,
        })
    }
}

/// Parse a number with an optional unit, into the base unit of `metric`.
pub(crate) fn parse_quantity(metric: Metric, s: &str) -> Result<f64, String> {
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E')))
        .unwrap_or(s.len());
    let (number, unit) = s.split_at(split);
    let number: f64 = number
        .parse()
        .map_err(|_| format!("'{s}' is not a number with an optional unit"))?;
    if !number.is_finite() {
        return Err(format!("'{s}' is not finite"));
    }
    let unit = unit.trim();
    if unit.is_empty() {
        return Ok(number);
    }
    let table: &[(&str, f64)] = if metric.is_time() {
        &[
            ("ns", 1e-9),
            ("us", 1e-6),
            ("µs", 1e-6),
            ("ms", 1e-3),
            ("s", 1.0),
            ("min", 60.0),
        ]
    } else if metric == Metric::Memory {
        &[
            ("B", 1.0),
            ("KB", 1e3),
            ("KiB", 1024.0),
            ("MB", 1e6),
            ("MiB", 1024.0 * 1024.0),
            ("GB", 1e9),
            ("GiB", 1024.0 * 1024.0 * 1024.0),
        ]
    } else if metric == Metric::Energy {
        &[("uJ", 1e-6), ("mJ", 1e-3), ("J", 1.0), ("kJ", 1e3)]
    } else {
        &[]
    };
    table
        .iter()
        .find(|(name, _)| *name == unit)
        .map(|(_, factor)| number * factor)
        .ok_or_else(|| {
            if table.is_empty() {
                format!("metric '{metric}' takes a plain number, got unit '{unit}'")
            } else {
                format!(
                    "unknown unit '{unit}' for metric '{metric}' (one of {})",
                    table.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")
                )
            }
        })
}

/// `--objective minimize:energy`
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Objective {
    pub direction: Direction,
    pub axis: Axis,
}

impl Objective {
    pub fn parse(s: &str) -> Result<Self, String> {
        let (dir, axis) = s.split_once(':').ok_or_else(|| {
            format!("invalid objective '{s}': expected minimize:<METRIC> or maximize:<METRIC>")
        })?;
        let direction = match dir.trim().to_ascii_lowercase().as_str() {
            "minimize" | "min" => Direction::LowerIsBetter,
            "maximize" | "max" => Direction::HigherIsBetter,
            other => {
                return Err(format!(
                    "invalid objective '{s}': '{other}' is not minimize or maximize"
                ))
            }
        };
        Ok(Self {
            direction,
            axis: Axis::parse(axis).map_err(|e| format!("invalid objective '{s}': {e}"))?,
        })
    }
}

/// Settings of `--pareto`, `--constraint`, `--objective` and `--noise-threshold`.
#[derive(Debug, Clone, PartialEq)]
pub struct ParetoConfig {
    pub axes: Vec<Axis>,
    pub constraints: Vec<Constraint>,
    pub objective: Option<Objective>,
    /// Relative noise threshold (0.05 = 5%); 0 compares the values strictly
    pub noise_threshold: f64,
    /// `--pareto-significance`: a command is dominated only when the difference is
    /// significant on every metric where the dominator is better (#376)
    pub significance: bool,
    /// `--pareto-probability`: bootstrap probability of being non-dominated (#376)
    pub probability: bool,
}

impl ParetoConfig {
    /// Build the configuration from the raw option values; `None` when none of
    /// `--pareto`, `--constraint` and `--objective` is given.
    pub fn parse<'a>(
        pareto: Option<&str>,
        constraints: impl IntoIterator<Item = &'a str>,
        objective: Option<&str>,
        noise_threshold: Option<&str>,
    ) -> Result<Option<Self>, String> {
        Self::parse_with(
            pareto,
            constraints,
            objective,
            noise_threshold,
            false,
            false,
        )
    }

    /// [`parse`](Self::parse) with `--pareto-significance` and `--pareto-probability`
    pub fn parse_with<'a>(
        pareto: Option<&str>,
        constraints: impl IntoIterator<Item = &'a str>,
        objective: Option<&str>,
        noise_threshold: Option<&str>,
        significance: bool,
        probability: bool,
    ) -> Result<Option<Self>, String> {
        let constraints: Vec<Constraint> = constraints
            .into_iter()
            .map(Constraint::parse)
            .collect::<Result<_, _>>()?;
        let objective = objective.map(Objective::parse).transpose()?;
        let mut axes: Vec<Axis> = Vec::new();
        if let Some(list) = pareto {
            for item in list.split(',') {
                let axis = Axis::parse(item)?;
                if axes.iter().any(|a| a.name() == axis.name()) {
                    return Err(format!("duplicate metric '{}' in --pareto", axis.name()));
                }
                axes.push(axis);
            }
        }
        let noise_threshold = match noise_threshold {
            Some(s) => {
                let value: f64 = s
                    .trim()
                    .trim_end_matches('%')
                    .parse()
                    .ok()
                    .filter(|v: &f64| v.is_finite() && *v >= 0.0)
                    .ok_or_else(|| {
                        format!("invalid --noise-threshold '{s}': expected a percentage >= 0")
                    })?;
                value / 100.0
            }
            None => 0.0,
        };
        if axes.is_empty() {
            match (&objective, constraints.is_empty()) {
                (None, true) if significance || probability => {
                    return Err(
                        "--pareto-significance and --pareto-probability need --pareto or --objective"
                            .into(),
                    );
                }
                (None, true) => return Ok(None),
                (None, false) => {
                    return Err("--constraint needs --pareto or --objective".into());
                }
                (Some(o), _) => axes.push(o.axis),
            }
        }
        Ok(Some(Self {
            axes,
            constraints,
            objective,
            noise_threshold,
            significance,
            probability,
        }))
    }

    /// The configuration of an intermediate export: without the tests and the bootstrap,
    /// which only the final analysis runs
    pub fn without_uncertainty(&self) -> Self {
        Self {
            significance: false,
            probability: false,
            ..self.clone()
        }
    }

    /// The direction of the axis `i` of the frontier.
    fn directions(&self) -> Vec<Direction> {
        self.axes
            .iter()
            .map(|a| {
                match &self.objective {
                    // an explicit objective on the same axis overrides the default direction
                    Some(o) if o.axis == *a => o.direction,
                    _ => direction(a.metric),
                }
            })
            .collect()
    }
}

/// Why a command is not a candidate.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Excluded {
    pub command_index: usize,
    /// `metric_unavailable` or `constraint_violated`
    pub reason: &'static str,
    /// The metric without data, or the violated constraint as written
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Dominated {
    pub command_index: usize,
    pub by: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ObjectiveOutcome {
    pub direction: Direction,
    pub metric: String,
    /// `None` when no command is a candidate
    pub best: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
}

/// The `pareto` object of the JSON export.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Analysis {
    pub metrics: Vec<AxisInfo>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub noise_threshold: Option<f64>,
    pub frontier: Vec<usize>,
    pub dominated: Vec<Dominated>,
    /// On the frontier, but strictly dominated by a command within the noise threshold
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub within_noise: Vec<usize>,
    /// `--pareto-significance`: how the tests were made
    #[serde(skip_serializing_if = "Option::is_none")]
    pub significance: Option<SignificanceInfo>,
    /// `--pareto-significance`: on the frontier because the commands that are better in the
    /// estimates are not significantly better on every such metric (new in 1.37, #376)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub not_significant: Vec<Dominated>,
    /// `--pareto-probability`: resamples and seed of the bootstrap
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bootstrap: Option<BootstrapInfo>,
    /// `--pareto-probability`: share of the resamples in which a candidate is not dominated
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub non_dominated_probability: Vec<Probability>,
    pub excluded: Vec<Excluded>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub objective: Option<ObjectiveOutcome>,
    /// Value of every axis for each command, in `metrics` order; only for the
    /// terminal and Markdown tables
    #[serde(skip)]
    pub values: Vec<Vec<Option<f64>>>,
}

/// The tests behind `--pareto-significance`
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SignificanceInfo {
    /// `--significance-level`
    pub level: f64,
    /// `--correction` over the tests of this analysis, omitted without one
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correction: Option<&'static str>,
    /// Number of tests made (the family the correction applies to)
    pub tests: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BootstrapInfo {
    pub resamples: usize,
    pub seed: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Probability {
    pub command_index: usize,
    /// In [0, 1]
    pub probability: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AxisInfo {
    pub metric: String,
    pub direction: Direction,
}

/// +1: `a` is better than `b` on this axis, -1: worse, 0: tie within `noise` (relative to the
/// larger absolute value; 0 = strict)
fn compare_axis(a: f64, b: f64, dir: Direction, noise: f64) -> i8 {
    let diff = match dir {
        Direction::LowerIsBetter => b - a,
        Direction::HigherIsBetter => a - b,
    };
    let margin = noise * a.abs().max(b.abs());
    if diff > margin {
        1
    } else if diff < -margin {
        -1
    } else {
        0
    }
}

/// `a` dominates `b` if it is not worse on any axis and better on at least one, where
/// "better" and "worse" need a relative difference above `noise` (0 = strict).
fn dominates(a: &[f64], b: &[f64], dirs: &[Direction], noise: f64) -> bool {
    let mut better = false;
    for ((x, y), dir) in a.iter().zip(b).zip(dirs) {
        match compare_axis(*x, *y, *dir, noise) {
            -1 => return false,
            1 => better = true,
            _ => {}
        }
    }
    better
}

/// The axes on which `a` is better than `b` beyond `noise`
fn better_axes(a: &[f64], b: &[f64], dirs: &[Direction], noise: f64) -> Vec<usize> {
    (0..a.len().min(b.len()))
        .filter(|&k| compare_axis(a[k], b[k], dirs[k], noise) == 1)
        .collect()
}

/// Split of `candidates` (rows of oriented axis values) into the Pareto frontier and
/// the dominated points. Pure, for unit tests.
///
/// Returns, per candidate: its dominators (beyond noise) and its strict dominators.
fn dominators(values: &[&[f64]], dirs: &[Direction], noise: f64) -> Vec<(Vec<usize>, Vec<usize>)> {
    (0..values.len())
        .map(|b| {
            let by = |n: f64| -> Vec<usize> {
                (0..values.len())
                    .filter(|&a| a != b && dominates(values[a], values[b], dirs, n))
                    .collect()
            };
            (by(noise), if noise > 0.0 { by(0.0) } else { Vec::new() })
        })
        .collect()
}

/// Two-sided p-value of the difference of `axis` between two results, with the settings of
/// #320 (resamples, seed) and the estimator that matches the statistic of the axis. `None`
/// when a result has fewer than 3 samples of the metric. The variance of a `--subtract`
/// baseline is part of the test for the mean of the wall time, as for the significance markers.
fn axis_p_value(
    axis: &Axis,
    a: &BenchmarkResult,
    b: &BenchmarkResult,
    settings: &StatSettings,
) -> Option<f64> {
    let (xs, ys) = (
        a.primary_samples(axis.metric)?,
        b.primary_samples(axis.metric)?,
    );
    let settings = StatSettings {
        estimator: axis.statistic.estimator(),
        ..settings.clone()
    };
    let baseline_variance = |r: &BenchmarkResult| {
        if axis.metric == Metric::Wall {
            r.baseline
                .as_ref()
                .and_then(|sub| sub.stddev.map(|sd| sd.powi(2) / sub.runs.max(1) as f64))
                .unwrap_or(0.0)
        } else {
            0.0
        }
    };
    let (var_a, var_b) = (baseline_variance(a), baseline_variance(b));
    if let Some(stats) =
        crate::stats::deep::compare_samples_with_settings(&xs, &ys, var_a, var_b, &settings)
    {
        return Some(stats.p_value);
    }
    let constant = |v: &[f64]| v.len() >= 3 && v.iter().all(|&x| x == v[0]);
    (var_a == 0.0 && var_b == 0.0 && constant(&xs) && constant(&ys) && xs[0] != ys[0])
        .then_some(0.0)
}

/// `--pareto-significance`: keep, per candidate, the dominators whose advantage is significant
/// on every axis where they are better beyond the noise threshold. The p-values of all
/// those tests form one family for `--correction`. Returns the kept dominators (positions
/// among the candidates) and the number of tests.
fn significant_dominators(
    config: &ParetoConfig,
    settings: &StatSettings,
    results: &[&BenchmarkResult],
    rows: &[&[f64]],
    dirs: &[Direction],
    point: &[Vec<usize>],
) -> (Vec<Vec<usize>>, usize) {
    // one entry per (dominator, dominated, axis)
    let mut tests: Vec<(usize, usize, usize)> = Vec::new();
    for (b, by) in point.iter().enumerate() {
        for &a in by {
            for k in better_axes(rows[a], rows[b], dirs, config.noise_threshold) {
                tests.push((a, b, k));
            }
        }
    }
    let raw: Vec<f64> = tests
        .iter()
        .map(|&(a, b, k)| {
            axis_p_value(&config.axes[k], results[a], results[b], settings).unwrap_or(f64::NAN)
        })
        .collect();
    let adjusted = crate::stats::correction::adjust(&raw, settings.correction);
    let mut kept: Vec<Vec<usize>> = point.to_vec();
    for (&(a, b, _), p) in tests.iter().zip(&adjusted) {
        // a NaN p-value (too few samples) never shows a difference
        if p.is_nan() || *p >= settings.significance_level {
            kept[b].retain(|&x| x != a);
        }
    }
    (kept, raw.iter().filter(|p| !p.is_nan()).count())
}

/// `--pareto-probability`: the share of `resamples` bootstrap replicates in which each
/// candidate is not dominated. Each candidate's runs are resampled together (the same
/// run index for every metric, so that time and energy stay correlated) when all its
/// metrics have the same number of samples, otherwise metric by metric; a metric without
/// per-run samples keeps its value. Dominance is that of the point estimates (with the noise
/// threshold), in every replicate.
fn non_dominated_probability(
    config: &ParetoConfig,
    settings: &StatSettings,
    results: &[&BenchmarkResult],
    rows: &[&[f64]],
    dirs: &[Direction],
) -> Vec<f64> {
    use rand::{Rng, SeedableRng};
    let samples: Vec<Vec<Option<Vec<f64>>>> = results
        .iter()
        .map(|r| {
            config
                .axes
                .iter()
                .map(|axis| {
                    r.primary_samples(axis.metric)
                        .map(|mut v| {
                            v.retain(|x| x.is_finite());
                            v
                        })
                        .filter(|v| !v.is_empty())
                })
                .collect()
        })
        .collect();
    let mut rng = rand::rngs::StdRng::seed_from_u64(settings.seed);
    let resamples = settings.bootstrap_resamples.max(1);
    let n = rows.len();
    let mut free = vec![0usize; n];
    let mut point = vec![vec![0.0; config.axes.len()]; n];
    let mut scratch: Vec<f64> = Vec::new();
    for _ in 0..resamples {
        for c in 0..n {
            let lengths: Vec<usize> = samples[c].iter().flatten().map(Vec::len).collect();
            let joint = lengths.first().filter(|l| lengths.iter().all(|x| x == *l));
            let joint_indices: Option<Vec<usize>> =
                joint.map(|&len| (0..len).map(|_| rng.gen_range(0..len)).collect());
            for (k, axis) in config.axes.iter().enumerate() {
                point[c][k] = match &samples[c][k] {
                    None => rows[c][k],
                    Some(values) => {
                        scratch.clear();
                        match &joint_indices {
                            Some(indices) => scratch.extend(indices.iter().map(|&i| values[i])),
                            None => scratch.extend(
                                (0..values.len()).map(|_| values[rng.gen_range(0..values.len())]),
                            ),
                        }
                        axis.statistic.of(&scratch).unwrap_or(rows[c][k])
                    }
                };
            }
        }
        for b in 0..n {
            let dominated = (0..n)
                .any(|a| a != b && dominates(&point[a], &point[b], dirs, config.noise_threshold));
            if !dominated {
                free[b] += 1;
            }
        }
    }
    free.iter().map(|&c| c as f64 / resamples as f64).collect()
}

/// Analyse the results with the installed statistics settings. Command indices refer to
/// positions in `results`.
pub fn analyze(config: &ParetoConfig, results: &[BenchmarkResult]) -> Analysis {
    analyze_with(config, results, &crate::stats::settings::current())
}

/// [`analyze`] with explicit statistics settings (significance level, correction,
/// bootstrap resamples and seed, used by `--pareto-significance` and `--pareto-probability`)
pub fn analyze_with(
    config: &ParetoConfig,
    results: &[BenchmarkResult],
    settings: &StatSettings,
) -> Analysis {
    let dirs = config.directions();
    let mut excluded = Vec::new();
    let mut candidates: Vec<usize> = Vec::new();
    let mut values: Vec<Vec<Option<f64>>> = Vec::with_capacity(results.len());
    let mut candidate_values: Vec<Vec<f64>> = Vec::new();
    let mut objective_values: Vec<f64> = Vec::new();

    for (index, result) in results.iter().enumerate() {
        let axis_values: Vec<Option<f64>> = config.axes.iter().map(|a| a.value(result)).collect();
        let mut reason: Option<(&'static str, String)> = None;
        for c in &config.constraints {
            match c.axis.value(result) {
                None => {
                    reason = Some(("metric_unavailable", c.axis.name()));
                    break;
                }
                Some(v) if !c.op.holds(v, c.value) => {
                    reason = Some(("constraint_violated", c.text.clone()));
                    break;
                }
                Some(_) => {}
            }
        }
        if reason.is_none() {
            if let Some(i) = axis_values.iter().position(Option::is_none) {
                reason = Some(("metric_unavailable", config.axes[i].name()));
            }
        }
        let objective_value = config.objective.and_then(|o| o.axis.value(result));
        if reason.is_none() && config.objective.is_some() && objective_value.is_none() {
            let name = config.objective.map(|o| o.axis.name()).unwrap_or_default();
            reason = Some(("metric_unavailable", name));
        }
        match reason {
            Some((reason, detail)) => excluded.push(Excluded {
                command_index: index,
                reason,
                detail,
            }),
            None => {
                candidates.push(index);
                candidate_values.push(axis_values.iter().map(|v| v.unwrap_or(0.0)).collect());
                objective_values.push(objective_value.unwrap_or(0.0));
            }
        }
        values.push(axis_values);
    }

    let rows: Vec<&[f64]> = candidate_values.iter().map(Vec::as_slice).collect();
    let doms = dominators(&rows, &dirs, config.noise_threshold);
    let candidate_results: Vec<&BenchmarkResult> =
        candidates.iter().map(|&i| &results[i]).collect();
    let point: Vec<Vec<usize>> = doms.iter().map(|(by, _)| by.clone()).collect();
    let (decisive, tests) = if config.significance {
        significant_dominators(config, settings, &candidate_results, &rows, &dirs, &point)
    } else {
        (point.clone(), 0)
    };
    let mut frontier = Vec::new();
    let mut dominated = Vec::new();
    let mut within_noise = Vec::new();
    let mut not_significant = Vec::new();
    for (pos, (_, strict)) in doms.iter().enumerate() {
        let index = candidates[pos];
        let to_indices = |by: &[usize]| by.iter().map(|&p| candidates[p]).collect::<Vec<_>>();
        if decisive[pos].is_empty() {
            frontier.push(index);
            if !point[pos].is_empty() {
                not_significant.push(Dominated {
                    command_index: index,
                    by: to_indices(&point[pos]),
                });
            } else if !strict.is_empty() {
                within_noise.push(index);
            }
        } else {
            dominated.push(Dominated {
                command_index: index,
                by: to_indices(&decisive[pos]),
            });
        }
    }
    let probabilities = if config.probability && !candidates.is_empty() {
        let p = non_dominated_probability(config, settings, &candidate_results, &rows, &dirs);
        candidates
            .iter()
            .zip(p)
            .map(|(&command_index, probability)| Probability {
                command_index,
                probability,
            })
            .collect()
    } else {
        Vec::new()
    };

    let objective = config.objective.map(|o| {
        let mut best: Option<(usize, f64)> = None;
        for (pos, &v) in objective_values.iter().enumerate() {
            let better = match (best, o.direction) {
                (None, _) => true,
                (Some((_, b)), Direction::LowerIsBetter) => v < b,
                (Some((_, b)), Direction::HigherIsBetter) => v > b,
            };
            if better {
                best = Some((pos, v));
            }
        }
        ObjectiveOutcome {
            direction: o.direction,
            metric: o.axis.name(),
            best: best.map(|(pos, _)| candidates[pos]),
            value: best.map(|(_, v)| v),
        }
    });

    Analysis {
        metrics: config
            .axes
            .iter()
            .zip(&dirs)
            .map(|(a, d)| AxisInfo {
                metric: a.name(),
                direction: *d,
            })
            .collect(),
        constraints: config.constraints.iter().map(|c| c.text.clone()).collect(),
        noise_threshold: (config.noise_threshold > 0.0).then_some(config.noise_threshold),
        frontier,
        dominated,
        within_noise,
        significance: config.significance.then(|| SignificanceInfo {
            level: settings.significance_level,
            correction: (!settings.correction.is_none()).then(|| settings.correction.name()),
            tests,
        }),
        not_significant,
        bootstrap: (!probabilities.is_empty()).then(|| BootstrapInfo {
            resamples: settings.bootstrap_resamples.max(1),
            seed: settings.seed,
        }),
        non_dominated_probability: probabilities,
        excluded,
        objective,
        values,
    }
}

pub(crate) fn format_value(metric: Metric, value: f64) -> String {
    match metric {
        m if m.is_time() => format_duration(value, None),
        Metric::Energy => format!("{value:.3} J"),
        Metric::Memory => format!("{value:.0} B"),
        Metric::Instructions | Metric::Cycles => format!("{value:.0}"),
        _ => format!("{value:.4}"),
    }
}

fn status(analysis: &Analysis, results: &[BenchmarkResult], index: usize) -> String {
    let name = |i: usize| results[i].command.clone();
    if let Some(e) = analysis.excluded.iter().find(|e| e.command_index == index) {
        return match e.reason {
            "constraint_violated" => format!("excluded: violates '{}'", e.detail),
            _ => format!("excluded: no data for {}", e.detail),
        };
    }
    if let Some(d) = analysis.dominated.iter().find(|d| d.command_index == index) {
        let by: Vec<String> = d.by.iter().map(|&i| name(i)).collect();
        return format!("dominated by {}", by.join(", "));
    }
    let mut text = if analysis
        .not_significant
        .iter()
        .any(|d| d.command_index == index)
    {
        "not dominated (difference not significant)".to_string()
    } else if analysis.within_noise.contains(&index) {
        "not dominated (within noise)".to_string()
    } else {
        "frontier".to_string()
    };
    if analysis.objective.as_ref().and_then(|o| o.best) == Some(index) {
        text.push_str(", best objective");
    }
    text
}

fn header_line(analysis: &Analysis) -> String {
    let metrics: Vec<String> = analysis
        .metrics
        .iter()
        .map(|m| {
            let arrow = match m.direction {
                Direction::LowerIsBetter => "↓",
                Direction::HigherIsBetter => "↑",
            };
            format!("{}{arrow}", m.metric)
        })
        .collect();
    let mut line = format!("Pareto frontier over {}", metrics.join(", "));
    if !analysis.constraints.is_empty() {
        let _ = write!(
            line,
            " with constraints {}",
            analysis.constraints.join("; ")
        );
    }
    if let Some(s) = &analysis.significance {
        let _ = write!(line, ", significant differences only (alpha = {}", s.level);
        if let Some(c) = s.correction {
            let _ = write!(line, ", {c} over {} tests", s.tests);
        }
        line.push(')');
    }
    line
}

fn objective_line(analysis: &Analysis, results: &[BenchmarkResult]) -> Option<String> {
    let o = analysis.objective.as_ref()?;
    let verb = match o.direction {
        Direction::LowerIsBetter => "minimize",
        Direction::HigherIsBetter => "maximize",
    };
    Some(match o.best {
        Some(i) => format!("Objective {verb} {}: {}", o.metric, results[i].command),
        None => format!(
            "Objective {verb} {}: no command satisfies the constraints",
            o.metric
        ),
    })
}

fn cells(
    config: &ParetoConfig,
    analysis: &Analysis,
    results: &[BenchmarkResult],
) -> Vec<Vec<String>> {
    results
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let mut row = vec![r.command.clone()];
            for (axis, value) in config.axes.iter().zip(&analysis.values[i]) {
                row.push(match value {
                    Some(v) => format_value(axis.metric, *v),
                    None => "n/a".into(),
                });
            }
            if !analysis.non_dominated_probability.is_empty() {
                row.push(
                    analysis
                        .non_dominated_probability
                        .iter()
                        .find(|p| p.command_index == i)
                        .map_or("-".to_string(), |p| {
                            format!("{:.1}%", p.probability * 100.0)
                        }),
                );
            }
            row.push(status(analysis, results, i));
            row
        })
        .collect()
}

/// Column titles of the table: command, metrics, `P(non-dominated)` if computed, status
fn head(config: &ParetoConfig, analysis: &Analysis) -> Vec<String> {
    let mut head = vec!["Command".to_string()];
    head.extend(config.axes.iter().map(Axis::name));
    if !analysis.non_dominated_probability.is_empty() {
        head.push("P(non-dominated)".into());
    }
    head.push("Status".into());
    head
}

/// Terminal section; empty without results.
pub fn terminal(config: &ParetoConfig, analysis: &Analysis, results: &[BenchmarkResult]) -> String {
    if results.is_empty() {
        return String::new();
    }
    let mut table = vec![head(config, analysis)];
    table.extend(cells(config, analysis, results));
    let columns = table[0].len();
    let widths: Vec<usize> = (0..columns)
        .map(|c| {
            table
                .iter()
                .map(|r| r[c].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut out = format!("{}\n", header_line(analysis));
    for (n, row) in table.iter().enumerate() {
        let mut line = String::from("  ");
        for (c, cell) in row.iter().enumerate() {
            line.push_str(cell);
            line.push_str(&" ".repeat(widths[c] - cell.chars().count() + 2));
        }
        out.push_str(line.trim_end());
        out.push('\n');
        if n == 0 {
            out.push_str(&format!(
                "  {}\n",
                "─".repeat(widths.iter().sum::<usize>() + 2 * columns)
            ));
        }
    }
    if let Some(line) = objective_line(analysis, results) {
        let _ = writeln!(out, "  {line}");
    }
    out.push('\n');
    out
}

fn escape_md(s: &str) -> String {
    s.replace('|', "\\|")
}

/// Markdown section appended to the Markdown export.
pub fn markdown(config: &ParetoConfig, analysis: &Analysis, results: &[BenchmarkResult]) -> String {
    if results.is_empty() {
        return String::new();
    }
    let mut out = format!("\n### {}\n\n", escape_md(&header_line(analysis)));
    let head = head(config, analysis);
    let _ = writeln!(out, "| {} |", head.join(" | "));
    let mut aligns = vec![":---"];
    aligns.extend(config.axes.iter().map(|_| "---:"));
    if !analysis.non_dominated_probability.is_empty() {
        aligns.push("---:");
    }
    aligns.push(":---");
    let _ = writeln!(out, "|{}|", aligns.join("|"));
    for row in cells(config, analysis, results) {
        let row: Vec<String> = row.iter().map(|c| escape_md(c)).collect();
        let _ = writeln!(out, "| {} |", row.join(" | "));
    }
    if let Some(line) = objective_line(analysis, results) {
        let _ = write!(out, "\n{}\n", escape_md(&line));
    }
    out
}

/// Marker for the summary table of the HTML export: `★ frontier` or nothing.
pub fn is_on_frontier(analysis: &Analysis, index: usize) -> bool {
    analysis.frontier.contains(&index)
}

/// HTML section (escaped by the caller-supplied `escape`).
pub fn html(
    config: &ParetoConfig,
    analysis: &Analysis,
    results: &[BenchmarkResult],
    escape: &dyn Fn(&str) -> String,
) -> String {
    if results.is_empty() {
        return String::new();
    }
    let mut out = format!(
        "<h2>{}</h2>\n<table><thead><tr><th>Command</th>",
        escape(&header_line(analysis))
    );
    for title in head(config, analysis).iter().skip(1) {
        let _ = write!(out, "<th>{}</th>", escape(title));
    }
    out.push_str("</tr></thead><tbody>");
    for (i, row) in cells(config, analysis, results).iter().enumerate() {
        let class = if is_on_frontier(analysis, i) {
            " class=\"frontier\""
        } else {
            ""
        };
        let _ = write!(out, "<tr{class}>");
        for (c, cell) in row.iter().enumerate() {
            if c == 0 {
                let _ = write!(out, "<td><code>{}</code></td>", escape(cell));
            } else {
                let _ = write!(out, "<td>{}</td>", escape(cell));
            }
        }
        out.push_str("</tr>");
    }
    out.push_str("</tbody></table>\n");
    if let Some(line) = objective_line(analysis, results) {
        let _ = writeln!(out, "<p>{}</p>", escape(&line));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use Direction::{HigherIsBetter, LowerIsBetter};

    fn frontier_of(rows: &[[f64; 3]], dirs: &[Direction], noise: f64) -> Vec<usize> {
        let slices: Vec<&[f64]> = rows.iter().map(|r| r.as_slice()).collect();
        dominators(&slices, dirs, noise)
            .iter()
            .enumerate()
            .filter(|(_, (by, _))| by.is_empty())
            .map(|(i, _)| i)
            .collect()
    }

    /// time (ms), energy (J), memory (MB) of rust, c++, go, java, python
    const FIXTURE: [[f64; 3]; 5] = [
        [10.0, 1.2, 60.0],
        [8.0, 2.0, 80.0],
        [14.0, 0.9, 45.0],
        [19.0, 2.5, 140.0],
        [24.0, 3.1, 35.0],
    ];

    #[test]
    fn five_implementation_frontier() {
        // java is worse than rust in all three; python has the least memory; the rest trade off
        assert_eq!(
            frontier_of(&FIXTURE, &[LowerIsBetter; 3], 0.0),
            vec![0, 1, 2, 4]
        );
        // time and energy only: python (24 ms, 3.1 J) is dominated by everything
        let two: Vec<[f64; 3]> = FIXTURE.to_vec();
        let slices: Vec<&[f64]> = two.iter().map(|r| &r[..2]).collect();
        let d = dominators(&slices, &[LowerIsBetter; 2], 0.0);
        let frontier: Vec<usize> = (0..5).filter(|&i| d[i].0.is_empty()).collect();
        assert_eq!(frontier, vec![0, 1, 2]);
        assert_eq!(d[3].0, vec![0, 1, 2]);
    }

    #[test]
    fn higher_is_better_flips_the_axis() {
        // throughput (higher) vs energy (lower)
        let rows = [[100.0, 5.0, 0.0], [200.0, 5.0, 0.0], [150.0, 9.0, 0.0]];
        let dirs = [HigherIsBetter, LowerIsBetter, LowerIsBetter];
        assert_eq!(frontier_of(&rows, &dirs, 0.0), vec![1]);
        assert_eq!(frontier_of(&rows, &[LowerIsBetter; 3], 0.0), vec![0]);
    }

    #[test]
    fn equal_points_do_not_dominate_each_other() {
        let rows = [[1.0, 1.0, 1.0], [1.0, 1.0, 1.0]];
        assert_eq!(frontier_of(&rows, &[LowerIsBetter; 3], 0.0), vec![0, 1]);
    }

    #[test]
    fn noise_threshold_keeps_close_points_on_the_frontier() {
        // b is 2% slower and equal elsewhere: strictly dominated, not beyond 5% noise
        let rows = [[100.0, 1.0, 1.0], [102.0, 1.0, 1.0]];
        let slices: Vec<&[f64]> = rows.iter().map(|r| r.as_slice()).collect();
        let strict = dominators(&slices, &[LowerIsBetter; 3], 0.0);
        assert_eq!(strict[1].0, vec![0]);
        let noisy = dominators(&slices, &[LowerIsBetter; 3], 0.05);
        assert!(noisy[1].0.is_empty());
        assert_eq!(noisy[1].1, vec![0], "strict dominator is still reported");
        // 20% slower is beyond the noise
        let rows = [[100.0, 1.0, 1.0], [120.0, 1.0, 1.0]];
        let slices: Vec<&[f64]> = rows.iter().map(|r| r.as_slice()).collect();
        assert_eq!(dominators(&slices, &[LowerIsBetter; 3], 0.05)[1].0, vec![0]);
    }

    #[test]
    fn five_hundred_commands_are_fast() {
        let rows: Vec<[f64; 3]> = (0..500)
            .map(|i| {
                let x = i as f64;
                [x, 500.0 - x, (x * 7.0) % 97.0]
            })
            .collect();
        let slices: Vec<&[f64]> = rows.iter().map(|r| r.as_slice()).collect();
        let start = std::time::Instant::now();
        let d = dominators(&slices, &[LowerIsBetter; 3], 0.0);
        assert!(start.elapsed().as_secs() < 5);
        assert_eq!(d.len(), 500);
        // time and energy anti-correlate, so nothing is dominated on them
        assert!(d.iter().filter(|(by, _)| by.is_empty()).count() >= 90);
    }

    #[test]
    fn parses_axes_and_constraints() {
        assert_eq!(Axis::parse("wall").unwrap().name(), "wall");
        assert_eq!(Axis::parse("wall.p95").unwrap().name(), "wall.p95");
        assert_eq!(Axis::parse("memory.MAX").unwrap().name(), "memory.max");
        assert_eq!(Axis::parse("custom:ops").unwrap().name(), "custom:ops");
        assert!(Axis::parse("wall.p101").is_err());
        assert!(Axis::parse("bogus").is_err());

        let c = Constraint::parse("memory <= 256MiB").unwrap();
        assert_eq!((c.op, c.value), (Op::Le, 256.0 * 1024.0 * 1024.0));
        let c = Constraint::parse("wall.p95<20ms").unwrap();
        assert_eq!((c.op, c.axis.name()), (Op::Lt, "wall.p95".to_string()));
        assert!((c.value - 0.02).abs() < 1e-12);
        let c = Constraint::parse("energy >= 1.5").unwrap();
        assert_eq!((c.op, c.value), (Op::Ge, 1.5));
        assert_eq!(Constraint::parse("energy > 2mJ").unwrap().value, 0.002);
        assert!(Constraint::parse("wall 20ms").is_err());
        assert!(Constraint::parse("wall <= fast").is_err());
        assert!(Constraint::parse("wall <= 5MiB").is_err());
        assert!(Constraint::parse("instructions <= 5ms").is_err());
    }

    #[test]
    fn config_rules() {
        assert_eq!(ParetoConfig::parse(None, [], None, None).unwrap(), None);
        assert!(ParetoConfig::parse(None, ["wall<=1s"], None, None).is_err());
        assert!(ParetoConfig::parse(Some("wall,wall"), [], None, None).is_err());
        // the threshold also serves the statistics (#323): not an error without --pareto
        assert_eq!(
            ParetoConfig::parse(None, [], None, Some("5")).unwrap(),
            None
        );
        assert!(ParetoConfig::parse(None, [], None, Some("-5")).is_err());
        let c = ParetoConfig::parse(None, [], Some("maximize:custom:ops"), None)
            .unwrap()
            .unwrap();
        assert_eq!(c.axes.len(), 1);
        assert_eq!(c.directions(), vec![HigherIsBetter]);
        let c = ParetoConfig::parse(Some("wall,energy"), [], None, Some("5%"))
            .unwrap()
            .unwrap();
        assert_eq!(c.noise_threshold, 0.05);
        assert!(ParetoConfig::parse(Some("wall"), [], None, Some("-1")).is_err());
    }

    fn result(name: &str, wall_ms: f64, energy: Option<f64>, mem_mb: u64) -> BenchmarkResult {
        let wall = wall_ms / 1000.0;
        BenchmarkResult {
            command: name.into(),
            mean: wall,
            times: Some(vec![wall * 0.9, wall, wall * 1.1]),
            energy_joules: energy.map(|e| vec![e; 3]),
            mean_energy_joules: energy,
            memory_usage_byte: Some(vec![mem_mb * 1_000_000; 3]),
            ..Default::default()
        }
    }

    fn fixture() -> Vec<BenchmarkResult> {
        FIXTURE
            .iter()
            .zip(["rust", "c++", "go", "java", "python"])
            .map(|(r, n)| result(n, r[0], Some(r[1]), r[2] as u64))
            .collect()
    }

    fn config(pareto: &str, constraints: &[&str], objective: Option<&str>) -> ParetoConfig {
        ParetoConfig::parse(Some(pareto), constraints.iter().copied(), objective, None)
            .unwrap()
            .unwrap()
    }

    #[test]
    fn analysis_of_the_fixture() {
        let results = fixture();
        let a = analyze(&config("wall,energy,memory", &[], None), &results);
        assert_eq!(a.frontier, vec![0, 1, 2, 4]);
        assert_eq!(
            a.dominated,
            vec![Dominated {
                command_index: 3,
                by: vec![0, 1, 2]
            }]
        );
        assert!(a.excluded.is_empty());
        let a = analyze(&config("wall,energy", &[], None), &results);
        assert_eq!(a.frontier, vec![0, 1, 2]);
    }

    #[test]
    fn constraints_filter_candidates_before_dominance() {
        let results = fixture();
        // c++ (80 MB) and java (140 MB) are out; without c++, rust dominates nothing new
        let a = analyze(&config("wall,energy", &["memory <= 70MB"], None), &results);
        assert_eq!(
            a.excluded
                .iter()
                .map(|e| (e.command_index, e.reason))
                .collect::<Vec<_>>(),
            vec![(1, "constraint_violated"), (3, "constraint_violated")]
        );
        assert_eq!(a.frontier, vec![0, 2]);
        assert_eq!(
            a.dominated
                .iter()
                .map(|d| d.command_index)
                .collect::<Vec<_>>(),
            vec![4]
        );
        // a percentile constraint
        let a = analyze(&config("wall", &["wall.p95 <= 20ms"], None), &results);
        assert_eq!(
            a.excluded
                .iter()
                .map(|e| e.command_index)
                .collect::<Vec<_>>(),
            vec![3, 4]
        );
    }

    #[test]
    fn unavailable_metrics_exclude_instead_of_counting_as_zero() {
        let mut results = fixture();
        results[2].energy_joules = None;
        results[2].mean_energy_joules = None;
        let a = analyze(&config("wall,energy", &[], None), &results);
        assert_eq!(
            a.excluded,
            vec![Excluded {
                command_index: 2,
                reason: "metric_unavailable",
                detail: "energy".into()
            }]
        );
        // go would be the best at 0 J; it must not be on the frontier
        assert!(!a.frontier.contains(&2));
        assert_eq!(a.values[2][1], None);
    }

    #[test]
    fn objective_picks_the_best_candidate() {
        let results = fixture();
        let a = analyze(
            &config("wall,energy", &["memory <= 70MB"], Some("minimize:energy")),
            &results,
        );
        let o = a.objective.unwrap();
        assert_eq!((o.best, o.value), (Some(2), Some(0.9)));
        let a = analyze(&config("wall,energy", &[], Some("maximize:wall")), &results);
        assert_eq!(a.objective.unwrap().best, Some(4));
        let a = analyze(
            &config("wall", &["wall <= 1ms"], Some("minimize:wall")),
            &results,
        );
        assert_eq!(a.objective.unwrap().best, None);
    }

    #[test]
    fn within_noise_is_reported() {
        let mut results = fixture();
        // 2% slower than rust and no better anywhere
        results.push(result("rust2", 10.2, Some(1.2), 60));
        let strict = analyze(&config("wall,energy,memory", &[], None), &results);
        assert!(strict.dominated.iter().any(|d| d.command_index == 5));
        let noisy = ParetoConfig::parse(Some("wall,energy,memory"), [], None, Some("5"))
            .unwrap()
            .unwrap();
        let noisy = analyze(&noisy, &results);
        assert!(noisy.frontier.contains(&5));
        assert_eq!(noisy.within_noise, vec![5]);
        assert_eq!(noisy.noise_threshold, Some(0.05));
    }

    #[test]
    fn markdown_and_json_shapes() {
        let results = fixture();
        let cfg = config("wall,energy", &["memory <= 100MB"], Some("minimize:energy"));
        let a = analyze(&cfg, &results);
        let md = markdown(&cfg, &a, &results);
        assert!(
            md.contains("### Pareto frontier over wall↓, energy↓ with constraints memory <= 100MB"),
            "{md}"
        );
        assert!(md.contains("| Command | wall | energy | Status |"), "{md}");
        assert!(
            md.contains("| go | 14.0 ms | 0.900 J | frontier, best objective |"),
            "{md}"
        );
        assert!(
            md.contains("| java | 19.0 ms | 2.500 J | excluded: violates 'memory <= 100MB' |"),
            "{md}"
        );
        assert!(
            md.contains("| python | 24.0 ms | 3.100 J | dominated by rust, c++, go |"),
            "{md}"
        );
        let json = serde_json::to_value(&a).unwrap();
        assert_eq!(json["frontier"], serde_json::json!([0, 1, 2]));
        assert_eq!(
            json["metrics"][0],
            serde_json::json!({"metric": "wall", "direction": "lower_is_better"})
        );
        assert_eq!(json["excluded"][0]["reason"], "constraint_violated");
        assert!(json.get("values").is_none());
        assert!(terminal(&cfg, &a, &results).contains("Objective minimize energy: go"));
        assert!(html(&cfg, &a, &results, &|s| s.replace('<', "&lt;"))
            .contains("<tr class=\"frontier\">"));
    }

    // ----- uncertainty (#376) -----

    /// `n` runs of a command whose wall time is N(wall_ms, wall_sd) and energy N(energy, energy_sd)
    fn noisy(
        name: &str,
        (wall_ms, wall_sd): (f64, f64),
        (energy, energy_sd): (f64, f64),
        n: usize,
        seed: u64,
    ) -> BenchmarkResult {
        use rand::{Rng, SeedableRng};
        let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
        let mut normal = |mean: f64, sd: f64| {
            let z: f64 = (0..12).map(|_| rng.gen::<f64>()).sum::<f64>() - 6.0;
            mean + sd * z
        };
        let times: Vec<f64> = (0..n).map(|_| normal(wall_ms, wall_sd) / 1000.0).collect();
        let energies: Vec<f64> = (0..n).map(|_| normal(energy, energy_sd)).collect();
        BenchmarkResult {
            command: name.into(),
            mean: times.iter().sum::<f64>() / n as f64,
            times: Some(times),
            mean_energy_joules: Some(energies.iter().sum::<f64>() / n as f64),
            energy_joules: Some(energies),
            ..Default::default()
        }
    }

    fn uncertain(significance: bool, probability: bool, noise: Option<&str>) -> ParetoConfig {
        ParetoConfig::parse_with(
            Some("wall,energy"),
            [],
            None,
            noise,
            significance,
            probability,
        )
        .unwrap()
        .unwrap()
    }

    fn settings() -> StatSettings {
        StatSettings {
            bootstrap_resamples: 1000,
            ..StatSettings::default()
        }
    }

    #[test]
    fn noisy_candidates_stay_on_the_frontier() {
        // b is 3% slower and 3% hungrier than a, but the runs scatter by 30%
        let results = vec![
            noisy("a", (10.0, 3.0), (1.0, 0.3), 20, 1),
            noisy("b", (10.3, 3.0), (1.03, 0.3), 20, 2),
        ];
        // in the estimates a dominates b (checked, so that the test means something)
        let strict = analyze_with(&uncertain(false, false, None), &results, &settings());
        assert_eq!(strict.frontier, vec![0], "{strict:?}");
        let analysis = analyze_with(&uncertain(true, false, None), &results, &settings());
        assert_eq!(analysis.frontier, vec![0, 1]);
        assert!(analysis.dominated.is_empty());
        assert_eq!(
            analysis.not_significant,
            vec![Dominated {
                command_index: 1,
                by: vec![0]
            }]
        );
        assert_eq!(analysis.significance.as_ref().unwrap().tests, 2);
        assert!(status(&analysis, &results, 1).contains("not significant"));
    }

    #[test]
    fn a_clear_difference_still_dominates() {
        let results = vec![
            noisy("a", (10.0, 0.3), (1.0, 0.03), 20, 3),
            noisy("b", (13.0, 0.3), (1.3, 0.03), 20, 4),
        ];
        let analysis = analyze_with(&uncertain(true, false, None), &results, &settings());
        assert_eq!(analysis.frontier, vec![0]);
        assert_eq!(analysis.dominated[0].command_index, 1);
        assert!(analysis.not_significant.is_empty());
    }

    #[test]
    fn every_metric_where_the_dominator_is_better_must_be_significant() {
        // a is clearly faster; its energy is better by noise only
        let results = vec![
            noisy("a", (10.0, 0.3), (1.0, 0.4), 20, 5),
            noisy("b", (13.0, 0.3), (1.02, 0.4), 20, 6),
        ];
        let strict = analyze_with(&uncertain(false, false, None), &results, &settings());
        assert_eq!(strict.frontier, vec![0], "a dominates b in the estimates");
        let analysis = analyze_with(&uncertain(true, false, None), &results, &settings());
        assert_eq!(analysis.frontier, vec![0, 1]);
        assert_eq!(analysis.not_significant.len(), 1);
    }

    #[test]
    fn a_metric_without_a_difference_does_not_block_domination() {
        // identical memory (a tie in the estimates) is not a metric a is better on
        let mut results = vec![
            noisy("a", (10.0, 0.3), (1.0, 0.03), 20, 7),
            noisy("b", (13.0, 0.3), (1.0, 0.03), 20, 8),
        ];
        for r in &mut results {
            r.energy_joules = Some(vec![1.0; 20]);
            r.mean_energy_joules = Some(1.0);
        }
        let analysis = analyze_with(&uncertain(true, false, None), &results, &settings());
        assert_eq!(analysis.frontier, vec![0]);
    }

    #[test]
    fn too_few_runs_never_show_a_difference() {
        let results = vec![
            noisy("a", (10.0, 0.0), (1.0, 0.0), 2, 9),
            noisy("b", (20.0, 0.0), (2.0, 0.0), 2, 10),
        ];
        let analysis = analyze_with(&uncertain(true, false, None), &results, &settings());
        assert_eq!(analysis.frontier, vec![0, 1]);
        assert_eq!(analysis.significance.unwrap().tests, 0);
    }

    #[test]
    fn the_correction_covers_the_tests_of_the_analysis() {
        // five commands, each slower than the previous by a clear margin: many tests
        let results: Vec<BenchmarkResult> = (0..5)
            .map(|i| {
                noisy(
                    &format!("c{i}"),
                    (10.0 + i as f64 * 2.0, 0.3),
                    (1.0 + i as f64 * 0.2, 0.03),
                    20,
                    20 + i,
                )
            })
            .collect();
        let corrected = StatSettings {
            correction: crate::stats::correction::Correction::Holm,
            ..settings()
        };
        let analysis = analyze_with(&uncertain(true, false, None), &results, &corrected);
        let info = analysis.significance.unwrap();
        assert_eq!(info.correction, Some("holm"));
        // every command but the first has a dominator with two better axes: 1+2+3+4 pairs
        assert_eq!(info.tests, 20);
        assert_eq!(analysis.frontier, vec![0]);
        assert!(header_line(&analyze_with(
            &uncertain(true, false, None),
            &results,
            &corrected
        ))
        .contains("holm over 20 tests"));
    }

    #[test]
    fn the_noise_threshold_applies_to_the_better_axes() {
        // a is 1% faster with tight runs: significant, but within a 5% noise threshold
        let results = vec![
            noisy("a", (10.0, 0.05), (1.0, 0.01), 40, 31),
            noisy("b", (10.1, 0.05), (1.01, 0.01), 40, 32),
        ];
        let analysis = analyze_with(&uncertain(true, false, Some("5")), &results, &settings());
        assert_eq!(analysis.frontier, vec![0, 1]);
        assert_eq!(analysis.within_noise, vec![1]);
        assert!(analysis.not_significant.is_empty());
    }

    #[test]
    fn probability_of_being_non_dominated() {
        let results = vec![
            noisy("a", (10.0, 3.0), (1.0, 0.3), 20, 41),
            noisy("b", (10.3, 3.0), (1.03, 0.3), 20, 42),
            noisy("c", (30.0, 0.3), (3.0, 0.03), 20, 43),
        ];
        let config = uncertain(false, true, None);
        let analysis = analyze_with(&config, &results, &settings());
        let p = |i: usize| {
            analysis
                .non_dominated_probability
                .iter()
                .find(|p| p.command_index == i)
                .unwrap()
                .probability
        };
        // a and b are indistinguishable: each is non-dominated far more often than not
        assert!(p(0) > 0.3 && p(0) < 0.95, "{}", p(0));
        assert!(p(1) > 0.2 && p(1) < 0.95, "{}", p(1));
        // c is clearly worse than both
        assert!(p(2) < 0.01, "{}", p(2));
        assert_eq!(
            analysis.bootstrap,
            Some(BootstrapInfo {
                resamples: 1000,
                seed: settings().seed
            })
        );
        // the same seed gives the same probabilities, another seed does not
        let again = analyze_with(&config, &results, &settings());
        assert_eq!(
            analysis.non_dominated_probability,
            again.non_dominated_probability
        );
        let other = StatSettings {
            seed: 7,
            ..settings()
        };
        let other = analyze_with(&config, &results, &other);
        assert_ne!(
            analysis.non_dominated_probability,
            other.non_dominated_probability
        );
        // without the flag nothing is computed
        let plain = analyze_with(&uncertain(false, false, None), &results, &settings());
        assert!(plain.non_dominated_probability.is_empty() && plain.bootstrap.is_none());
    }

    #[test]
    fn probability_of_a_command_without_samples_uses_its_estimate() {
        let mut results = vec![
            noisy("a", (10.0, 0.3), (1.0, 0.03), 20, 51),
            noisy("b", (20.0, 0.3), (2.0, 0.03), 20, 52),
        ];
        results[1].times = None; // only the mean of the wall time is known
        let analysis = analyze_with(&uncertain(false, true, None), &results, &settings());
        assert_eq!(analysis.non_dominated_probability[0].probability, 1.0);
        assert_eq!(analysis.non_dominated_probability[1].probability, 0.0);
    }

    #[test]
    fn the_uncertainty_flags_need_a_frontier() {
        assert!(ParetoConfig::parse_with(None, [], None, None, true, false).is_err());
        assert!(ParetoConfig::parse_with(None, [], None, None, false, true).is_err());
        let c = ParetoConfig::parse_with(Some("wall"), [], None, None, true, true)
            .unwrap()
            .unwrap();
        assert!(c.significance && c.probability);
        assert!(!c.without_uncertainty().significance);
    }

    #[test]
    fn json_and_table_show_the_uncertainty() {
        let results = vec![
            noisy("a", (10.0, 3.0), (1.0, 0.3), 20, 1),
            noisy("b", (10.3, 3.0), (1.03, 0.3), 20, 2),
        ];
        let config = uncertain(true, true, None);
        let analysis = analyze_with(&config, &results, &settings());
        let json = serde_json::to_value(&analysis).unwrap();
        assert_eq!(json["significance"]["tests"], 2);
        assert_eq!(json["not_significant"][0]["command_index"], 1);
        assert_eq!(json["bootstrap"]["resamples"], 1000);
        assert_eq!(
            json["non_dominated_probability"].as_array().unwrap().len(),
            2
        );
        let text = terminal(&config, &analysis, &results);
        assert!(text.contains("P(non-dominated)"), "{text}");
        assert!(
            text.contains("not dominated (difference not significant)"),
            "{text}"
        );
        let md = markdown(&config, &analysis, &results);
        assert!(
            md.contains("| P(non-dominated) |") || md.contains("P(non-dominated)"),
            "{md}"
        );
        // the default analysis has none of these keys
        let plain = serde_json::to_value(analyze(&config.without_uncertainty(), &results)).unwrap();
        for key in [
            "significance",
            "not_significant",
            "bootstrap",
            "non_dominated_probability",
        ] {
            assert!(plain.get(key).is_none(), "{key}");
        }
    }
}
