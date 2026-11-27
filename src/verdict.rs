//! `--verdict-json FILE`: a compact, machine-readable verdict of a `--compare` run, for agents
//! and scripts that decide "improved, regressed or trade-off" without parsing terminal text
//! (#386, `docs/specs/verdict-json.md`).
//!
//! The document has its own version (`verdictVersion`) and is independent of the
//! `schemaVersion` of the measurement export. It is built from a [`Comparison`] and the
//! budget report of `--budget`; no measurement happens here.

use serde::Serialize;

use crate::benchmark::benchmark_result::BenchmarkResult;
use crate::budget::{Evaluation, Report};
use crate::compare::{Comparison, Delta};
use crate::metric::Metric;
use crate::pareto::{self, Direction};
use crate::stats::dist::t_critical;
use crate::stats::effect::{tost_p_value, welch};
use crate::stats::settings::StatSettings;
use crate::stats::significance::MIN_EFFECT;

/// Version of the verdict document. Additive changes raise the minor number.
pub const VERDICT_VERSION: &str = "1.0";

/// Outcome of one metric of one command
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Assessment {
    /// Significantly better, beyond the noise threshold
    Improved,
    /// Significantly worse, beyond the noise threshold
    Regressed,
    /// The change is below the noise threshold
    Unchanged,
    /// Within `--equivalence-margin` (TOST)
    Equivalent,
    /// No difference is established and neither is equivalence; more runs are needed
    Inconclusive,
    /// No data on one side; see `reason`
    NotEvaluated,
}

/// Outcome of a command, or of the whole comparison
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Overall {
    Improved,
    Regressed,
    /// At least one metric improved and at least one regressed
    TradeOff,
    Unchanged,
    Inconclusive,
    NotEvaluated,
}

impl Overall {
    fn combine(assessments: impl IntoIterator<Item = Assessment>) -> Self {
        let (mut better, mut worse, mut flat, mut open) = (false, false, false, false);
        for a in assessments {
            match a {
                Assessment::Improved => better = true,
                Assessment::Regressed => worse = true,
                Assessment::Unchanged | Assessment::Equivalent => flat = true,
                Assessment::Inconclusive => open = true,
                Assessment::NotEvaluated => {}
            }
        }
        match (better, worse) {
            (true, true) => Overall::TradeOff,
            (true, false) => Overall::Improved,
            (false, true) => Overall::Regressed,
            _ if open => Overall::Inconclusive,
            _ if flat => Overall::Unchanged,
            _ => Overall::NotEvaluated,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Runs {
    pub baseline: usize,
    pub current: usize,
}

/// One metric of one command
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MetricVerdict {
    pub metric: String,
    pub direction: Direction,
    pub assessment: Assessment,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<f64>,
    /// `current / baseline - 1` of the estimator (0.05 = 5% higher)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change: Option<f64>,
    /// Interval of `change` at `settings.confidence_level`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci_low: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci_high: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p_value: Option<f64>,
    /// p-value of the equivalence test, with `--equivalence-margin` and the mean
    #[serde(skip_serializing_if = "Option::is_none")]
    pub equivalence_p_value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runs: Option<Runs>,
    /// Why the assessment is `inconclusive` or `not_evaluated`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CommandVerdict {
    pub command: String,
    pub verdict: Overall,
    pub metrics: Vec<MetricVerdict>,
    /// The budgets of `--budget` that apply to this command
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub budgets: Vec<Evaluation>,
}

/// Result of the budgets of `--budget` / `--fail-if-regressed`
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Gate {
    /// `pass`, `fail` or `not_evaluated`
    pub verdict: &'static str,
    /// Budgets that fail the gate (the exit code 3 condition)
    pub failures: usize,
    pub on_unavailable: crate::budget::OnUnavailable,
    /// Budgets of commands that are not in the comparison (absolute budgets of new commands)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub other_budgets: Vec<Evaluation>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Settings {
    pub estimator: String,
    pub confidence_level: f64,
    pub significance_level: f64,
    /// Fraction; the floor below which a change is `unchanged` (default 0.01)
    pub noise_threshold: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub equivalence_margin: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Tool {
    pub name: &'static str,
    pub version: &'static str,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Summary {
    pub improved: usize,
    pub regressed: usize,
    pub trade_off: usize,
    pub unchanged: usize,
    pub inconclusive: usize,
    pub not_evaluated: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Document {
    /// First key, as `schemaVersion` of the export
    #[serde(rename = "verdictVersion")]
    pub verdict_version: &'static str,
    pub tool: Tool,
    pub baseline: String,
    pub settings: Settings,
    /// Worst-case combination over the commands: `trade_off` if any metric improved and any
    /// regressed (in the same or in different commands)
    pub verdict: Overall,
    pub summary: Summary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate: Option<Gate>,
    pub commands: Vec<CommandVerdict>,
    /// Commands without a baseline
    pub new: Vec<String>,
    /// Baseline commands that were not benchmarked
    pub removed: Vec<String>,
}

/// Metrics assessed when both sides have data (a custom metric when both sides have it)
fn metrics_of(delta: &Delta<'_>) -> Vec<Metric> {
    let mut metrics = vec![
        Metric::Wall,
        Metric::Cpu,
        Metric::Memory,
        Metric::Energy,
        Metric::Instructions,
        Metric::Cycles,
    ];
    let names = |r: &BenchmarkResult| -> Vec<String> {
        r.custom_metrics_summary
            .as_ref()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    };
    let current = names(delta.current);
    for name in names(delta.baseline) {
        if current.contains(&name) {
            metrics.push(Metric::custom(&name));
        }
    }
    metrics
}

fn assess_metric(
    delta: &Delta<'_>,
    metric: Metric,
    settings: &StatSettings,
) -> Option<MetricVerdict> {
    let (base, cur) = (delta.baseline, delta.current);
    let estimator = settings.estimator;
    let b = base.primary_center_with(metric, estimator);
    let c = cur.primary_center_with(metric, estimator);
    let direction = pareto::direction(metric);
    let mut out = MetricVerdict {
        metric: metric.name().to_string(),
        direction,
        assessment: Assessment::NotEvaluated,
        baseline: b,
        current: c,
        change: None,
        ci_low: None,
        ci_high: None,
        p_value: None,
        equivalence_p_value: None,
        runs: None,
        reason: None,
    };
    let (b, c) = match (b, c) {
        (None, None) => return None,
        (Some(b), Some(c)) if b.is_finite() && c.is_finite() => (b, c),
        (None, _) => {
            out.reason = Some("unavailable_in_baseline");
            return Some(out);
        }
        (_, None) => {
            out.reason = Some("unavailable_in_current");
            return Some(out);
        }
        _ => {
            out.reason = Some("not_finite");
            return Some(out);
        }
    };
    if b <= 0.0 {
        out.reason = Some("baseline_not_positive");
        return Some(out);
    }
    let change = c / b - 1.0;
    out.change = Some(change);

    let samples = (base.primary_samples(metric), cur.primary_samples(metric));
    let (a_s, b_s) = match &samples {
        (Some(a), Some(b)) => (a.as_slice(), b.as_slice()),
        _ => (&[][..], &[][..]),
    };
    out.runs = Some(Runs {
        baseline: a_s.len(),
        current: b_s.len(),
    });
    // The wall time keeps the p-value of `--compare` (it accounts for `--subtract`)
    out.p_value = if metric == Metric::Wall {
        delta.p_value
    } else {
        crate::compare::samples_p_value(a_s, b_s)
    };
    if a_s.len() >= 3 && b_s.len() >= 3 {
        if settings.estimator.is_mean() {
            if let Some(w) = welch(a_s, b_s) {
                let half = t_critical(settings.confidence_level, w.df) * w.se;
                out.ci_low = Some((w.difference - half) / b);
                out.ci_high = Some((w.difference + half) / b);
                if let Some(margin) = settings.equivalence_margin {
                    out.equivalence_p_value = Some(tost_p_value(&w, margin * b));
                }
            }
        } else if let Some((lo, hi)) =
            crate::stats::deep::difference_confidence_interval(a_s, b_s, settings)
        {
            out.ci_low = Some(lo / b);
            out.ci_high = Some(hi / b);
        }
    }

    let Some(p) = out.p_value else {
        out.assessment = Assessment::Inconclusive;
        out.reason = Some(if a_s.is_empty() || b_s.is_empty() {
            "no_samples"
        } else {
            "too_few_runs"
        });
        return Some(out);
    };
    let significant = p < settings.significance_level;
    let floor = settings.noise_threshold.unwrap_or(MIN_EFFECT);
    let worse = match direction {
        Direction::LowerIsBetter => change > 0.0,
        Direction::HigherIsBetter => change < 0.0,
    };
    // An interval that lies inside the noise floor shows "no change worth reporting" even
    // when the test is not significant (identical commands)
    let inside_floor = out
        .ci_low
        .zip(out.ci_high)
        .is_some_and(|(lo, hi)| lo > -floor && hi < floor);
    out.assessment = if out
        .equivalence_p_value
        .is_some_and(|e| e < settings.significance_level)
    {
        Assessment::Equivalent
    } else if change.abs() < floor
        && (settings.noise_threshold.is_some() || significant || inside_floor)
    {
        Assessment::Unchanged
    } else if significant {
        if worse {
            Assessment::Regressed
        } else {
            Assessment::Improved
        }
    } else {
        out.reason = Some("not_significant");
        Assessment::Inconclusive
    };
    Some(out)
}

/// Build the document. `budgets` is the report of `--budget`, if any.
pub fn build(
    comparison: &Comparison<'_>,
    budgets: Option<&Report>,
    baseline_path: &str,
    settings: &StatSettings,
) -> Document {
    let mut commands = Vec::with_capacity(comparison.deltas.len());
    for delta in &comparison.deltas {
        let metrics: Vec<MetricVerdict> = metrics_of(delta)
            .into_iter()
            .filter_map(|m| assess_metric(delta, m, settings))
            .collect();
        commands.push(CommandVerdict {
            command: delta.name.to_string(),
            verdict: Overall::combine(metrics.iter().map(|m| m.assessment)),
            metrics,
            budgets: budgets
                .map(|r| {
                    r.budgets
                        .iter()
                        .filter(|e| e.command == delta.name)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default(),
        });
    }
    let overall = Overall::combine(
        commands
            .iter()
            .flat_map(|c| c.metrics.iter().map(|m| m.assessment)),
    );
    let mut summary = Summary::default();
    for c in &commands {
        match c.verdict {
            Overall::Improved => summary.improved += 1,
            Overall::Regressed => summary.regressed += 1,
            Overall::TradeOff => summary.trade_off += 1,
            Overall::Unchanged => summary.unchanged += 1,
            Overall::Inconclusive => summary.inconclusive += 1,
            Overall::NotEvaluated => summary.not_evaluated += 1,
        }
    }
    let gate = budgets.map(|r| Gate {
        verdict: r.verdict,
        failures: r.failures(),
        on_unavailable: r.on_unavailable,
        other_budgets: r
            .budgets
            .iter()
            .filter(|e| !comparison.deltas.iter().any(|d| d.name == e.command))
            .cloned()
            .collect(),
    });
    Document {
        verdict_version: VERDICT_VERSION,
        tool: Tool {
            name: "perfratio",
            version: env!("CARGO_PKG_VERSION"),
        },
        baseline: baseline_path.to_string(),
        settings: Settings {
            estimator: settings.estimator.name(),
            confidence_level: settings.confidence_level,
            significance_level: settings.significance_level,
            noise_threshold: settings.noise_threshold.unwrap_or(MIN_EFFECT),
            equivalence_margin: settings.equivalence_margin,
        },
        verdict: overall,
        summary,
        gate,
        commands,
        new: comparison.new.iter().map(|s| s.to_string()).collect(),
        removed: comparison.removed.iter().map(|s| s.to_string()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Assessment::*;

    #[test]
    fn combining_assessments() {
        assert_eq!(Overall::combine([Improved, Unchanged]), Overall::Improved);
        assert_eq!(Overall::combine([Regressed, Unchanged]), Overall::Regressed);
        assert_eq!(Overall::combine([Improved, Regressed]), Overall::TradeOff);
        assert_eq!(
            Overall::combine([Improved, Inconclusive]),
            Overall::Improved
        );
        assert_eq!(
            Overall::combine([Unchanged, Inconclusive]),
            Overall::Inconclusive
        );
        assert_eq!(
            Overall::combine([Unchanged, Equivalent]),
            Overall::Unchanged
        );
        assert_eq!(Overall::combine([NotEvaluated]), Overall::NotEvaluated);
        assert_eq!(Overall::combine([]), Overall::NotEvaluated);
    }
}
