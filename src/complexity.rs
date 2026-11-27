//! `--complexity <VAR>`: empirical complexity fit over a parameter scan (#374,
//! `docs/specs/complexity-fit.md`).
//!
//! For every command template and metric the measured means `y(n)` are fitted
//! by least squares with each of the models `c`, `c·log n`, `c·n`,
//! `c·n·log n`, `c·n²` and `c·n³` (one coefficient, no offset, as Google
//! Benchmark does). The model with the smallest RMS error is reported as the
//! best one. This is an empirical fit of a handful of noisy points, not a
//! proof of the asymptotic complexity of the program.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::benchmark::benchmark_result::BenchmarkResult;
use crate::metric::Metric;

/// Fewest distinct parameter values a fit is attempted for
pub const MIN_POINTS: usize = 3;

/// A relative improvement of the RMS error below this does not displace a
/// simpler model.
const TIE: f64 = 1e-9;

/// Growth models, from the simplest to the steepest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Model {
    #[serde(rename = "O(1)")]
    Constant,
    #[serde(rename = "O(log n)")]
    Log,
    #[serde(rename = "O(n)")]
    Linear,
    #[serde(rename = "O(n log n)")]
    NLogN,
    #[serde(rename = "O(n^2)")]
    Quadratic,
    #[serde(rename = "O(n^3)")]
    Cubic,
}

impl Model {
    pub const ALL: [Model; 6] = [
        Model::Constant,
        Model::Log,
        Model::Linear,
        Model::NLogN,
        Model::Quadratic,
        Model::Cubic,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Model::Constant => "O(1)",
            Model::Log => "O(log n)",
            Model::Linear => "O(n)",
            Model::NLogN => "O(n log n)",
            Model::Quadratic => "O(n^2)",
            Model::Cubic => "O(n^3)",
        }
    }

    /// The basis function at `n` (natural logarithm; the base only scales the coefficient)
    pub fn basis(self, n: f64) -> f64 {
        match self {
            Model::Constant => 1.0,
            Model::Log => n.ln(),
            Model::Linear => n,
            Model::NLogN => n * n.ln(),
            Model::Quadratic => n * n,
            Model::Cubic => n * n * n,
        }
    }
}

/// One fitted model
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ModelFit {
    pub model: Model,
    /// `c` of `y = c·g(n)`
    pub coefficient: f64,
    /// Root mean square error, in the unit of the metric
    pub rms: f64,
    /// `rms` divided by the mean of the data (Google Benchmark's "RMS")
    pub rms_relative: f64,
    /// `1 - SSres/SStot`; can be negative for a model without offset that fits worse than the mean
    pub r2: f64,
}

/// The fits of one metric
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MetricFit {
    pub unit: &'static str,
    pub best: Model,
    pub coefficient: f64,
    pub rms: f64,
    pub rms_relative: f64,
    pub r2: f64,
    /// Every model, in the order O(1) to O(n^3)
    pub models: Vec<ModelFit>,
}

/// The analysis of one command template
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ComplexityReport {
    pub parameter: String,
    /// The command with `{VAR}` in place of the value
    pub command: String,
    /// Other parameters, which are equal for all points of the fit
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub fixed: BTreeMap<String, String>,
    /// The parameter values, ascending
    pub values: Vec<f64>,
    /// Fits by metric name: `wall`, and `memory`, `energy`, `instructions` when every point has them
    pub metrics: BTreeMap<String, MetricFit>,
    pub note: &'static str,
}

pub const NOTE: &str = "Empirical least-squares fit over the measured points, not a proof of the \
                        asymptotic complexity.";

/// Least-squares fit of every model to the points `(n, y)`.
/// `None` for fewer than [`MIN_POINTS`] points, or non-finite or non-positive `n`.
pub fn fit(points: &[(f64, f64)], unit: &'static str) -> Option<MetricFit> {
    if points.len() < MIN_POINTS
        || points
            .iter()
            .any(|(n, y)| !n.is_finite() || *n <= 0.0 || !y.is_finite())
    {
        return None;
    }
    let count = points.len() as f64;
    let mean = points.iter().map(|p| p.1).sum::<f64>() / count;
    let total: f64 = points.iter().map(|p| (p.1 - mean).powi(2)).sum();
    let mut models = Vec::new();
    for model in Model::ALL {
        let (mut gy, mut gg) = (0.0, 0.0);
        for &(n, y) in points {
            let g = model.basis(n);
            gy += g * y;
            gg += g * g;
        }
        if gg == 0.0 {
            continue;
        }
        let c = gy / gg;
        let residual: f64 = points
            .iter()
            .map(|&(n, y)| (y - c * model.basis(n)).powi(2))
            .sum();
        let rms = (residual / count).sqrt();
        models.push(ModelFit {
            model,
            coefficient: c,
            rms,
            rms_relative: if mean != 0.0 { rms / mean.abs() } else { 0.0 },
            r2: if total > 0.0 {
                1.0 - residual / total
            } else if residual == 0.0 {
                1.0
            } else {
                0.0
            },
        });
    }
    let mut best = models.first()?.clone();
    for m in &models[1..] {
        if m.rms < best.rms * (1.0 - TIE) {
            best = m.clone();
        }
    }
    Some(MetricFit {
        unit,
        best: best.model,
        coefficient: best.coefficient,
        rms: best.rms,
        rms_relative: best.rms_relative,
        r2: best.r2,
        models,
    })
}

const FITTED: [(&str, Metric, &str); 4] = [
    ("wall", Metric::Wall, "s"),
    ("memory", Metric::Memory, "B"),
    ("energy", Metric::Energy, "J"),
    ("instructions", Metric::Instructions, "instructions"),
];

type Group<'a> = (
    String,
    BTreeMap<String, String>,
    Vec<(f64, &'a BenchmarkResult)>,
);

/// Groups `results` by command template (everything except the value of `var`) and fits each group.
/// Groups with fewer than [`MIN_POINTS`] numeric values are returned in the second list as warnings.
pub fn analyze(var: &str, results: &[BenchmarkResult]) -> (Vec<ComplexityReport>, Vec<String>) {
    let mut groups: Vec<Group> = Vec::new();
    let mut warnings = Vec::new();
    for result in results {
        let Some(text) = result.parameters.get(var) else {
            continue;
        };
        let Ok(n) = text.trim().parse::<f64>() else {
            warnings.push(format!(
                "'--complexity {var}': the value '{text}' of '{}' is not a number; skipped",
                result.command
            ));
            continue;
        };
        let template = result.command.replace(text.as_str(), &format!("{{{var}}}"));
        let mut fixed = result.parameters.clone();
        fixed.remove(var);
        match groups
            .iter_mut()
            .find(|(t, f, _)| *t == template && *f == fixed)
        {
            Some(group) => group.2.push((n, result)),
            None => groups.push((template, fixed, vec![(n, result)])),
        }
    }
    let mut reports = Vec::new();
    for (command, fixed, mut points) in groups {
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        points.dedup_by(|a, b| a.0 == b.0);
        let mut metrics = BTreeMap::new();
        for (name, metric, unit) in FITTED {
            let data: Option<Vec<(f64, f64)>> = points
                .iter()
                .map(|(n, r)| r.primary_center(metric).map(|y| (*n, y)))
                .collect();
            if let Some(f) = data.and_then(|d| fit(&d, unit)) {
                metrics.insert(name.to_string(), f);
            }
        }
        if !metrics.contains_key("wall") {
            warnings.push(format!(
                "'--complexity {var}': '{command}' has {} usable value(s) of '{var}' (need at \
                 least {MIN_POINTS} positive ones); no fit",
                points.len()
            ));
            continue;
        }
        reports.push(ComplexityReport {
            parameter: var.to_string(),
            command,
            fixed,
            values: points.iter().map(|p| p.0).collect(),
            metrics,
            note: NOTE,
        });
    }
    (reports, warnings)
}

fn significant(value: f64) -> String {
    if value != 0.0 && (value.abs() < 1e-3 || value.abs() >= 1e6) {
        format!("{value:.3e}")
    } else {
        format!("{value:.4}")
    }
}

/// Terminal summary
pub fn terminal(reports: &[ComplexityReport]) -> String {
    let mut out = String::new();
    for r in reports {
        out.push_str(&format!(
            "\nComplexity of '{}' over {} ({} points):\n",
            r.command,
            r.parameter,
            r.values.len()
        ));
        for (name, m) in &r.metrics {
            out.push_str(&format!(
                "  {name:<13} {:<11} coefficient {} {}, RMS {:.1}%, R² {:.4}\n",
                m.best.name(),
                significant(m.coefficient),
                m.unit,
                m.rms_relative * 100.0,
                m.r2
            ));
        }
    }
    if !out.is_empty() {
        out.push_str(&format!("\nNote: {NOTE}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn points(f: impl Fn(f64) -> f64) -> Vec<(f64, f64)> {
        [10.0, 20.0, 40.0, 80.0, 160.0, 320.0, 640.0]
            .iter()
            .map(|&n| (n, f(n)))
            .collect()
    }

    fn best(f: impl Fn(f64) -> f64) -> MetricFit {
        fit(&points(f), "s").unwrap()
    }

    #[test]
    fn recovers_each_model() {
        type Case = (Model, f64, fn(f64) -> f64);
        let cases: [Case; 6] = [
            (Model::Constant, 3.0, |_| 3.0),
            (Model::Log, 2.5, |n| 2.5 * n.ln()),
            (Model::Linear, 0.5, |n| 0.5 * n),
            (Model::NLogN, 0.25, |n| 0.25 * n * n.ln()),
            (Model::Quadratic, 0.001, |n| 0.001 * n * n),
            (Model::Cubic, 1e-6, |n| 1e-6 * n * n * n),
        ];
        for (model, c, f) in cases {
            let fit = best(f);
            assert_eq!(fit.best, model, "{model:?}");
            assert!(
                (fit.coefficient - c).abs() < 1e-9 * c.abs().max(1.0),
                "{model:?}"
            );
            assert!(fit.rms < 1e-6, "{model:?}");
            assert!((fit.r2 - 1.0).abs() < 1e-9);
            assert_eq!(fit.models.len(), 6);
        }
    }

    #[test]
    fn tolerates_noise() {
        let noise = [1.02, 0.97, 1.03, 0.99, 1.01, 0.98, 1.02];
        let data: Vec<(f64, f64)> = points(|n| 0.001 * n * n)
            .into_iter()
            .zip(noise)
            .map(|((n, y), k)| (n, y * k))
            .collect();
        let fit = fit(&data, "s").unwrap();
        assert_eq!(fit.best, Model::Quadratic);
        assert!(fit.rms_relative < 0.05 && fit.r2 > 0.99);
    }

    #[test]
    fn rejects_unusable_points() {
        assert!(fit(&[(1.0, 1.0), (2.0, 2.0)], "s").is_none());
        assert!(fit(&[(0.0, 1.0), (2.0, 2.0), (3.0, 3.0)], "s").is_none());
        assert!(fit(&[(1.0, f64::NAN), (2.0, 2.0), (3.0, 3.0)], "s").is_none());
    }

    #[test]
    fn analyze_groups_by_template_and_reads_means() {
        let result = |n: u32, mean: f64, other: &str| {
            let mut r = BenchmarkResult {
                command: format!("work {n} {other}"),
                mean,
                ..Default::default()
            };
            r.parameters.insert("n".into(), n.to_string());
            r.parameters.insert("k".into(), other.into());
            r
        };
        let mut results = Vec::new();
        for n in [100, 200, 400, 800] {
            results.push(result(n, 1e-5 * f64::from(n), "a"));
            results.push(result(n, 3.0, "b"));
        }
        let (reports, warnings) = analyze("n", &results);
        assert!(warnings.is_empty());
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].command, "work {n} a");
        assert_eq!(reports[0].metrics["wall"].best, Model::Linear);
        assert_eq!(reports[1].metrics["wall"].best, Model::Constant);
        assert_eq!(reports[0].fixed["k"], "a");
        assert_eq!(reports[0].values, vec![100.0, 200.0, 400.0, 800.0]);
        assert!(!reports[0].metrics.contains_key("energy"));
        assert!(terminal(&reports).contains("O(n)"));
    }

    #[test]
    fn analyze_warns_on_few_or_non_numeric_values() {
        let mut r = BenchmarkResult {
            command: "w x".into(),
            mean: 1.0,
            ..Default::default()
        };
        r.parameters.insert("n".into(), "x".into());
        let (reports, warnings) = analyze("n", &[r]);
        assert!(reports.is_empty());
        assert_eq!(warnings.len(), 1);
    }
}
