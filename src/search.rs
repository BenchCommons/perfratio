//! Multi-dimensional parameter search for `--optimize` (#385, `docs/specs/optimize-search.md`):
//! random search, successive halving and Bayesian optimization over the integer grid of
//! several parameters, plus the independent validation of the winner.
//!
//! The algorithms are pure: they receive a function that benchmarks one point (at an
//! optional fidelity, i.e. a number of runs) and return every trial in evaluation order.
//! The scheduler supplies that function. All randomness comes from a seeded
//! SplitMix64 stream, so a search is reproducible for a given `--seed`.

use std::collections::{BTreeMap, HashSet};

use anyhow::{bail, Result};
#[cfg(feature = "cli")]
use clap::ArgMatches;
use serde::Serialize;

use crate::metric::Metric;
use crate::optimize::{Num, ALPHA};
#[cfg(feature = "cli")]
use crate::optimize::{DEFAULT_BUDGET, MIN_BUDGET};
use crate::output::format::format_duration;

/// Successive halving keeps `1/ETA` of the points and multiplies the runs by `ETA` per rung.
pub const ETA: u64 = 3;
/// Candidates scored per Bayesian step when the grid is larger than this
const MAX_CANDIDATES: usize = 1024;
/// Largest grid that is scored completely by the Bayesian search
const FULL_GRID: u128 = 4096;
/// Points listed in the terminal summary
const SUMMARY_ROWS: usize = 10;

/// `--optimize-algorithm`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    /// One variable, unimodal (the original `--optimize`; not handled here)
    Golden,
    Random,
    Halving,
    Bayes,
}

impl Algorithm {
    pub fn name(self) -> &'static str {
        match self {
            Algorithm::Golden => "golden-section",
            Algorithm::Random => "random",
            Algorithm::Halving => "successive-halving",
            Algorithm::Bayes => "bayesian",
        }
    }

    #[cfg(feature = "cli")]
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "golden" => Algorithm::Golden,
            "random" => Algorithm::Random,
            "halving" => Algorithm::Halving,
            "bayes" => Algorithm::Bayes,
            _ => return None,
        })
    }
}

/// One searched variable: the integers in `[min, max]`
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Dim {
    #[serde(rename = "name")]
    pub var: String,
    pub min: i64,
    pub max: i64,
}

impl Dim {
    fn size(&self) -> u128 {
        (i128::from(self.max) - i128::from(self.min) + 1) as u128
    }
}

/// Validated options of a search that is not the plain one-variable golden-section search
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchSpec {
    pub dims: Vec<Dim>,
    pub algorithm: Algorithm,
    /// Maximum number of distinct points
    pub budget: usize,
    pub seed: u64,
    /// Benchmark the winner and the runner-up again in a fresh, independent round
    pub validate: bool,
}

impl SearchSpec {
    /// Parse `--optimize` (repeatable), `--optimize-algorithm` and `--optimize-validate`.
    /// `None` when the original single-variable golden-section search applies.
    #[cfg(feature = "cli")]
    pub fn from_cli_arguments(matches: &ArgMatches, seed: u64) -> Result<Option<Self>> {
        let explicit = matches
            .get_one::<String>("optimize-algorithm")
            .map(|a| Algorithm::parse(a).expect("restricted by the value parser"));
        let validate = matches.get_flag("optimize-validate");
        let Some(values) = matches.get_many::<String>("optimize") else {
            for (flag, set) in [
                ("--optimize-algorithm", explicit.is_some()),
                ("--optimize-validate", validate),
            ] {
                if set {
                    bail!("'{flag}' requires '--optimize'");
                }
            }
            return Ok(None);
        };
        let values: Vec<&str> = values.map(String::as_str).collect();
        let n = values.len() / 3;
        let algorithm = match (explicit, n) {
            (Some(Algorithm::Golden) | None, 1) => {
                if validate {
                    bail!(
                        "'--optimize-validate' needs '--optimize-algorithm random', 'halving' or \
                         'bayes', or more than one '--optimize'"
                    );
                }
                return Ok(None);
            }
            (Some(Algorithm::Golden), _) => bail!(
                "'--optimize-algorithm golden' searches one variable; use 'random', 'halving' \
                 or 'bayes' with several '--optimize'"
            ),
            (None, _) => Algorithm::Random,
            (Some(a), _) => a,
        };
        if matches.get_one::<String>("parameter-step-size").is_some() {
            bail!(
                "'--parameter-step-size' ('-D') is only supported by the one-variable \
                 golden-section '--optimize'; the other algorithms search integers"
            );
        }
        let mut dims: Vec<Dim> = Vec::new();
        for chunk in values.chunks(3) {
            let [var, min, max] = chunk[..] else {
                bail!("'--optimize' needs VAR, MIN and MAX");
            };
            let parse = |what: &str, text: &str| -> Result<i64> {
                text.trim().parse::<i64>().map_err(|_| {
                    anyhow::anyhow!(
                        "'--optimize {var}': {what} '{text}' is not an integer (the {} \
                         algorithm searches integers)",
                        algorithm.name()
                    )
                })
            };
            let (min, max) = (parse("MIN", min)?, parse("MAX", max)?);
            if min > max {
                bail!("'--optimize {var}': MIN ({min}) must not be greater than MAX ({max})");
            }
            if var.is_empty() || var.contains(['{', '}']) {
                bail!("'--optimize': invalid parameter name '{var}'");
            }
            if var == crate::command::ITERATION_PLACEHOLDER {
                bail!(
                    "The parameter name '{}' is reserved",
                    crate::command::ITERATION_PLACEHOLDER
                );
            }
            if dims.iter().any(|d| d.var == var) {
                bail!("'--optimize {var}' is given twice");
            }
            dims.push(Dim {
                var: var.to_string(),
                min,
                max,
            });
        }
        let budget = matches
            .get_one::<usize>("optimize-budget")
            .copied()
            .unwrap_or(DEFAULT_BUDGET);
        if budget < MIN_BUDGET {
            bail!("'--optimize-budget' must be at least {MIN_BUDGET}, got {budget}");
        }
        if matches.value_source("optimize-confirm") == Some(clap::parser::ValueSource::CommandLine)
        {
            bail!(
                "'--optimize-confirm' belongs to the golden-section search; the {} search \
                 has '--optimize-validate'",
                algorithm.name()
            );
        }
        for (arg, arity) in [
            ("parameter-scan", 3),
            ("parameter-list", 2),
            ("parameter-file", 2),
            ("parameter-sample", 2),
        ] {
            for dim in &dims {
                let clash = matches
                    .get_many::<String>(arg)
                    .is_some_and(|v| v.step_by(arity).any(|name| *name == dim.var));
                if clash {
                    bail!(
                        "'--optimize {var}' conflicts with '--{arg}' on the same variable \
                         '{var}'. Use a different name for one of them",
                        var = dim.var
                    );
                }
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
        Ok(Some(Self {
            dims,
            algorithm,
            budget,
            seed,
            validate,
        }))
    }

    /// Number of points of the grid (saturating)
    pub fn space(&self) -> u128 {
        self.dims
            .iter()
            .fold(1u128, |acc, d| acc.saturating_mul(d.size()))
    }

    /// Text of coordinate `x` of dimension `i`, as substituted into the command
    pub fn text(&self, point: &[i64]) -> Vec<(String, String)> {
        self.dims
            .iter()
            .zip(point)
            .map(|(d, x)| (d.var.clone(), x.to_string()))
            .collect()
    }

    fn at_boundary(&self, point: &[i64]) -> bool {
        self.dims
            .iter()
            .zip(point)
            .any(|(d, x)| d.min < d.max && (*x == d.min || *x == d.max))
    }
}

/// Measurement of one trial
#[derive(Debug, Clone, PartialEq)]
pub struct Eval {
    pub value: f64,
    /// Standard error of the mean of the primary metric
    pub se: f64,
    pub samples: Option<Vec<f64>>,
    pub wall_mean: f64,
    pub command: String,
    /// Benchmark runs executed
    pub runs: u64,
}

/// One benchmark of one point, at the fidelity of its rung
#[derive(Debug, Clone, PartialEq)]
pub struct Trial {
    pub point: Vec<i64>,
    pub rung: usize,
    pub eval: Eval,
}

/// Benchmark one point. `Some(runs)` fixes the number of runs (successive halving);
/// `Ok(None)` stops the search (interruption).
pub trait Evaluate: FnMut(&[i64], Option<u64>) -> Result<Option<Eval>> {}
impl<F: FnMut(&[i64], Option<u64>) -> Result<Option<Eval>>> Evaluate for F {}

/// What a search did
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// Every benchmark in evaluation order (halving benchmarks a point once per rung)
    pub trials: Vec<Trial>,
    pub interrupted: bool,
    /// The whole grid fit into the budget and was evaluated
    pub exhaustive: bool,
    /// `(runs per point, points)` of each rung of successive halving
    pub rungs: Vec<(u64, usize)>,
}

/// SplitMix64: tiny, fixed forever, so seeds keep their meaning across releases
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u128) -> u128 {
        let wide = (u128::from(self.next()) << 64) | u128::from(self.next());
        wide % n
    }
}

/// The point at grid index `index` (row-major, the last variable varies fastest)
fn decode(dims: &[Dim], mut index: u128) -> Vec<i64> {
    let mut point = vec![0; dims.len()];
    for (i, d) in dims.iter().enumerate().rev() {
        let size = d.size();
        point[i] = (i128::from(d.min) + (index % size) as i128) as i64;
        index /= size;
    }
    point
}

/// `n` distinct grid points in random order (all of them, in grid order, if `n >= space`)
fn sample_points(dims: &[Dim], space: u128, n: usize, rng: &mut Rng) -> Vec<Vec<i64>> {
    if space <= n as u128 {
        return (0..space).map(|i| decode(dims, i)).collect();
    }
    let mut seen = HashSet::new();
    let mut points = Vec::with_capacity(n);
    while points.len() < n {
        let index = rng.below(space);
        if seen.insert(index) {
            points.push(decode(dims, index));
        }
    }
    points
}

struct Recorder<F> {
    f: F,
    trials: Vec<Trial>,
    interrupted: bool,
}

impl<F: Evaluate> Recorder<F> {
    /// `false` once the search must stop
    fn run(&mut self, point: &[i64], rung: usize, runs: Option<u64>) -> Result<Option<f64>> {
        match (self.f)(point, runs)? {
            Some(eval) => {
                let value = eval.value;
                self.trials.push(Trial {
                    point: point.to_vec(),
                    rung,
                    eval,
                });
                Ok(Some(value))
            }
            None => {
                self.interrupted = true;
                Ok(None)
            }
        }
    }
}

/// Run `spec.algorithm` (never `Golden`).
///
/// `base_runs` sets the runs per point of successive halving: the first rung uses
/// `base_runs`, each further rung `ETA` times more.
pub fn run_search(spec: &SearchSpec, base_runs: u64, f: impl Evaluate) -> Result<Outcome> {
    let space = spec.space();
    let mut rng = Rng(spec.seed);
    let mut rec = Recorder {
        f,
        trials: Vec::new(),
        interrupted: false,
    };
    let exhaustive = space <= spec.budget as u128;
    let mut rungs = Vec::new();
    match spec.algorithm {
        Algorithm::Golden => bail!("the golden-section search is not a multi-dimensional search"),
        Algorithm::Random => {
            for p in sample_points(&spec.dims, space, spec.budget, &mut rng) {
                if rec.run(&p, 0, None)?.is_none() {
                    break;
                }
            }
        }
        Algorithm::Halving => {
            let mut alive = sample_points(&spec.dims, space, spec.budget, &mut rng);
            let mut runs = base_runs.max(1);
            let mut rung = 0;
            'rungs: loop {
                let mut scored = Vec::with_capacity(alive.len());
                for p in &alive {
                    let Some(v) = rec.run(p, rung, Some(runs))? else {
                        break 'rungs;
                    };
                    scored.push((p.clone(), v));
                }
                rungs.push((runs, alive.len()));
                if alive.len() <= 2 {
                    break;
                }
                scored.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
                let keep = alive.len().div_ceil(ETA as usize).max(2);
                alive = scored.into_iter().take(keep).map(|(p, _)| p).collect();
                runs = runs.saturating_mul(ETA);
                rung += 1;
            }
        }
        Algorithm::Bayes => {
            if exhaustive {
                for p in sample_points(&spec.dims, space, spec.budget, &mut rng) {
                    if rec.run(&p, 0, None)?.is_none() {
                        break;
                    }
                }
            } else {
                bayes(spec, space, &mut rng, &mut rec)?;
            }
        }
    }
    Ok(Outcome {
        trials: rec.trials,
        interrupted: rec.interrupted,
        exhaustive,
        rungs,
    })
}

fn bayes<F: Evaluate>(
    spec: &SearchSpec,
    space: u128,
    rng: &mut Rng,
    rec: &mut Recorder<F>,
) -> Result<()> {
    let initial = (2 * spec.dims.len() + 1).clamp(3, spec.budget);
    let mut seen: HashSet<Vec<i64>> = HashSet::new();
    for p in sample_points(&spec.dims, space, initial, rng) {
        seen.insert(p.clone());
        if rec.run(&p, 0, None)?.is_none() {
            return Ok(());
        }
    }
    while rec.trials.len() < spec.budget {
        let candidates = candidates(spec, space, &seen, rng);
        let next = suggest(spec, &rec.trials, &candidates)
            .or_else(|| candidates.first().cloned())
            .expect("the grid is larger than the budget");
        seen.insert(next.clone());
        if rec.run(&next, 0, None)?.is_none() {
            break;
        }
    }
    Ok(())
}

fn candidates(
    spec: &SearchSpec,
    space: u128,
    seen: &HashSet<Vec<i64>>,
    rng: &mut Rng,
) -> Vec<Vec<i64>> {
    if space <= FULL_GRID {
        return (0..space)
            .map(|i| decode(&spec.dims, i))
            .filter(|p| !seen.contains(p))
            .collect();
    }
    let mut out: Vec<Vec<i64>> = Vec::with_capacity(MAX_CANDIDATES);
    let mut tried = HashSet::new();
    // Stop once every point was drawn: fewer than MAX_CANDIDATES may remain unseen
    while out.len() < MAX_CANDIDATES && (tried.len() as u128) < space {
        let index = rng.below(space);
        if !tried.insert(index) {
            continue;
        }
        let p = decode(&spec.dims, index);
        if !seen.contains(&p) {
            out.push(p);
        }
    }
    out
}

fn normalized(spec: &SearchSpec, point: &[i64]) -> Vec<f64> {
    spec.dims
        .iter()
        .zip(point)
        .map(|(d, x)| {
            if d.max == d.min {
                0.0
            } else {
                (*x as f64 - d.min as f64) / (d.max as f64 - d.min as f64)
            }
        })
        .collect()
}

fn sq_dist(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum()
}

/// Lower Cholesky factor of a symmetric positive definite matrix (row-major `n x n`)
fn cholesky(a: &[f64], n: usize) -> Option<Vec<f64>> {
    let mut l = vec![0.0; n * n];
    for i in 0..n {
        for j in 0..=i {
            let mut sum = a[i * n + j];
            for k in 0..j {
                sum -= l[i * n + k] * l[j * n + k];
            }
            if i == j {
                if sum <= 1e-12 {
                    return None;
                }
                l[i * n + i] = sum.sqrt();
            } else {
                l[i * n + j] = sum / l[j * n + j];
            }
        }
    }
    Some(l)
}

/// Solve `L x = b` (forward substitution)
fn forward(l: &[f64], n: usize, b: &[f64]) -> Vec<f64> {
    let mut x = vec![0.0; n];
    for i in 0..n {
        let s: f64 = (0..i).map(|k| l[i * n + k] * x[k]).sum();
        x[i] = (b[i] - s) / l[i * n + i];
    }
    x
}

/// Solve `Lᵀ x = b` (back substitution)
fn backward(l: &[f64], n: usize, b: &[f64]) -> Vec<f64> {
    let mut x = vec![0.0; n];
    for i in (0..n).rev() {
        let s: f64 = (i + 1..n).map(|k| l[k * n + i] * x[k]).sum();
        x[i] = (b[i] - s) / l[i * n + i];
    }
    x
}

struct Gp {
    xs: Vec<Vec<f64>>,
    l: Vec<f64>,
    alpha: Vec<f64>,
    length: f64,
}

impl Gp {
    fn kernel(length: f64, a: &[f64], b: &[f64]) -> f64 {
        (-sq_dist(a, b) / (2.0 * length * length)).exp()
    }

    /// Fit with the length scale of the highest marginal likelihood
    fn fit(xs: &[Vec<f64>], ys: &[f64], noise: &[f64]) -> Option<Gp> {
        let n = xs.len();
        let mut best: Option<(f64, Gp)> = None;
        for length in [0.15, 0.3, 0.6, 1.2] {
            let mut k = vec![0.0; n * n];
            for i in 0..n {
                for j in 0..n {
                    k[i * n + j] = Self::kernel(length, &xs[i], &xs[j]);
                }
                k[i * n + i] += noise[i] + 1e-6;
            }
            let Some(l) = cholesky(&k, n) else { continue };
            let z = forward(&l, n, ys);
            let alpha = backward(&l, n, &z);
            let log_det: f64 = (0..n).map(|i| l[i * n + i].ln()).sum();
            let fit: f64 = ys.iter().zip(&alpha).map(|(y, a)| y * a).sum();
            let likelihood = -0.5 * fit - log_det;
            if best.as_ref().is_none_or(|(b, _)| likelihood > *b) {
                best = Some((
                    likelihood,
                    Gp {
                        xs: xs.to_vec(),
                        l,
                        alpha,
                        length,
                    },
                ));
            }
        }
        best.map(|(_, gp)| gp)
    }

    /// Posterior mean and standard deviation at `x`
    fn predict(&self, x: &[f64]) -> (f64, f64) {
        let n = self.xs.len();
        let k: Vec<f64> = self
            .xs
            .iter()
            .map(|xi| Self::kernel(self.length, xi, x))
            .collect();
        let mean: f64 = k.iter().zip(&self.alpha).map(|(a, b)| a * b).sum();
        let v = forward(&self.l, n, &k);
        let var = 1.0 - v.iter().map(|x| x * x).sum::<f64>();
        (mean, var.max(0.0).sqrt())
    }
}

fn normal_pdf(z: f64) -> f64 {
    (-0.5 * z * z).exp() / (2.0 * std::f64::consts::PI).sqrt()
}

fn normal_cdf(z: f64) -> f64 {
    0.5 * crate::stats::diagnostics::erfc(-z / std::f64::consts::SQRT_2)
}

/// Expected improvement of a point with posterior `(mean, sd)` over `best` (minimization)
fn expected_improvement(best: f64, mean: f64, sd: f64) -> f64 {
    if sd <= 1e-12 {
        return (best - mean).max(0.0);
    }
    let z = (best - mean) / sd;
    (best - mean) * normal_cdf(z) + sd * normal_pdf(z)
}

/// The candidate with the highest expected improvement; ties keep the earlier candidate.
/// `None` when the model cannot be fitted or every value is equal.
fn suggest(spec: &SearchSpec, trials: &[Trial], candidates: &[Vec<i64>]) -> Option<Vec<i64>> {
    let n = trials.len() as f64;
    let mean = trials.iter().map(|t| t.eval.value).sum::<f64>() / n;
    let sd = (trials
        .iter()
        .map(|t| (t.eval.value - mean).powi(2))
        .sum::<f64>()
        / n)
        .sqrt();
    if sd <= 0.0 || !sd.is_finite() {
        return None;
    }
    let xs: Vec<Vec<f64>> = trials.iter().map(|t| normalized(spec, &t.point)).collect();
    let ys: Vec<f64> = trials.iter().map(|t| (t.eval.value - mean) / sd).collect();
    let noise: Vec<f64> = trials.iter().map(|t| (t.eval.se / sd).powi(2)).collect();
    let gp = Gp::fit(&xs, &ys, &noise)?;
    let best = ys.iter().copied().fold(f64::INFINITY, f64::min);
    let mut top: Option<(f64, &Vec<i64>)> = None;
    for c in candidates {
        let (m, s) = gp.predict(&normalized(spec, c));
        let ei = expected_improvement(best, m, s);
        if top.is_none_or(|(b, _)| ei > b + 1e-15) {
            top = Some((ei, c));
        }
    }
    top.map(|(_, c)| c.clone())
}

/// Trials at the highest rung reached, best first (ties: the lower point)
pub fn ranking(outcome: &Outcome) -> Vec<&Trial> {
    let Some(top) = outcome.trials.iter().map(|t| t.rung).max() else {
        return Vec::new();
    };
    let mut last: BTreeMap<&Vec<i64>, &Trial> = BTreeMap::new();
    for t in outcome.trials.iter().filter(|t| t.rung == top) {
        last.insert(&t.point, t);
    }
    let mut trials: Vec<&Trial> = last.into_values().collect();
    trials.sort_by(|a, b| {
        a.eval
            .value
            .total_cmp(&b.eval.value)
            .then_with(|| a.point.cmp(&b.point))
    });
    trials
}

fn p_value(a: &Eval, b: &Eval) -> Option<f64> {
    crate::stats::deep::compare_samples(a.samples.as_deref()?, b.samples.as_deref()?)
        .map(|c| c.p_value)
}

/// Outcome of the independent validation round
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Validation {
    /// `confirmed`, `refuted` or `inconclusive`
    pub status: &'static str,
    /// Winner in the validation round
    pub value: f64,
    /// Winner in the search
    pub search_value: f64,
    /// `value / search_value - 1`: the winner's curse shows up as a positive number
    pub drift: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_up_value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p_value: Option<f64>,
    /// Benchmark runs of the validation round
    pub runs: u64,
}

/// Judge a fresh round of the winner (and the runner-up) against the search result.
/// `refuted`: the runner-up is significantly better in the fresh round.
pub fn validate(
    search_value: f64,
    fresh_best: &Eval,
    fresh_runner_up: Option<&Eval>,
) -> Validation {
    let p = fresh_runner_up.and_then(|r| p_value(fresh_best, r));
    let status = match (fresh_runner_up, p) {
        (Some(r), Some(p)) if p < ALPHA => {
            if fresh_best.value < r.value {
                "confirmed"
            } else {
                "refuted"
            }
        }
        (None, _) => "inconclusive",
        _ => "inconclusive",
    };
    Validation {
        status,
        value: fresh_best.value,
        search_value,
        drift: fresh_best.value / search_value - 1.0,
        runner_up_value: fresh_runner_up.map(|r| r.value),
        p_value: p,
        runs: fresh_best.runs + fresh_runner_up.map_or(0, |r| r.runs),
    }
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Cost {
    /// Benchmarks executed (a point benchmarked on two rungs counts twice)
    pub benchmarks: usize,
    /// Distinct points benchmarked; at most `budget`
    pub points: usize,
    /// Benchmark runs summed over all benchmarks, without the validation round
    pub runs: u64,
    /// `[{"runs": n, "points": m}]` per rung (successive halving only)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rungs: Vec<serde_json::Value>,
}

/// The `optimize` object of a multi-dimensional or non-golden search (additive, 1.37)
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct SearchReport {
    /// Present for one variable, as for the golden-section search
    #[serde(skip_serializing_if = "Option::is_none")]
    pub var: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<i64>,
    pub metric: String,
    pub algorithm: &'static str,
    pub budget: usize,
    pub evaluations: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub best: Option<Num>,
    pub best_value: f64,
    pub best_command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_up: Option<Num>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p_value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub significant: Option<bool>,
    pub at_boundary: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evaluated: Option<Vec<Num>>,
    /// The searched variables
    pub vars: Vec<Dim>,
    pub best_point: BTreeMap<String, i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_up_point: Option<BTreeMap<String, i64>>,
    pub evaluated_points: Vec<BTreeMap<String, i64>>,
    pub seed: u64,
    pub cost: Cost,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validation: Option<Validation>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub fixed: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub interrupted: bool,
    /// Best first, the points of the highest rung (kept for the terminal summary)
    #[serde(skip)]
    pub ranked: Vec<(Vec<i64>, Eval)>,
}

fn named(spec: &SearchSpec, point: &[i64]) -> BTreeMap<String, i64> {
    spec.dims
        .iter()
        .zip(point)
        .map(|(d, x)| (d.var.clone(), *x))
        .collect()
}

/// Evaluate a finished (or interrupted) search
pub fn analyze(
    spec: &SearchSpec,
    metric: Metric,
    outcome: &Outcome,
    validation: Option<Validation>,
    fixed: BTreeMap<String, String>,
) -> Option<SearchReport> {
    let ranked = ranking(outcome);
    let best = *ranked.first()?;
    let runner_up = ranked.get(1).copied();
    let comparison = runner_up.and_then(|r| p_value(&best.eval, &r.eval));
    let mut distinct: Vec<&Vec<i64>> = Vec::new();
    for t in &outcome.trials {
        if !distinct.contains(&&t.point) {
            distinct.push(&t.point);
        }
    }
    let single = spec.dims.len() == 1;
    let dim = &spec.dims[0];
    let coordinate = |p: &[i64]| Num::Int(p[0]);
    Some(SearchReport {
        var: single.then(|| dim.var.clone()),
        min: single.then_some(dim.min),
        max: single.then_some(dim.max),
        metric: metric.name().to_string(),
        algorithm: if outcome.exhaustive {
            "exhaustive"
        } else {
            spec.algorithm.name()
        },
        budget: spec.budget,
        evaluations: distinct.len(),
        best: single.then(|| coordinate(&best.point)),
        best_value: best.eval.value,
        best_command: best.eval.command.clone(),
        runner_up: runner_up.filter(|_| single).map(|r| coordinate(&r.point)),
        p_value: comparison,
        significant: comparison.map(|p| p < ALPHA),
        at_boundary: spec.at_boundary(&best.point),
        evaluated: single.then(|| distinct.iter().map(|p| coordinate(p)).collect()),
        vars: spec.dims.clone(),
        best_point: named(spec, &best.point),
        runner_up_point: runner_up.map(|r| named(spec, &r.point)),
        evaluated_points: distinct.iter().map(|p| named(spec, p)).collect(),
        seed: spec.seed,
        cost: Cost {
            benchmarks: outcome.trials.len(),
            points: distinct.len(),
            runs: outcome.trials.iter().map(|t| t.eval.runs).sum(),
            rungs: outcome
                .rungs
                .iter()
                .map(|(runs, points)| serde_json::json!({ "runs": runs, "points": points }))
                .collect(),
        },
        validation,
        fixed,
        interrupted: outcome.interrupted,
        ranked: ranked
            .into_iter()
            .map(|t| (t.point.clone(), t.eval.clone()))
            .collect(),
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

fn label(spec: &SearchSpec, point: &[i64]) -> String {
    spec.dims
        .iter()
        .zip(point)
        .map(|(d, x)| format!("{}={x}", d.var))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Terminal summary of one search; points are listed best first.
pub fn summary(spec: &SearchSpec, metric: Metric, report: &SearchReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let space: Vec<String> = spec
        .dims
        .iter()
        .map(|d| format!("{} ∈ [{}, {}]", d.var, d.min, d.max))
        .collect();
    let _ = writeln!(
        out,
        "Optimization of {} for {} ({} search, {} points, {} runs{}):",
        space.join(", "),
        metric.name(),
        report.algorithm,
        report.cost.points,
        report.cost.runs,
        if report.interrupted {
            ", interrupted"
        } else {
            ""
        }
    );
    for (i, (point, eval)) in report.ranked.iter().take(SUMMARY_ROWS).enumerate() {
        let _ = writeln!(
            out,
            "  {}  {} ± {}{}",
            label(spec, point),
            format_value(metric, eval.value),
            format_value(metric, eval.se),
            if i == 0 { "   ← best" } else { "" }
        );
    }
    if report.ranked.len() > SUMMARY_ROWS {
        let _ = writeln!(out, "  … {} more", report.ranked.len() - SUMMARY_ROWS);
    }
    let best = &report.ranked[0];
    let _ = write!(
        out,
        "Best: {} ({} {}).",
        label(spec, &best.0),
        metric.name(),
        format_value(metric, best.1.value)
    );
    if let Some((point, _)) = report.ranked.get(1) {
        match (report.p_value, report.significant) {
            (Some(p), Some(sig)) => {
                let _ = write!(
                    out,
                    " The difference from {} is {}significant (p={p:.2}).",
                    label(spec, point),
                    if sig { "" } else { "not " }
                );
            }
            _ => {
                let _ = write!(
                    out,
                    " The significance of the difference from {} is not computable.",
                    label(spec, point)
                );
            }
        }
    }
    if let Some(v) = &report.validation {
        let _ = write!(
            out,
            "\nValidation in a fresh round: {} ({} {}, {:+.1}% from the search",
            v.status,
            metric.name(),
            format_value(metric, v.value),
            v.drift * 100.0
        );
        if let Some(p) = v.p_value {
            let _ = write!(out, ", p={p:.2} against the runner-up");
        }
        out.push_str(").");
    }
    if report.at_boundary {
        out.push_str(
            "\nThe best point is on the boundary of the range; the optimum may lie outside it.",
        );
    }
    if report.algorithm == "exhaustive" {
        let _ = write!(
            out,
            "\nThe grid fits into the budget of {}, so all points were evaluated.",
            report.budget
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(dims: &[(&str, i64, i64)], algorithm: Algorithm, budget: usize) -> SearchSpec {
        SearchSpec {
            dims: dims
                .iter()
                .map(|(v, a, b)| Dim {
                    var: v.to_string(),
                    min: *a,
                    max: *b,
                })
                .collect(),
            algorithm,
            budget,
            seed: 7,
            validate: false,
        }
    }

    #[test]
    fn candidates_terminate_when_few_points_are_unseen() {
        let s = spec(&[("A", 1, 5000)], Algorithm::Bayes, 4000);
        let seen: HashSet<Vec<i64>> = (1..=4000).map(|i| vec![i]).collect();
        let c = candidates(&s, 5000, &seen, &mut Rng(1));
        assert_eq!(c.len(), 1000);
        assert!(c.iter().all(|p| !seen.contains(p)));
    }

    fn eval(value: f64, runs: u64) -> Eval {
        Eval {
            value,
            se: 0.0,
            samples: None,
            wall_mean: value,
            command: String::new(),
            runs,
        }
    }

    /// A smooth bowl with its minimum at (13, 4)
    fn bowl(p: &[i64]) -> f64 {
        let (a, b) = (p[0] as f64, p[1] as f64);
        1.0 + (a - 13.0).powi(2) / 100.0 + (b - 4.0).powi(2) / 10.0
    }

    fn run(s: &SearchSpec, f: impl Fn(&[i64], u64) -> f64) -> Outcome {
        run_search(s, 4, |p: &[i64], runs: Option<u64>| {
            let runs = runs.unwrap_or(4);
            Ok(Some(eval(f(p, runs), runs)))
        })
        .unwrap()
    }

    fn best(o: &Outcome) -> Vec<i64> {
        ranking(o)[0].point.clone()
    }

    #[test]
    fn decode_walks_the_grid_row_major() {
        let s = spec(&[("a", 1, 2), ("b", 10, 12)], Algorithm::Random, 3);
        let all: Vec<_> = (0..s.space()).map(|i| decode(&s.dims, i)).collect();
        assert_eq!(all.len(), 6);
        assert_eq!(all[0], [1, 10]);
        assert_eq!(all[1], [1, 11]);
        assert_eq!(all[3], [2, 10]);
        assert_eq!(all[5], [2, 12]);
    }

    #[test]
    fn random_search_is_deterministic_distinct_and_within_budget() {
        let s = spec(&[("a", 1, 30), ("b", 1, 30)], Algorithm::Random, 20);
        let one = run(&s, |p, _| bowl(p));
        let two = run(&s, |p, _| bowl(p));
        assert_eq!(one, two);
        assert_eq!(one.trials.len(), 20);
        let distinct: HashSet<_> = one.trials.iter().map(|t| t.point.clone()).collect();
        assert_eq!(distinct.len(), 20);
        let other = run(&SearchSpec { seed: 8, ..s }, |p, _| bowl(p));
        assert_ne!(one.trials, other.trials);
    }

    #[test]
    fn a_grid_inside_the_budget_is_evaluated_completely() {
        for algorithm in [Algorithm::Random, Algorithm::Halving, Algorithm::Bayes] {
            let s = spec(&[("a", 1, 3), ("b", 1, 4)], algorithm, 12);
            let o = run(&s, |p, _| bowl(p));
            assert!(o.exhaustive, "{algorithm:?}");
            assert_eq!(best(&o), vec![3, 4]);
        }
    }

    #[test]
    fn halving_spends_more_runs_on_fewer_points() {
        let s = spec(&[("a", 1, 40), ("b", 1, 10)], Algorithm::Halving, 18);
        let o = run(&s, |p, _| bowl(p));
        // 18 points at 4 runs, 6 at 12, 2 at 36
        assert_eq!(o.rungs, [(4, 18), (12, 6), (36, 2)]);
        assert_eq!(o.trials.len(), 26);
        let distinct: HashSet<_> = o.trials.iter().map(|t| t.point.clone()).collect();
        assert_eq!(distinct.len(), 18);
        let top = ranking(&o);
        assert_eq!(top.len(), 2);
        assert!(top.iter().all(|t| t.rung == 2 && t.eval.runs == 36));
    }

    #[test]
    fn halving_keeps_the_points_that_look_best() {
        // the winner of the last rung must be among the best of the first
        let s = spec(&[("a", 1, 40), ("b", 1, 10)], Algorithm::Halving, 27);
        let o = run(&s, |p, _| bowl(p));
        let mut first: Vec<_> = o.trials.iter().filter(|t| t.rung == 0).collect();
        first.sort_by(|a, b| a.eval.value.total_cmp(&b.eval.value));
        let winner = best(&o);
        assert!(first.iter().take(9).any(|t| t.point == winner));
    }

    #[test]
    fn bayes_beats_random_on_a_smooth_surface() {
        let s = spec(&[("a", 1, 64), ("b", 1, 16)], Algorithm::Bayes, 24);
        let o = run(&s, |p, _| bowl(p));
        assert_eq!(o.trials.len(), 24);
        let distinct: HashSet<_> = o.trials.iter().map(|t| t.point.clone()).collect();
        assert_eq!(distinct.len(), 24);
        let found = bowl(&best(&o));
        // The optimum is 1.0
        assert!(
            found <= 1.06,
            "bayesian search found {found} at {:?}",
            best(&o)
        );
        // reproducible
        assert_eq!(o, run(&s, |p, _| bowl(p)));
    }

    #[test]
    fn bayes_handles_a_flat_surface_and_one_dimension() {
        let s = spec(&[("a", 1, 100)], Algorithm::Bayes, 10);
        let flat = run(&s, |_, _| 2.0);
        assert_eq!(flat.trials.len(), 10);
        let parabola = run(&s, |p, _| (p[0] as f64 - 61.0).powi(2) + 1.0);
        assert!(
            (best(&parabola)[0] - 61).abs() <= 6,
            "{:?}",
            best(&parabola)
        );
    }

    #[test]
    fn bayes_searches_a_grid_too_large_to_enumerate() {
        let s = spec(&[("a", 1, 1000), ("b", 1, 1000)], Algorithm::Bayes, 14);
        let o = run(&s, |p, _| bowl(p));
        assert_eq!(o.trials.len(), 14);
    }

    #[test]
    fn interruption_returns_the_partial_trials() {
        for algorithm in [Algorithm::Random, Algorithm::Halving, Algorithm::Bayes] {
            let s = spec(&[("a", 1, 30), ("b", 1, 30)], algorithm, 12);
            let mut n = 0;
            let o = run_search(&s, 2, |p: &[i64], _| {
                n += 1;
                Ok((n <= 5).then(|| eval(bowl(p), 2)))
            })
            .unwrap();
            assert!(o.interrupted, "{algorithm:?}");
            assert_eq!(o.trials.len(), 5);
        }
    }

    #[test]
    fn errors_propagate() {
        let s = spec(&[("a", 1, 30)], Algorithm::Random, 5);
        let r = run_search(&s, 2, |_: &[i64], _| anyhow::bail!("boom"));
        assert!(r.is_err());
    }

    #[test]
    fn ties_pick_the_lower_point() {
        let s = spec(&[("a", 1, 5), ("b", 1, 5)], Algorithm::Random, 25);
        let o = run(&s, |_, _| 1.0);
        assert_eq!(best(&o), [1, 1]);
    }

    #[test]
    fn analysis_names_best_runner_up_cost_and_boundary() {
        let s = spec(&[("a", 1, 3), ("b", 1, 3)], Algorithm::Random, 12);
        let o = run(&s, |p, _| (p[0] * 10 + p[1]) as f64);
        let r = analyze(&s, Metric::Wall, &o, None, BTreeMap::new()).unwrap();
        assert_eq!(r.algorithm, "exhaustive");
        assert_eq!(r.best_point["a"], 1);
        assert_eq!(r.best_point["b"], 1);
        assert_eq!(r.runner_up_point.as_ref().unwrap()["b"], 2);
        assert!(r.at_boundary);
        assert_eq!(r.evaluations, 9);
        assert_eq!(r.cost.runs, 36);
        assert!(r.var.is_none() && r.best.is_none());
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["vars"][1]["name"], "b");
        assert_eq!(json["evaluated_points"].as_array().unwrap().len(), 9);
        assert!(json.get("ranked").is_none());
    }

    #[test]
    fn one_variable_keeps_the_golden_section_fields() {
        let s = spec(&[("n", 1, 50)], Algorithm::Random, 8);
        let o = run(&s, |p, _| (p[0] as f64 - 20.0).abs() + 1.0);
        let r = analyze(&s, Metric::Wall, &o, None, BTreeMap::new()).unwrap();
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["var"], "n");
        assert_eq!(json["min"], 1);
        assert_eq!(json["evaluated"].as_array().unwrap().len(), 8);
        assert_eq!(json["best"], r.best_point["n"]);
    }

    fn fresh(mean: f64, runs: u64) -> Eval {
        let samples: Vec<f64> = (0..runs).map(|i| mean + (i % 3) as f64 * 0.001).collect();
        Eval {
            samples: Some(samples),
            ..eval(mean, runs)
        }
    }

    #[test]
    fn validation_confirms_refutes_or_stays_inconclusive() {
        let v = validate(1.0, &fresh(1.0, 20), Some(&fresh(1.5, 20)));
        assert_eq!(v.status, "confirmed");
        assert_eq!(v.runs, 40);
        assert!(v.drift.abs() < 1e-9);
        let v = validate(1.0, &fresh(1.5, 20), Some(&fresh(1.0, 20)));
        assert_eq!(v.status, "refuted");
        assert!((v.drift - 0.5).abs() < 1e-9);
        let v = validate(1.0, &eval(1.0, 3), Some(&eval(1.5, 3)));
        assert_eq!(v.status, "inconclusive");
        assert_eq!(validate(1.0, &fresh(1.0, 20), None).status, "inconclusive");
    }

    #[test]
    fn the_summary_lists_the_best_first() {
        let s = spec(&[("a", 1, 3), ("b", 1, 3)], Algorithm::Random, 12);
        let o = run(&s, |p, _| (p[0] * 10 + p[1]) as f64);
        let r = analyze(&s, Metric::Wall, &o, None, BTreeMap::new()).unwrap();
        let text = summary(&s, Metric::Wall, &r);
        assert!(text.contains("a=1 b=1"));
        assert!(text.contains("← best"));
        assert!(text.contains("boundary"));
    }

    #[test]
    fn cholesky_solves_a_small_system() {
        let a = [4.0, 2.0, 2.0, 3.0];
        let l = cholesky(&a, 2).unwrap();
        let x = backward(&l, 2, &forward(&l, 2, &[2.0, 1.0]));
        // A x = b
        assert!((4.0 * x[0] + 2.0 * x[1] - 2.0).abs() < 1e-12);
        assert!((2.0 * x[0] + 3.0 * x[1] - 1.0).abs() < 1e-12);
        assert!(cholesky(&[1.0, 2.0, 2.0, 1.0], 2).is_none());
    }

    #[test]
    fn expected_improvement_is_zero_without_uncertainty_or_gain() {
        assert_eq!(expected_improvement(0.0, 1.0, 0.0), 0.0);
        assert!(expected_improvement(0.0, 1.0, 0.5) > 0.0);
        assert!(expected_improvement(0.0, -1.0, 0.5) > expected_improvement(0.0, 0.0, 0.5));
        assert!((normal_cdf(0.0) - 0.5).abs() < 1e-7);
    }
}
