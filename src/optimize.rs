//! `--optimize <VAR> <MIN> <MAX>`: search an integer parameter for the value
//! that minimizes the primary metric (#98, `docs/specs/optimize.md`).
//!
//! The search itself ([`golden_section_min`]) and the analysis of its
//! results ([`analyze`]) are pure functions. The scheduler supplies the
//! function that runs one full benchmark per parameter value.

use std::collections::BTreeMap;

#[cfg(feature = "cli")]
use anyhow::bail;
use anyhow::Result;
#[cfg(feature = "cli")]
use clap::ArgMatches;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::Serialize;

use crate::metric::Metric;
use crate::output::colors;
use crate::output::format::format_duration;

/// Default for `--optimize-budget`
pub const DEFAULT_BUDGET: usize = 12;
/// Default for `--optimize-confirm`
pub const DEFAULT_CONFIRM: usize = 2;
/// Smallest accepted `--optimize-budget`
pub const MIN_BUDGET: usize = 3;
/// Endpoints are only evaluated when the budget allows at least this many values
const ENDPOINT_BUDGET: usize = 5;
/// Fewer evaluated points cannot show a violation of unimodality
const MIN_POINTS_FOR_UNIMODALITY: usize = 4;
const INV_PHI: f64 = 0.618_033_988_749_895;
const ALPHA: f64 = 0.05;

/// Validated `--optimize` options
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptimizeSpec {
    pub var: String,
    /// Search coordinates: the integers themselves, or indices into `grid`
    pub min: i64,
    pub max: i64,
    pub budget: usize,
    /// Re-run the best value and up to this many close candidates (0: off)
    pub confirm: usize,
    /// `-D DELTA`: the values `MIN, MIN+DELTA, ...` that the search runs over
    /// (by index). `None` for plain integers.
    pub grid: Option<Vec<Decimal>>,
}

/// A parameter value in the JSON export: an integer stays an integer.
#[derive(Serialize, Debug, Clone, Copy, PartialEq)]
#[serde(untagged)]
pub enum Num {
    Int(i64),
    Float(f64),
}

impl OptimizeSpec {
    /// Parse and validate `--optimize` and `--optimize-budget`; `None` without `--optimize`.
    #[cfg(feature = "cli")]
    pub fn from_cli_arguments(matches: &ArgMatches) -> Result<Option<Self>> {
        let Some(values) = matches.get_many::<String>("optimize") else {
            if matches.contains_id("optimize-budget")
                && matches.value_source("optimize-budget")
                    == Some(clap::parser::ValueSource::CommandLine)
            {
                bail!("'--optimize-budget' requires '--optimize'");
            }
            return Ok(None);
        };
        let values: Vec<&str> = values.map(String::as_str).collect();
        let [var, min, max] = values[..] else {
            bail!("'--optimize' needs VAR, MIN and MAX");
        };
        let step = matches.get_one::<String>("parameter-step-size");
        if step.is_some() && matches.contains_id("parameter-scan") {
            bail!(
                "'--parameter-step-size' ('-D') is ambiguous with both '--optimize' and \
                 '--parameter-scan'. Give '--optimize' fractional bounds without '-P', or drop '-D'"
            );
        }
        let step = matches.get_one::<String>("parameter-step-size");
        let (min, max, grid) = match step {
            Some(step) => {
                let decimal = |what: &str, text: &str| -> Result<Decimal> {
                    text.trim().parse::<Decimal>().map_err(|_| {
                        anyhow::anyhow!("'--optimize': {what} '{text}' is not a number")
                    })
                };
                let (lo, hi) = (decimal("MIN", min)?, decimal("MAX", max)?);
                let step = decimal("'--parameter-step-size'", step)?;
                if lo > hi {
                    bail!("'--optimize': MIN ({lo}) must not be greater than MAX ({hi})");
                }
                let values = grid_values(lo, hi, step)?;
                (0, values.len() as i64 - 1, Some(values))
            }
            None => {
                let parse = |what: &str, text: &str| -> Result<i64> {
                    text.trim().parse::<i64>().map_err(|_| {
                        anyhow::anyhow!(
                            "'--optimize': {what} '{text}' is not an integer. For fractional \
                             values give a step with '-D DELTA' ('--parameter-step-size')"
                        )
                    })
                };
                let (min, max) = (parse("MIN", min)?, parse("MAX", max)?);
                if min > max {
                    bail!("'--optimize': MIN ({min}) must not be greater than MAX ({max})");
                }
                (min, max, None)
            }
        };
        if var.is_empty() || var.contains(['{', '}']) {
            bail!("'--optimize': invalid parameter name '{var}'");
        }
        if var == crate::command::ITERATION_PLACEHOLDER {
            bail!(
                "The parameter name '{}' is reserved",
                crate::command::ITERATION_PLACEHOLDER
            );
        }
        let budget = matches
            .get_one::<usize>("optimize-budget")
            .copied()
            .unwrap_or(DEFAULT_BUDGET);
        if budget < MIN_BUDGET {
            bail!("'--optimize-budget' must be at least {MIN_BUDGET}, got {budget}");
        }

        // Other parameters combine with the search, the same name does not
        for (arg, arity) in [
            ("parameter-scan", 3),
            ("parameter-list", 2),
            ("parameter-file", 2),
            ("parameter-sample", 2),
        ] {
            let clash = matches
                .get_many::<String>(arg)
                .is_some_and(|v| v.step_by(arity).any(|name| name == var));
            if clash {
                bail!(
                    "'--optimize {var}' conflicts with '--{arg}' on the same variable '{var}'. \
                     Use a different name for one of them"
                );
            }
        }
        let round_robin = matches.get_flag("round-robin")
            || matches
                .get_one::<String>("schedule")
                .is_some_and(|s| s != "grouped");
        if round_robin {
            bail!("'--optimize' cannot be combined with round-robin scheduling");
        }
        if matches.get_many::<String>("import-json").is_some() {
            bail!("'--optimize' cannot be combined with '--import-json'");
        }
        if matches.get_one::<String>("reference").is_some() {
            bail!("'--optimize' cannot be combined with '--reference'");
        }
        let confirm = matches
            .get_one::<usize>("optimize-confirm")
            .copied()
            .unwrap_or(DEFAULT_CONFIRM);
        Ok(Some(Self {
            var: var.to_string(),
            min,
            max,
            budget,
            confirm,
            grid,
        }))
    }

    /// The parameter value of search coordinate `x`, as substituted into the command.
    pub fn text(&self, x: i64) -> String {
        match &self.grid {
            Some(grid) => crate::parameter::ParameterValue::Numeric(
                grid[usize::try_from(x).unwrap_or(0).min(grid.len() - 1)].into(),
            )
            .to_string(),
            None => x.to_string(),
        }
    }

    /// The parameter value of search coordinate `x`, for the JSON export.
    pub fn num(&self, x: i64) -> Num {
        match &self.grid {
            Some(grid) => {
                let d = grid[usize::try_from(x).unwrap_or(0).min(grid.len() - 1)];
                if d.fract().is_zero() {
                    d.to_i64()
                        .map_or_else(|| Num::Float(d.to_f64().unwrap_or(f64::NAN)), Num::Int)
                } else {
                    Num::Float(d.to_f64().unwrap_or(f64::NAN))
                }
            }
            None => Num::Int(x),
        }
    }

    /// Number of integers in `[min, max]`
    pub fn range_size(&self) -> i128 {
        i128::from(self.max) - i128::from(self.min) + 1
    }
}

impl std::fmt::Display for Num {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Num::Int(i) => write!(f, "{i}"),
            Num::Float(v) => write!(f, "{v}"),
        }
    }
}

/// The values `lo, lo+step, ...` up to `hi`
#[cfg(feature = "cli")]
fn grid_values(lo: Decimal, hi: Decimal, step: Decimal) -> Result<Vec<Decimal>> {
    if step <= Decimal::ZERO {
        bail!("'--optimize': the step must be positive, got {step}");
    }
    let values: Vec<Decimal> = crate::parameter::range_step::RangeStep::new(lo, hi, step)
        .map_err(|e| anyhow::anyhow!("'--optimize': {e}"))?
        .collect();
    if values.is_empty() {
        bail!("'--optimize': the range {lo}..{hi} with step {step} has no values");
    }
    Ok(values)
}

/// Result of a search: values in evaluation order
#[derive(Debug, Clone, PartialEq)]
pub struct SearchOutcome {
    pub evaluated: Vec<(i64, f64)>,
    /// The evaluation function asked to stop (Ctrl-C)
    pub interrupted: bool,
    /// All values of the range were evaluated (no search happened)
    pub exhaustive: bool,
}

struct Evaluator<F> {
    cache: BTreeMap<i64, f64>,
    order: Vec<(i64, f64)>,
    budget: usize,
    f: F,
    stop: bool,
    interrupted: bool,
}

impl<F: FnMut(i64) -> Result<Option<f64>>> Evaluator<F> {
    /// Value at `x`, evaluating it if needed. `None` once the search must stop
    /// (budget spent or interrupted).
    fn get(&mut self, x: i128) -> Result<Option<f64>> {
        if self.stop {
            return Ok(None);
        }
        let x = i64::try_from(x).expect("search stays inside the i64 range");
        if let Some(v) = self.cache.get(&x) {
            return Ok(Some(*v));
        }
        if self.cache.len() >= self.budget {
            self.stop = true;
            return Ok(None);
        }
        match (self.f)(x)? {
            Some(v) => {
                self.cache.insert(x, v);
                self.order.push((x, v));
                Ok(Some(v))
            }
            None => {
                self.stop = true;
                self.interrupted = true;
                Ok(None)
            }
        }
    }
}

/// Minimize `f` over the integers in `[lo, hi]`, assuming it is unimodal.
///
/// `f` returns `Ok(None)` to stop the search (interruption). At most `budget`
/// distinct values are evaluated; repeated requests hit a cache. A range that
/// fits into the budget is evaluated completely.
pub fn golden_section_min(
    lo: i64,
    hi: i64,
    budget: usize,
    f: impl FnMut(i64) -> Result<Option<f64>>,
) -> Result<SearchOutcome> {
    let mut ev = Evaluator {
        cache: BTreeMap::new(),
        order: Vec::new(),
        budget,
        f,
        stop: false,
        interrupted: false,
    };
    let (lo, hi) = (i128::from(lo), i128::from(hi));
    let exhaustive = hi - lo < budget as i128;
    if exhaustive {
        for x in lo..=hi {
            if ev.get(x)?.is_none() {
                break;
            }
        }
    } else {
        search(&mut ev, lo, hi)?;
    }
    Ok(SearchOutcome {
        evaluated: ev.order,
        interrupted: ev.interrupted,
        exhaustive,
    })
}

fn search<F: FnMut(i64) -> Result<Option<f64>>>(
    ev: &mut Evaluator<F>,
    lo: i128,
    hi: i128,
) -> Result<()> {
    if ev.budget >= ENDPOINT_BUDGET {
        ev.get(lo)?;
        ev.get(hi)?;
    }
    let (mut l, mut h) = (lo, hi);
    while h - l > 2 {
        let d = ((h - l) as f64 * INV_PHI).round() as i128;
        let a = h - d;
        // for a width of 4 both interior points would coincide
        let b = (l + d).max(a + 1);
        let (Some(fa), Some(fb)) = (ev.get(a)?, ev.get(b)?) else {
            return Ok(());
        };
        // ties keep the lower side
        if fa <= fb {
            h = b;
        } else {
            l = a;
        }
    }
    for x in l..=h {
        if ev.get(x)?.is_none() {
            break;
        }
    }
    Ok(())
}

/// Index of the lowest value; ties go to the lower parameter value
pub fn best_index(points: &[(i64, f64)]) -> Option<usize> {
    points
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)))
        .map(|(i, _)| i)
}

/// A point with its standard error, sorted by parameter value inside the functions below
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: i64,
    pub value: f64,
    pub se: f64,
}

/// `false` if, moving away from the best point on either side, the metric
/// drops again by more than the combined standard error (an interior local
/// maximum). Needs at least four points, otherwise `true`.
pub fn looks_unimodal(points: &[Point]) -> bool {
    if points.len() < MIN_POINTS_FOR_UNIMODALITY {
        return true;
    }
    let mut sorted = points.to_vec();
    sorted.sort_by_key(|p| p.x);
    let best = sorted
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| a.value.total_cmp(&b.value).then(a.x.cmp(&b.x)))
        .map_or(0, |(i, _)| i);

    let dips = |side: &mut dyn Iterator<Item = &Point>| {
        let mut peak: Option<&Point> = None;
        for p in side {
            if let Some(q) = peak {
                if q.value - p.value > q.se.hypot(p.se) {
                    return true;
                }
            }
            if peak.is_none_or(|q| p.value > q.value) {
                peak = Some(p);
            }
        }
        false
    };
    !(dips(&mut sorted[best + 1..].iter()) || dips(&mut sorted[..best].iter().rev()))
}

/// Measured data of one evaluated value
#[derive(Debug, Clone)]
pub struct PointData {
    pub x: i64,
    pub value: f64,
    /// Standard error of the mean of the primary metric
    pub se: f64,
    /// Per-run samples of the primary metric
    pub samples: Option<Vec<f64>>,
    /// Mean wall-clock time, shown next to a non-time metric
    pub wall_mean: f64,
    pub command: String,
}

/// Values to run a second time after the search, best first.
///
/// With a noisy metric the search can settle on a value that only looked best
/// once. The candidates are the best value and up to `extra` other evaluated
/// values whose difference from it is not significant (or not computable);
/// when every other value is clearly worse, nothing needs confirming.
pub fn confirmation_candidates(points: &[PointData], extra: usize) -> Vec<i64> {
    let values: Vec<(i64, f64)> = points.iter().map(|p| (p.x, p.value)).collect();
    let Some(best_i) = best_index(&values) else {
        return Vec::new();
    };
    let best = &points[best_i];
    let mut others: Vec<&PointData> = points
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != best_i)
        .map(|(_, p)| p)
        .filter(|p| {
            let comparison = best
                .samples
                .as_deref()
                .zip(p.samples.as_deref())
                .and_then(|(a, b)| crate::stats::deep::compare_samples(a, b));
            comparison.is_none_or(|c| c.p_value >= ALPHA)
        })
        .collect();
    others.sort_by(|a, b| a.value.total_cmp(&b.value).then(a.x.cmp(&b.x)));
    others.truncate(extra);
    if others.is_empty() {
        return Vec::new();
    }
    std::iter::once(best.x)
        .chain(others.into_iter().map(|p| p.x))
        .collect()
}

/// Fold repeated evaluations of the same value into one point: the samples
/// are pooled (the mean and its standard error are recomputed from them).
/// Points keep the order of their first evaluation.
pub fn merge_repeated(points: Vec<PointData>) -> Vec<PointData> {
    let mut merged: Vec<PointData> = Vec::with_capacity(points.len());
    for point in points {
        match merged.iter_mut().find(|m| m.x == point.x) {
            None => merged.push(point),
            Some(m) => {
                m.wall_mean = (m.wall_mean + point.wall_mean) / 2.0;
                match (m.samples.as_mut(), point.samples) {
                    (Some(a), Some(b)) => {
                        a.extend(b);
                        let n = a.len() as f64;
                        m.value = a.iter().sum::<f64>() / n;
                        m.se = if a.len() > 1 {
                            let var =
                                a.iter().map(|v| (v - m.value).powi(2)).sum::<f64>() / (n - 1.0);
                            (var / n).sqrt()
                        } else {
                            0.0
                        };
                    }
                    (_, _) => {
                        // no samples to pool: average the two means
                        m.value = (m.value + point.value) / 2.0;
                        m.se = m.se.hypot(point.se) / 2.0;
                        m.samples = None;
                    }
                }
            }
        }
    }
    merged
}

/// The `optimize` object of the JSON export
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct OptimizeReport {
    pub var: String,
    pub min: Num,
    pub max: Num,
    pub metric: String,
    pub algorithm: &'static str,
    pub budget: usize,
    pub evaluations: usize,
    pub best: Num,
    /// Search coordinate of `best` (index into the grid with `-D`)
    #[serde(skip)]
    pub best_x: i64,
    pub best_value: f64,
    pub best_command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_up: Option<Num>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p_value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub significant: Option<bool>,
    pub unimodal_warning: bool,
    pub at_boundary: bool,
    pub evaluated: Vec<Num>,
    /// Values that were run a second time (`--optimize-confirm`); their samples
    /// are pooled into `best_value`. `results` keeps the first run of each value.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reevaluated: Vec<Num>,
    /// Values of the other parameters this search ran with
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub fixed: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub interrupted: bool,
}

/// Evaluate a finished (or interrupted) search. `points` are in evaluation order.
pub fn analyze(
    spec: &OptimizeSpec,
    metric: Metric,
    outcome: &SearchOutcome,
    points: &[PointData],
    reevaluated: &[i64],
    fixed: BTreeMap<String, String>,
) -> Option<OptimizeReport> {
    let values: Vec<(i64, f64)> = points.iter().map(|p| (p.x, p.value)).collect();
    let best_i = best_index(&values)?;
    let best = &points[best_i];
    let runner_up = points
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != best_i)
        .min_by(|(_, a), (_, b)| a.value.total_cmp(&b.value).then(a.x.cmp(&b.x)))
        .map(|(_, p)| p);
    let comparison = runner_up.and_then(|r| {
        crate::stats::deep::compare_samples(best.samples.as_deref()?, r.samples.as_deref()?)
    });
    let stats: Vec<Point> = points
        .iter()
        .map(|p| Point {
            x: p.x,
            value: p.value,
            se: p.se,
        })
        .collect();
    Some(OptimizeReport {
        var: spec.var.clone(),
        min: spec.num(spec.min),
        max: spec.num(spec.max),
        metric: metric.name().to_string(),
        algorithm: if outcome.exhaustive {
            "exhaustive"
        } else {
            "golden-section"
        },
        budget: spec.budget,
        evaluations: points.len(),
        best: spec.num(best.x),
        best_x: best.x,
        best_value: best.value,
        best_command: best.command.clone(),
        runner_up: runner_up.map(|r| spec.num(r.x)),
        p_value: comparison.as_ref().map(|c| c.p_value),
        significant: comparison.as_ref().map(|c| c.p_value < ALPHA),
        unimodal_warning: !looks_unimodal(&stats),
        at_boundary: spec.min < spec.max && (best.x == spec.min || best.x == spec.max),
        evaluated: points.iter().map(|p| spec.num(p.x)).collect(),
        reevaluated: reevaluated.iter().map(|x| spec.num(*x)).collect(),
        fixed,
        interrupted: outcome.interrupted,
    })
}

fn format_value(metric: Metric, value: f64) -> String {
    match metric {
        m if m.is_time() => format_duration(value, None),
        Metric::Energy => format!("{value:.3} J"),
        Metric::Memory => format!("{value:.0} B"),
        Metric::Instructions | Metric::Cycles => format!("{value:.0}"),
        _ => format!("{value:.4}"),
    }
}

/// Terminal summary of one search; points are listed best first.
pub fn summary(
    spec: &OptimizeSpec,
    metric: Metric,
    report: &OptimizeReport,
    points: &[PointData],
) -> String {
    use std::fmt::Write as _;
    let var = &spec.var;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Optimization of {var} ∈ [{}, {}] for {} ({} search, {} evaluations{}):",
        spec.num(spec.min),
        spec.num(spec.max),
        metric.name(),
        report.algorithm,
        report.evaluations,
        if report.interrupted {
            ", interrupted"
        } else {
            ""
        }
    );
    let mut sorted: Vec<&PointData> = points.iter().collect();
    sorted.sort_by(|a, b| a.value.total_cmp(&b.value).then(a.x.cmp(&b.x)));
    for p in sorted {
        let marker = if p.x == report.best_x {
            "   ← best"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "  {var}={}  {} ± {}{marker}",
            spec.text(p.x),
            format_value(metric, p.value),
            format_value(metric, p.se)
        );
    }
    let wall = points
        .iter()
        .find(|p| p.x == report.best_x)
        .filter(|_| !metric.is_time())
        .map(|p| format!("; wall {}", format_duration(p.wall_mean, None)))
        .unwrap_or_default();
    let _ = write!(
        out,
        "Best: {var}={} ({} {}{wall}).",
        spec.num(report.best_x),
        metric.name(),
        format_value(metric, report.best_value)
    );
    match (report.runner_up, report.p_value, report.significant) {
        (Some(r), Some(p), Some(sig)) => {
            let _ = write!(
                out,
                " The difference from {var}={r} is {}significant (p={p:.2}).",
                if sig { "" } else { "not " }
            );
        }
        (Some(r), _, _) => {
            let _ = write!(
                out,
                " The significance of the difference from {var}={r} is not computable."
            );
        }
        _ => {}
    }
    if !report.reevaluated.is_empty() {
        let list: Vec<String> = report
            .reevaluated
            .iter()
            .map(|v| format!("{var}={v}"))
            .collect();
        let _ = write!(
            out,
            "\nBecause the differences were not significant, {} {} run a second time; \
             the values above pool both runs.",
            list.join(", "),
            if list.len() == 1 { "was" } else { "were" }
        );
    }
    if report.at_boundary {
        let _ = write!(
            out,
            "\nThe best value is at the boundary of the range; the optimum may lie outside it."
        );
    }
    if report.algorithm == "exhaustive" {
        let _ = write!(
            out,
            "\nThe range fits into the budget of {}, so all values were evaluated.",
            report.budget
        );
    }
    out
}

/// Warning printed when the curve does not look unimodal
pub fn unimodal_warning_text(var: &str) -> String {
    format!(
        "{} The metric does not look unimodal in {var}; the result may be a local minimum.",
        colors::yellow("Warning:")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(lo: i64, hi: i64, budget: usize, f: impl Fn(i64) -> f64) -> SearchOutcome {
        golden_section_min(lo, hi, budget, |x| Ok(Some(f(x)))).unwrap()
    }

    fn best_x(o: &SearchOutcome) -> i64 {
        o.evaluated[best_index(&o.evaluated).unwrap()].0
    }

    #[test]
    fn parabola_found_within_budget_without_reevaluation() {
        let mut calls = Vec::new();
        let o = golden_section_min(1, 64, 12, |x| {
            calls.push(x);
            Ok(Some(((x - 17) * (x - 17)) as f64))
        })
        .unwrap();
        assert!(o.evaluated.len() <= 12);
        assert_eq!(best_x(&o), 17);
        let mut unique = calls.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), calls.len(), "a point was evaluated twice");
        assert!(!o.exhaustive);
    }

    #[test]
    fn plateau_terminates_and_picks_lowest() {
        let o = run(1, 100, 8, |_| 5.0);
        assert!(o.evaluated.len() <= 8);
        assert_eq!(best_x(&o), 1);
    }

    #[test]
    fn budget_exhaustion_returns_best_seen() {
        let o = run(1, 1_000_000, 3, |x| ((x - 700_000) as f64).abs());
        assert_eq!(o.evaluated.len(), 3);
        assert!(best_index(&o.evaluated).is_some());
    }

    #[test]
    fn tiny_ranges_are_evaluated_completely() {
        for (lo, hi) in [(4, 4), (4, 5), (4, 6)] {
            let o = run(lo, hi, 12, |x| x as f64);
            assert_eq!(o.evaluated.len() as i64, hi - lo + 1);
            assert!(o.exhaustive);
            assert_eq!(best_x(&o), lo);
        }
    }

    #[test]
    fn minimum_at_either_boundary() {
        assert_eq!(best_x(&run(1, 64, 12, |x| x as f64)), 1);
        assert_eq!(best_x(&run(1, 64, 12, |x| -(x as f64))), 64);
    }

    #[test]
    fn ties_pick_the_lower_value() {
        let o = run(1, 64, 12, |x| if x == 20 || x == 21 { 0.0 } else { 1.0 });
        let lowest = o
            .evaluated
            .iter()
            .filter(|(_, v)| *v == 0.0)
            .map(|(x, _)| *x)
            .min();
        if let Some(l) = lowest {
            assert_eq!(best_x(&o), l);
        }
        assert_eq!(best_index(&[(5, 1.0), (3, 1.0), (4, 2.0)]), Some(1));
    }

    #[test]
    fn negative_bounds() {
        let o = run(-50, -10, 12, |x| ((x + 33) * (x + 33)) as f64);
        assert_eq!(best_x(&o), -33);
    }

    #[test]
    fn extreme_bounds_do_not_overflow() {
        let o = run(i64::MIN, i64::MAX, 6, |x| (x as f64).abs());
        assert!(o.evaluated.len() <= 6);
    }

    #[test]
    fn interruption_returns_partial_results() {
        let mut n = 0;
        let o = golden_section_min(1, 64, 12, |x| {
            n += 1;
            Ok((n <= 4).then_some(x as f64))
        })
        .unwrap();
        assert!(o.interrupted);
        assert_eq!(o.evaluated.len(), 4);
    }

    #[test]
    fn errors_propagate() {
        let r = golden_section_min(1, 64, 12, |_| anyhow::bail!("boom"));
        assert!(r.is_err());
    }

    #[test]
    fn range_larger_than_budget_stays_within_budget() {
        for hi in [13, 20, 64, 1000] {
            let o = run(1, hi, 12, |x| ((x - 5) * (x - 5)) as f64);
            assert!(o.evaluated.len() <= 12, "range 1..={hi}");
        }
    }

    /// Deterministic stand-in for a property test: random convex integer functions
    #[test]
    fn random_convex_functions_are_within_one_step() {
        let mut state = 0x2545_F491_4F6C_DD1D_u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..500 {
            let lo = (next() % 50) as i64 - 25;
            let width = (next() % 300) as i64 + 20;
            let hi = lo + width;
            let center = lo + (next() % (width as u64 + 1)) as i64;
            let power = 1 + (next() % 3) as i32;
            let f = |x: i64| ((x - center).abs() as f64).powi(power);
            // ceil(log_phi(range)) + 5: the two endpoints cost 2 evaluations more than
            // the +3 of the spec, which assumes a search without them
            let budget = ((width as f64).ln() / (1.0 / INV_PHI).ln()).ceil() as usize + 5;
            let o = run(lo, hi, budget, f);
            let found = best_x(&o);
            assert!(
                (found - center).abs() <= 1,
                "range {lo}..={hi} center {center} power {power} budget {budget}: found {found}"
            );
            assert!(o.evaluated.len() <= budget);
        }
    }

    fn pts(values: &[(i64, f64)]) -> Vec<Point> {
        values
            .iter()
            .map(|&(x, value)| Point { x, value, se: 0.01 })
            .collect()
    }

    #[test]
    fn unimodality_detector() {
        let v = pts(&[(1, 9.0), (2, 5.0), (3, 2.0), (4, 4.0), (5, 8.0)]);
        assert!(looks_unimodal(&v));
        let w = pts(&[(1, 9.0), (2, 1.0), (3, 6.0), (4, 2.0), (5, 8.0)]);
        assert!(!looks_unimodal(&w));
        // dip on the left of the best
        let w_left = pts(&[(1, 5.0), (2, 9.0), (3, 6.0), (4, 1.0), (5, 3.0)]);
        assert!(!looks_unimodal(&w_left));
        // too few points
        assert!(looks_unimodal(&pts(&[(1, 1.0), (2, 9.0), (3, 1.0)])));
        // a dip within the standard error is noise
        let noisy: Vec<Point> = [(1, 9.0), (2, 1.0), (3, 6.0), (4, 5.9), (5, 8.0)]
            .iter()
            .map(|&(x, value)| Point { x, value, se: 1.0 })
            .collect();
        assert!(looks_unimodal(&noisy));
    }

    fn data(x: i64, value: f64, samples: Option<Vec<f64>>) -> PointData {
        PointData {
            x,
            value,
            se: 0.01,
            samples,
            wall_mean: 1.0,
            command: format!("cmd {x}"),
        }
    }

    fn spec() -> OptimizeSpec {
        OptimizeSpec {
            var: "j".into(),
            min: 1,
            max: 64,
            budget: 12,
            confirm: DEFAULT_CONFIRM,
            grid: None,
        }
    }

    #[test]
    fn analyze_reports_best_runner_up_and_significance() {
        let near = |c: f64| Some(vec![c - 0.01, c, c + 0.01, c, c - 0.01, c + 0.01]);
        let points = vec![
            data(1, 3.0, near(3.0)),
            data(24, 1.0, near(1.0)),
            data(16, 1.5, near(1.5)),
        ];
        let outcome = SearchOutcome {
            evaluated: points.iter().map(|p| (p.x, p.value)).collect(),
            interrupted: false,
            exhaustive: false,
        };
        let r = analyze(
            &spec(),
            Metric::Wall,
            &outcome,
            &points,
            &[],
            BTreeMap::new(),
        )
        .unwrap();
        assert_eq!((r.best, r.runner_up), (Num::Int(24), Some(Num::Int(16))));
        assert_eq!(r.significant, Some(true));
        assert!(!r.at_boundary && !r.unimodal_warning);
        assert_eq!(r.evaluated, vec![Num::Int(1), Num::Int(24), Num::Int(16)]);
        let text = summary(&spec(), Metric::Wall, &r, &points);
        assert!(text.contains("j=24"), "{text}");
        assert!(text.contains("← best"), "{text}");
        assert!(text.contains("is significant"), "{text}");
    }

    #[test]
    fn analyze_without_samples_is_not_computable() {
        let points = vec![data(1, 1.0, None), data(2, 2.0, None)];
        let outcome = SearchOutcome {
            evaluated: vec![(1, 1.0), (2, 2.0)],
            interrupted: false,
            exhaustive: true,
        };
        let r = analyze(
            &spec(),
            Metric::Wall,
            &outcome,
            &points,
            &[],
            BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(r.p_value, None);
        assert!(r.at_boundary);
        assert_eq!(r.algorithm, "exhaustive");
        assert!(summary(&spec(), Metric::Wall, &r, &points).contains("not computable"));
    }

    fn noisy(x: i64, center: f64) -> PointData {
        let samples = vec![center - 0.2, center + 0.2, center - 0.1, center + 0.1];
        data(x, center, Some(samples))
    }

    #[test]
    fn candidates_are_the_best_and_its_not_significantly_worse_neighbours() {
        // 8 and 9 are within the noise of 10, 1 and 40 are clearly worse
        let points = vec![
            noisy(1, 5.0),
            noisy(10, 1.0),
            noisy(8, 1.05),
            noisy(9, 1.1),
            noisy(40, 4.0),
        ];
        assert_eq!(confirmation_candidates(&points, 2), vec![10, 8, 9]);
        assert_eq!(confirmation_candidates(&points, 1), vec![10, 8]);
        assert!(confirmation_candidates(&points, 0).is_empty());
    }

    #[test]
    fn nothing_to_confirm_when_the_best_is_clearly_best() {
        let points = vec![noisy(1, 5.0), noisy(10, 1.0), noisy(40, 4.0)];
        assert!(confirmation_candidates(&points, 2).is_empty());
        assert!(confirmation_candidates(&[], 2).is_empty());
    }

    #[test]
    fn candidates_without_samples_are_confirmed() {
        let points = vec![data(1, 1.0, None), data(2, 1.1, None)];
        assert_eq!(confirmation_candidates(&points, 2), vec![1, 2]);
    }

    #[test]
    fn repeated_evaluations_are_pooled_and_can_change_the_best() {
        // x=10 looked best once (lucky), x=8 is better on average
        let first = vec![
            data(10, 1.0, Some(vec![1.0, 1.0])),
            data(8, 1.1, Some(vec![1.1, 1.1])),
        ];
        let again = vec![
            data(10, 1.6, Some(vec![1.6, 1.6])),
            data(8, 1.1, Some(vec![1.1, 1.1])),
        ];
        let merged = merge_repeated(first.into_iter().chain(again).collect());
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].x, 10);
        assert!((merged[0].value - 1.3).abs() < 1e-12);
        assert_eq!(merged[0].samples.as_ref().unwrap().len(), 4);
        assert!(merged[0].se > 0.0);
        let values: Vec<(i64, f64)> = merged.iter().map(|p| (p.x, p.value)).collect();
        assert_eq!(merged[best_index(&values).unwrap()].x, 8);
    }

    #[test]
    fn pooling_without_samples_averages_the_means() {
        let merged = merge_repeated(vec![data(1, 1.0, None), data(1, 3.0, None)]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].value, 2.0);
    }

    #[test]
    fn grid_values_map_to_text_and_json_numbers() {
        let spec = OptimizeSpec {
            var: "d".into(),
            min: 0,
            max: 4,
            budget: 12,
            confirm: 0,
            grid: Some(
                ["0.5", "1", "1.5", "2", "2.5"]
                    .iter()
                    .map(|v| v.parse().unwrap())
                    .collect(),
            ),
        };
        assert_eq!(spec.text(0), "0.5");
        assert_eq!(spec.text(1), "1");
        assert_eq!(spec.num(2), Num::Float(1.5));
        assert_eq!(spec.num(1), Num::Int(1));
        assert_eq!(serde_json::to_string(&spec.num(1)).unwrap(), "1");
        assert_eq!(serde_json::to_string(&spec.num(4)).unwrap(), "2.5");
    }

    #[cfg(feature = "cli")]
    #[test]
    fn grid_rejects_non_positive_steps_and_is_inclusive() {
        let d = |v: &str| -> Decimal { v.parse().unwrap() };
        assert!(grid_values(d("0"), d("1"), d("0")).is_err());
        assert!(grid_values(d("0"), d("1"), d("-0.1")).is_err());
        let g = grid_values(d("0.1"), d("0.5"), d("0.1")).unwrap();
        assert_eq!(g.len(), 5);
        assert_eq!(*g.last().unwrap(), d("0.5"));
    }
}
