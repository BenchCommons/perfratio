//! Measurement quality report (`--quality`, `--fail-on-quality`): one
//! machine-readable answer to "should I trust this measurement?" per
//! benchmark. See `docs/specs/measurement-quality.md`.
//!
//! The report only collects checks that perfratio already computes (outlier
//! statistics, interference diagnostics, `--target-precision`, counter
//! coverage, energy samples, workload validation). It does not recompute them
//! and it never states hypotheses about causes, only objective facts.

use serde::{Deserialize, Serialize};

use crate::benchmark::benchmark_result::BenchmarkResult;
use crate::metric::Metric;
use crate::outlier_detection::MIN_OUTLIER_SAMPLE;
use crate::stats::diagnostics::MIN_TREND_SAMPLES;

/// Below this many runs the spread of the run times cannot be estimated
pub const MIN_RUNS_FAIL: usize = 3;
/// Below this many runs a confidence interval of the mean is unreliable
pub const MIN_RUNS_WARN: usize = 10;
/// The exported 5th and 95th percentiles need at least this many runs
pub const MIN_RUNS_PERCENTILES: usize = 20;
/// Without `--target-precision`: relative 95% CI half-width above which the
/// mean is flagged, and above which it is considered too imprecise to use
pub const PRECISION_WARN: f64 = 0.05;
pub const PRECISION_FAIL: f64 = 0.20;
/// Counter coverage (`time_running / time_enabled`) below which counts are
/// flagged, and below which they are considered unreliable
pub const COVERAGE_WARN: f64 = 0.9;
pub const COVERAGE_FAIL: f64 = 0.5;

/// Outcome of one check
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
    /// The check does not apply (the feature was not requested)
    NotApplicable,
    /// The check applies but its data source gave nothing: never a `pass`
    Unavailable,
}

impl CheckStatus {
    pub fn name(self) -> &'static str {
        match self {
            CheckStatus::Pass => "pass",
            CheckStatus::Warn => "warn",
            CheckStatus::Fail => "fail",
            CheckStatus::NotApplicable => "not_applicable",
            CheckStatus::Unavailable => "unavailable",
        }
    }
}

/// Overall verdict, derived by a fixed rule from the checks: any `fail` is
/// `unreliable`, else any `warn` is `with_caveats`, else `comparable`.
/// Ordered from best to worst.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Grade {
    Comparable,
    WithCaveats,
    Unreliable,
}

impl Grade {
    pub fn name(self) -> &'static str {
        match self {
            Grade::Comparable => "comparable",
            Grade::WithCaveats => "with_caveats",
            Grade::Unreliable => "unreliable",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "comparable" => Some(Grade::Comparable),
            "with_caveats" => Some(Grade::WithCaveats),
            "unreliable" => Some(Grade::Unreliable),
            _ => None,
        }
    }
}

/// One check. `id` is an open enum: consumers must ignore ids they do not know.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    pub id: String,
    /// The metric the check is about, when it is per metric (`time`,
    /// `memory`, `energy`, `counters`, or the primary metric name)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metric: Option<String>,
    pub status: CheckStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The `quality` object of a result
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityReport {
    pub grade: Grade,
    pub checks: Vec<Check>,
}

/// What `--quality` prints
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QualityMode {
    /// Nothing
    Off,
    /// Without the option: the grade and the checks that are not `pass`, but
    /// only when some check is `warn`, `fail` or `unavailable`
    #[default]
    Auto,
    /// Always the grade and the checks that are not `pass`
    Summary,
    /// Every check
    Full,
}

impl QualityMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "off" => Some(QualityMode::Off),
            "summary" => Some(QualityMode::Summary),
            "full" => Some(QualityMode::Full),
            _ => None,
        }
    }
}

/// Session settings the checks need besides the result itself
#[derive(Debug, Clone, Copy)]
pub struct QualityContext {
    /// The primary metric (`--metric`)
    pub metric: Metric,
    /// `--counters` (or a counter metric) was requested
    pub counters_requested: bool,
    /// `--energy` was requested
    pub energy_requested: bool,
}

impl QualityReport {
    /// The grade of a list of checks (see [`Grade`])
    pub fn grade_of(checks: &[Check]) -> Grade {
        if checks.iter().any(|c| c.status == CheckStatus::Fail) {
            Grade::Unreliable
        } else if checks.iter().any(|c| c.status == CheckStatus::Warn) {
            Grade::WithCaveats
        } else {
            Grade::Comparable
        }
    }

    pub fn from_checks(checks: Vec<Check>) -> Self {
        QualityReport {
            grade: Self::grade_of(&checks),
            checks,
        }
    }

    /// Run every check on `result`
    pub fn assess(result: &BenchmarkResult, context: &QualityContext) -> Self {
        let mut checks = vec![
            sample_sufficiency(result, context),
            statistical_precision(result, context),
            outlier_burden(result, "time"),
        ];
        checks.extend(secondary_outliers(result));
        checks.push(trend(result));
        checks.push(interference(result));
        checks.push(environment_stability(result));
        checks.push(counter_coverage(result, context));
        checks.push(energy_coverage(result, context));
        checks.push(workload_validation(result));
        Self::from_checks(checks)
    }

    /// Whether the terminal shows this report in `mode`
    fn is_shown(&self, mode: QualityMode) -> bool {
        match mode {
            QualityMode::Off => false,
            QualityMode::Summary | QualityMode::Full => true,
            QualityMode::Auto => self.checks.iter().any(|c| {
                matches!(
                    c.status,
                    CheckStatus::Warn | CheckStatus::Fail | CheckStatus::Unavailable
                )
            }),
        }
    }

    /// The terminal block, `None` when `mode` shows nothing for this report
    pub fn render(&self, mode: QualityMode) -> Option<String> {
        if !self.is_shown(mode) {
            return None;
        }
        let mut out = format!("  Measurement quality: {}\n", self.grade.name());
        for check in &self.checks {
            let shown = match mode {
                QualityMode::Full => true,
                _ => !matches!(check.status, CheckStatus::Pass | CheckStatus::NotApplicable),
            };
            if !shown {
                continue;
            }
            let subject = match &check.metric {
                Some(metric) => format!("{} ({metric})", check.id),
                None => check.id.clone(),
            };
            out.push_str(&format!("    {:<14} {subject}", check.status.name()));
            if let Some(detail) = &check.detail {
                out.push_str(&format!(": {detail}"));
            }
            out.push('\n');
        }
        Some(out)
    }
}

fn check(
    id: &str,
    metric: Option<&str>,
    status: CheckStatus,
    detail: impl Into<Option<String>>,
) -> Check {
    Check {
        id: id.to_string(),
        metric: metric.map(str::to_string),
        status,
        detail: detail.into(),
    }
}

fn runs_of(result: &BenchmarkResult) -> usize {
    result.times.as_ref().map_or(0, Vec::len)
}

/// Runs vs. the minimum for a CI of the mean and for the exported percentiles
fn sample_sufficiency(result: &BenchmarkResult, context: &QualityContext) -> Check {
    let metric = context.metric.name();
    let n = runs_of(result);
    let id = "sample_sufficiency";
    if n == 0 {
        return check(
            id,
            Some(metric),
            CheckStatus::Unavailable,
            "no completed run".to_string(),
        );
    }
    if n < MIN_RUNS_FAIL {
        return check(
            id,
            Some(metric),
            CheckStatus::Fail,
            format!("{n} runs; at least {MIN_RUNS_FAIL} are needed to estimate the spread"),
        );
    }
    if n < MIN_RUNS_WARN {
        return check(
            id,
            Some(metric),
            CheckStatus::Warn,
            format!("{n} runs; {MIN_RUNS_WARN} or more give a usable confidence interval"),
        );
    }
    if result.percentiles.is_some() && n < MIN_RUNS_PERCENTILES {
        return check(
            id,
            Some(metric),
            CheckStatus::Warn,
            format!(
                "{n} runs; the 5th and 95th percentiles need at least {MIN_RUNS_PERCENTILES} \
                 (with fewer they are close to the minimum and maximum)"
            ),
        );
    }
    check(id, Some(metric), CheckStatus::Pass, format!("{n} runs"))
}

/// 95% CI half-width of the mean vs. `--target-precision`, or vs. the fixed
/// thresholds without a target
fn statistical_precision(result: &BenchmarkResult, context: &QualityContext) -> Check {
    let id = "statistical_precision";
    let metric = context.metric.name();
    let percent = |x: f64| format!("{:.1}%", x * 100.0);
    if let Some(precision) = &result.precision {
        let Some(reached) = precision.reached else {
            return check(
                id,
                Some(metric),
                CheckStatus::Unavailable,
                "the precision could not be computed".to_string(),
            );
        };
        return if precision.met {
            check(
                id,
                Some(metric),
                CheckStatus::Pass,
                format!(
                    "±{} reached, target ±{}",
                    percent(reached),
                    percent(precision.target)
                ),
            )
        } else {
            check(
                id,
                Some(metric),
                CheckStatus::Warn,
                format!(
                    "±{} reached, target ±{} ('--target-precision') was not met",
                    percent(reached),
                    percent(precision.target)
                ),
            )
        };
    }
    let samples = result.primary_samples(context.metric).unwrap_or_default();
    let Some(half_width) = crate::stats::precision::relative_ci_half_width(&samples) else {
        return check(
            id,
            Some(metric),
            CheckStatus::Unavailable,
            "fewer than 2 runs or a mean of 0".to_string(),
        );
    };
    let detail = format!("±{} (95% CI of the mean)", percent(half_width));
    let status = if half_width > PRECISION_FAIL {
        CheckStatus::Fail
    } else if half_width > PRECISION_WARN {
        CheckStatus::Warn
    } else {
        CheckStatus::Pass
    };
    check(id, Some(metric), status, detail)
}

/// Outliers of the run times: how many, and whether they inflate the variance
fn outlier_burden(result: &BenchmarkResult, metric: &str) -> Check {
    let id = "outlier_burden";
    let Some(report) = result.outliers.as_ref() else {
        return check(
            id,
            Some(metric),
            CheckStatus::Unavailable,
            format!("fewer than {MIN_OUTLIER_SAMPLE} runs"),
        );
    };
    let stats = report.time;
    if result
        .diagnostics
        .is_some_and(|d| d.has_inflated_variance())
    {
        return check(
            id,
            Some(metric),
            CheckStatus::Warn,
            format!(
                "{} of the runs are outliers ({:.1}%) and cause most of the variance",
                stats.count, stats.percent
            ),
        );
    }
    let detail = format!(
        "{} outliers ({:.1}% of the runs)",
        stats.count, stats.percent
    );
    let status = if stats.is_severe() {
        CheckStatus::Warn
    } else {
        CheckStatus::Pass
    };
    check(id, Some(metric), status, detail)
}

/// Outlier checks of memory and energy, only when their statistics exist
fn secondary_outliers(result: &BenchmarkResult) -> Vec<Check> {
    let Some(report) = result.outliers.as_ref() else {
        return vec![];
    };
    let mut checks = vec![];
    for (metric, stats) in [
        ("memory", report.memory_usage_byte),
        ("energy", report.energy_joules),
    ] {
        let Some(stats) = stats else { continue };
        let status = if stats.is_severe() {
            CheckStatus::Warn
        } else {
            CheckStatus::Pass
        };
        checks.push(check(
            "outlier_burden",
            Some(metric),
            status,
            format!(
                "{} outliers ({:.1}% of the runs)",
                stats.count, stats.percent
            ),
        ));
    }
    checks
}

/// Systematic drift of the run times (thermal throttling, filling caches)
fn trend(result: &BenchmarkResult) -> Check {
    let id = "trend";
    if runs_of(result) < MIN_TREND_SAMPLES {
        return check(
            id,
            Some("time"),
            CheckStatus::Unavailable,
            format!("needs at least {MIN_TREND_SAMPLES} runs"),
        );
    }
    match result.diagnostics.filter(|d| d.has_trend()) {
        Some(d) => check(
            id,
            Some("time"),
            CheckStatus::Warn,
            format!(
                "the run times drift by {:+.1}% from the first to the last run (p {:.3})",
                d.trend_rel.unwrap_or_default() * 100.0,
                d.trend_p.unwrap_or_default()
            ),
        ),
        None => check(id, Some("time"), CheckStatus::Pass, None),
    }
}

/// Two or more separated clusters of run times (a background process, caches)
fn interference(result: &BenchmarkResult) -> Check {
    let id = "interference";
    if runs_of(result) < MIN_TREND_SAMPLES {
        return check(
            id,
            Some("time"),
            CheckStatus::Unavailable,
            format!("needs at least {MIN_TREND_SAMPLES} runs"),
        );
    }
    match result.diagnostics.filter(|d| d.is_multimodal()) {
        Some(d) => check(
            id,
            Some("time"),
            CheckStatus::Warn,
            format!(
                "the run times form separate clusters (bimodality coefficient {:.2})",
                d.bimodality.unwrap_or_default()
            ),
        ),
        None => check(id, Some("time"), CheckStatus::Pass, None),
    }
}

/// A cgroup quota that throttled the runs, thermal throttling or a rising
/// temperature, a falling effective CPU frequency (#315). Facts of the
/// environment the benchmark ran in, not hypotheses.
fn environment_stability(result: &BenchmarkResult) -> Check {
    let id = "environment_stability";
    let metric = Some("environment");
    let runs = runs_of(result);
    let mut warnings: Vec<String> = Vec::new();
    let mut facts: Vec<String> = Vec::new();

    if let Some(i) = &result.interference {
        if let (Some(usec), Some(throttled_runs)) =
            (i.cgroup_throttled_usec, i.cgroup_throttled_runs)
        {
            if throttled_runs > 0 || usec > 0 {
                warnings.push(format!(
                    "the cgroup CPU quota throttled the benchmark for {:.1} ms in {throttled_runs} of {runs} runs",
                    usec as f64 / 1000.0
                ));
            } else {
                facts.push("cgroup not throttled".to_string());
            }
        }
        if let Some(kills) = i.cgroup_oom_kills.filter(|&k| k > 0) {
            warnings.push(format!(
                "the out-of-memory killer killed {kills} process(es) in the cgroup"
            ));
        }
        if let Some(t) = &i.thermal {
            if let Some(events) = t.throttle_events.filter(|&e| e > 0) {
                warnings.push(format!(
                    "the CPU reported {events} thermal throttling event(s)"
                ));
            }
            if t.end_c - t.start_c >= THERMAL_RISE_WARN_C {
                warnings.push(format!(
                    "the CPU temperature rose from {:.0} to {:.0} °C (peak {:.0} °C)",
                    t.start_c, t.end_c, t.max_c
                ));
            } else {
                facts.push(format!("temperature {:.0} to {:.0} °C", t.start_c, t.end_c));
            }
        }
    }

    let frequencies: Vec<f64> = result
        .effective_frequency_hz
        .iter()
        .flatten()
        .flatten()
        .copied()
        .collect();
    if let Some(trend) = crate::stats::diagnostics::trend(&frequencies) {
        if trend.rel_change <= -crate::stats::diagnostics::TREND_MIN_REL_CHANGE
            && trend.p_value < crate::stats::diagnostics::TREND_MAX_P
        {
            warnings.push(format!(
                "the effective CPU frequency fell {:.0}% from the first to the last run",
                -trend.rel_change * 100.0
            ));
        }
    }

    if !warnings.is_empty() {
        return check(id, metric, CheckStatus::Warn, warnings.join("; "));
    }
    if facts.is_empty() {
        return check(
            id,
            metric,
            CheckStatus::NotApplicable,
            "no cgroup throttle counters or temperature sensors can be read on this host"
                .to_string(),
        );
    }
    check(id, metric, CheckStatus::Pass, facts.join(", "))
}

/// A temperature rise during the benchmark that is flagged, °C
pub const THERMAL_RISE_WARN_C: f64 = 10.0;

/// Hardware counters: did every run read them, and were they scheduled the
/// whole time (multiplexing scales the counts when they were not)
fn counter_coverage(result: &BenchmarkResult, context: &QualityContext) -> Check {
    let id = "counter_coverage";
    if !context.counters_requested {
        return check(id, Some("counters"), CheckStatus::NotApplicable, None);
    }
    let Some(counters) = result.counters.as_ref().filter(|c| !c.is_empty()) else {
        return check(
            id,
            Some("counters"),
            CheckStatus::Unavailable,
            "no counter could be read (see the 'counters' status in the export)".to_string(),
        );
    };
    let unread: Vec<&String> = counters
        .iter()
        .filter(|(_, values)| values.iter().all(Option::is_none))
        .map(|(name, _)| name)
        .collect();
    if unread.len() == counters.len() {
        return check(
            id,
            Some("counters"),
            CheckStatus::Unavailable,
            "no run produced a counter reading".to_string(),
        );
    }
    let partial = counters
        .iter()
        .filter(|(_, values)| {
            values.iter().any(Option::is_none) && values.iter().any(Option::is_some)
        })
        .count();
    let lowest = result
        .counters_coverage
        .iter()
        .flatten()
        .map(|(name, coverage)| (name, *coverage))
        .min_by(|a, b| a.1.total_cmp(&b.1));
    if let Some((name, coverage)) = lowest.filter(|(_, c)| *c < COVERAGE_WARN) {
        let status = if coverage < COVERAGE_FAIL {
            CheckStatus::Fail
        } else {
            CheckStatus::Warn
        };
        return check(
            id,
            Some("counters"),
            status,
            format!(
                "'{name}' was scheduled {:.0}% of the time (PMU multiplexing); its counts are \
                 scaled estimates",
                coverage * 100.0
            ),
        );
    }
    if !unread.is_empty() || partial > 0 {
        return check(
            id,
            Some("counters"),
            CheckStatus::Warn,
            format!(
                "{} counters were never read and {partial} were missing in some runs",
                unread.len()
            ),
        );
    }
    check(id, Some("counters"), CheckStatus::Pass, None)
}

/// Energy: how many runs have a sample. The value is the energy of the whole
/// measurement domain (the package), not of this process alone.
fn energy_coverage(result: &BenchmarkResult, context: &QualityContext) -> Check {
    let id = "energy_coverage";
    if !context.energy_requested {
        return check(id, Some("energy"), CheckStatus::NotApplicable, None);
    }
    let runs = runs_of(result);
    let samples = result.energy_samples.unwrap_or(0);
    if samples == 0 {
        return check(
            id,
            Some("energy"),
            CheckStatus::Unavailable,
            "no run produced an energy sample (see the 'energy' status in the export)".to_string(),
        );
    }
    if samples < runs {
        let status = if samples * 2 < runs {
            CheckStatus::Fail
        } else {
            CheckStatus::Warn
        };
        return check(
            id,
            Some("energy"),
            status,
            format!("{samples} of {runs} runs have an energy sample"),
        );
    }
    check(
        id,
        Some("energy"),
        CheckStatus::Pass,
        "every run has a sample; the value covers the whole measurement domain, not only this process"
            .to_string(),
    )
}

/// `--expect-output-sha256` / `--validate` (#316)
fn workload_validation(result: &BenchmarkResult) -> Check {
    let id = "workload_validation";
    let Some(v) = result.validation.as_ref() else {
        return check(id, None, CheckStatus::NotApplicable, None);
    };
    if v.runs_invalid > 0 {
        return check(
            id,
            None,
            CheckStatus::Fail,
            format!(
                "{} of {} validated runs produced a wrong result ({} omitted)",
                v.runs_invalid, v.runs_checked, v.runs_omitted
            ),
        );
    }
    if v.runs_checked == 0 {
        return check(
            id,
            None,
            CheckStatus::Unavailable,
            "no run was validated".to_string(),
        );
    }
    let detail = if v.first_only {
        "the first run was validated".to_string()
    } else {
        format!("all {} validated runs are correct", v.runs_checked)
    };
    check(id, None, CheckStatus::Pass, detail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::benchmark::benchmark_result::PrecisionReached;
    use crate::outlier_detection::{OutlierMethod, OutlierReport, OutlierStats};
    use crate::stats::diagnostics::Diagnostics;
    use crate::validation::{OnInvalid, ValidationSummary};

    fn context() -> QualityContext {
        QualityContext {
            metric: Metric::Wall,
            counters_requested: false,
            energy_requested: false,
        }
    }

    fn with_times(times: Vec<f64>) -> BenchmarkResult {
        let mean = times.iter().sum::<f64>() / times.len().max(1) as f64;
        BenchmarkResult {
            mean,
            times: Some(times),
            ..Default::default()
        }
    }

    fn status_of(report: &QualityReport, id: &str, metric: Option<&str>) -> CheckStatus {
        report
            .checks
            .iter()
            .find(|c| c.id == id && c.metric.as_deref() == metric)
            .unwrap_or_else(|| panic!("no check {id}"))
            .status
    }

    fn steady(n: usize) -> Vec<f64> {
        (0..n).map(|i| 1.0 + 0.001 * (i % 3) as f64).collect()
    }

    #[test]
    fn grade_derivation_is_table_driven() {
        use CheckStatus::*;
        let cases: [(&[CheckStatus], Grade); 7] = [
            (&[], Grade::Comparable),
            (&[Pass, NotApplicable, Unavailable], Grade::Comparable),
            (&[Pass, Warn], Grade::WithCaveats),
            (&[Warn, Warn], Grade::WithCaveats),
            (&[Pass, Fail], Grade::Unreliable),
            (&[Warn, Fail, Pass], Grade::Unreliable),
            (&[Unavailable, Fail], Grade::Unreliable),
        ];
        for (statuses, expected) in cases {
            let checks: Vec<Check> = statuses
                .iter()
                .map(|&status| check("x", None, status, None))
                .collect();
            assert_eq!(QualityReport::grade_of(&checks), expected, "{statuses:?}");
        }
        assert!(Grade::Comparable < Grade::WithCaveats && Grade::WithCaveats < Grade::Unreliable);
    }

    #[test]
    fn sample_sufficiency_pass_and_fail() {
        let ctx = context();
        let good = QualityReport::assess(&with_times(steady(30)), &ctx);
        assert_eq!(
            status_of(&good, "sample_sufficiency", Some("wall")),
            CheckStatus::Pass
        );
        let few = QualityReport::assess(&with_times(steady(2)), &ctx);
        assert_eq!(
            status_of(&few, "sample_sufficiency", Some("wall")),
            CheckStatus::Fail
        );
        assert_eq!(few.grade, Grade::Unreliable);
        let some = QualityReport::assess(&with_times(steady(5)), &ctx);
        assert_eq!(
            status_of(&some, "sample_sufficiency", Some("wall")),
            CheckStatus::Warn
        );
        let none = QualityReport::assess(&BenchmarkResult::default(), &ctx);
        assert_eq!(
            status_of(&none, "sample_sufficiency", Some("wall")),
            CheckStatus::Unavailable
        );
    }

    #[test]
    fn exported_percentiles_need_enough_runs() {
        let mut result = with_times(steady(12));
        result.percentiles = Some(crate::benchmark::benchmark_result::Percentiles {
            p05: 1.0,
            p25: 1.0,
            p75: 1.0,
            p95: 1.0,
        });
        let report = QualityReport::assess(&result, &context());
        assert_eq!(
            status_of(&report, "sample_sufficiency", Some("wall")),
            CheckStatus::Warn
        );
        let mut result = with_times(steady(25));
        result.percentiles = Some(Default::default());
        let report = QualityReport::assess(&result, &context());
        assert_eq!(
            status_of(&report, "sample_sufficiency", Some("wall")),
            CheckStatus::Pass
        );
    }

    #[test]
    fn precision_against_target_and_default_thresholds() {
        let ctx = context();
        let mut result = with_times(steady(30));
        result.precision = Some(PrecisionReached {
            target: 0.01,
            reached: Some(0.004),
            confidence: 0.95,
            met: true,
        });
        let report = QualityReport::assess(&result, &ctx);
        assert_eq!(
            status_of(&report, "statistical_precision", Some("wall")),
            CheckStatus::Pass
        );
        result.precision = Some(PrecisionReached {
            target: 0.01,
            reached: Some(0.03),
            confidence: 0.95,
            met: false,
        });
        let report = QualityReport::assess(&result, &ctx);
        assert_eq!(
            status_of(&report, "statistical_precision", Some("wall")),
            CheckStatus::Warn
        );

        // no target: a tight series passes, a noisy one fails
        let tight = QualityReport::assess(&with_times(steady(30)), &ctx);
        assert_eq!(
            status_of(&tight, "statistical_precision", Some("wall")),
            CheckStatus::Pass
        );
        let noisy: Vec<f64> = (0..10)
            .map(|i| if i % 2 == 0 { 1.0 } else { 3.0 })
            .collect();
        let noisy = QualityReport::assess(&with_times(noisy), &ctx);
        assert_eq!(
            status_of(&noisy, "statistical_precision", Some("wall")),
            CheckStatus::Fail
        );
    }

    fn outlier_stats(count: usize, percent: f64) -> OutlierStats {
        OutlierStats {
            count,
            percent,
            lower_fence: 0.0,
            upper_fence: 1.0,
        }
    }

    #[test]
    fn outlier_burden_pass_warn_and_unavailable() {
        let ctx = context();
        let mut result = with_times(steady(30));
        let report = QualityReport::assess(&result, &ctx);
        assert_eq!(
            status_of(&report, "outlier_burden", Some("time")),
            CheckStatus::Unavailable
        );
        result.outliers = Some(OutlierReport {
            method: OutlierMethod::default(),
            time: outlier_stats(1, 3.3),
            memory_usage_byte: Some(outlier_stats(0, 0.0)),
            energy_joules: Some(outlier_stats(6, 20.0)),
        });
        let report = QualityReport::assess(&result, &ctx);
        assert_eq!(
            status_of(&report, "outlier_burden", Some("time")),
            CheckStatus::Pass
        );
        assert_eq!(
            status_of(&report, "outlier_burden", Some("memory")),
            CheckStatus::Pass
        );
        assert_eq!(
            status_of(&report, "outlier_burden", Some("energy")),
            CheckStatus::Warn
        );
        result.outliers.as_mut().unwrap().time = outlier_stats(4, 13.3);
        let report = QualityReport::assess(&result, &ctx);
        assert_eq!(
            status_of(&report, "outlier_burden", Some("time")),
            CheckStatus::Warn
        );
    }

    #[test]
    fn trend_and_interference_use_the_existing_diagnostics() {
        let ctx = context();
        let calm = QualityReport::assess(&with_times(steady(30)), &ctx);
        assert_eq!(status_of(&calm, "trend", Some("time")), CheckStatus::Pass);
        assert_eq!(
            status_of(&calm, "interference", Some("time")),
            CheckStatus::Pass
        );

        // a rising series: the diagnostics module flags the trend
        let rising: Vec<f64> = (0..30).map(|i| 1.0 + 0.02 * i as f64).collect();
        let mut result = with_times(rising.clone());
        result.diagnostics = Some(Diagnostics::compute(&rising, 3.5));
        let report = QualityReport::assess(&result, &ctx);
        assert_eq!(status_of(&report, "trend", Some("time")), CheckStatus::Warn);

        // two clusters: flagged as interference
        let clusters: Vec<f64> = (0..30)
            .map(|i| {
                if i % 2 == 0 {
                    1.0 + 0.001 * i as f64
                } else {
                    2.0 + 0.001 * i as f64
                }
            })
            .collect();
        let mut result = with_times(clusters.clone());
        result.diagnostics = Some(Diagnostics::compute(&clusters, 3.5));
        let report = QualityReport::assess(&result, &ctx);
        assert_eq!(
            status_of(&report, "interference", Some("time")),
            CheckStatus::Warn
        );

        // too few runs for the diagnostics
        let short = QualityReport::assess(&with_times(steady(5)), &ctx);
        assert_eq!(
            status_of(&short, "trend", Some("time")),
            CheckStatus::Unavailable
        );
    }

    #[test]
    fn counter_coverage_checks() {
        let mut ctx = context();
        let result = with_times(steady(10));
        let report = QualityReport::assess(&result, &ctx);
        assert_eq!(
            status_of(&report, "counter_coverage", Some("counters")),
            CheckStatus::NotApplicable
        );

        ctx.counters_requested = true;
        // requested but nothing exported: unavailable, never pass
        let report = QualityReport::assess(&result, &ctx);
        assert_eq!(
            status_of(&report, "counter_coverage", Some("counters")),
            CheckStatus::Unavailable
        );

        let mut result = with_times(steady(10));
        let mut counters = std::collections::BTreeMap::new();
        counters.insert("instructions".to_string(), vec![Some(1u64); 10]);
        result.counters = Some(counters);
        let report = QualityReport::assess(&result, &ctx);
        assert_eq!(
            status_of(&report, "counter_coverage", Some("counters")),
            CheckStatus::Pass
        );

        let mut coverage = std::collections::BTreeMap::new();
        coverage.insert("instructions".to_string(), 0.7);
        result.counters_coverage = Some(coverage.clone());
        let report = QualityReport::assess(&result, &ctx);
        assert_eq!(
            status_of(&report, "counter_coverage", Some("counters")),
            CheckStatus::Warn
        );
        coverage.insert("instructions".to_string(), 0.3);
        result.counters_coverage = Some(coverage);
        let report = QualityReport::assess(&result, &ctx);
        assert_eq!(
            status_of(&report, "counter_coverage", Some("counters")),
            CheckStatus::Fail
        );

        // every reading missing
        let mut result = with_times(steady(10));
        let mut counters = std::collections::BTreeMap::new();
        counters.insert("cycles".to_string(), vec![None::<u64>; 10]);
        result.counters = Some(counters);
        let report = QualityReport::assess(&result, &ctx);
        assert_eq!(
            status_of(&report, "counter_coverage", Some("counters")),
            CheckStatus::Unavailable
        );
    }

    #[test]
    fn energy_coverage_checks() {
        let mut ctx = context();
        let mut result = with_times(steady(10));
        let report = QualityReport::assess(&result, &ctx);
        assert_eq!(
            status_of(&report, "energy_coverage", Some("energy")),
            CheckStatus::NotApplicable
        );

        ctx.energy_requested = true;
        let report = QualityReport::assess(&result, &ctx);
        assert_eq!(
            status_of(&report, "energy_coverage", Some("energy")),
            CheckStatus::Unavailable
        );
        result.energy_samples = Some(0);
        let report = QualityReport::assess(&result, &ctx);
        assert_eq!(
            status_of(&report, "energy_coverage", Some("energy")),
            CheckStatus::Unavailable
        );
        result.energy_samples = Some(7);
        let report = QualityReport::assess(&result, &ctx);
        assert_eq!(
            status_of(&report, "energy_coverage", Some("energy")),
            CheckStatus::Warn
        );
        result.energy_samples = Some(3);
        let report = QualityReport::assess(&result, &ctx);
        assert_eq!(
            status_of(&report, "energy_coverage", Some("energy")),
            CheckStatus::Fail
        );
        result.energy_samples = Some(10);
        let report = QualityReport::assess(&result, &ctx);
        assert_eq!(
            status_of(&report, "energy_coverage", Some("energy")),
            CheckStatus::Pass
        );
    }

    #[test]
    fn workload_validation_checks() {
        let ctx = context();
        let mut result = with_times(steady(10));
        let report = QualityReport::assess(&result, &ctx);
        assert_eq!(
            status_of(&report, "workload_validation", None),
            CheckStatus::NotApplicable
        );

        let summary = |checked, invalid, omitted| ValidationSummary {
            mode: "output_sha256".into(),
            on_invalid: OnInvalid::Keep,
            first_only: false,
            runs_checked: checked,
            runs_invalid: invalid,
            runs_omitted: omitted,
            omitted_runs: vec![],
            output_sha256: None,
        };
        result.validation = Some(summary(10, 0, 0));
        let report = QualityReport::assess(&result, &ctx);
        assert_eq!(
            status_of(&report, "workload_validation", None),
            CheckStatus::Pass
        );
        result.validation = Some(summary(10, 2, 2));
        let report = QualityReport::assess(&result, &ctx);
        assert_eq!(
            status_of(&report, "workload_validation", None),
            CheckStatus::Fail
        );
        assert_eq!(report.grade, Grade::Unreliable);
        result.validation = Some(summary(0, 0, 0));
        let report = QualityReport::assess(&result, &ctx);
        assert_eq!(
            status_of(&report, "workload_validation", None),
            CheckStatus::Unavailable
        );
    }

    #[test]
    fn terminal_rendering_by_mode() {
        let ctx = context();
        let clean = QualityReport::assess(&with_times(steady(30)), &ctx);
        // clean results print nothing by default; unavailable checks print
        let auto = clean.render(QualityMode::Auto);
        assert!(
            auto.as_deref().is_none_or(|s| s.contains("unavailable")),
            "{auto:?}"
        );
        let few = QualityReport::assess(&with_times(steady(2)), &ctx);
        let text = few.render(QualityMode::Auto).unwrap();
        assert!(text.contains("Measurement quality: unreliable"), "{text}");
        assert!(text.contains("fail"), "{text}");
        assert!(few.render(QualityMode::Off).is_none());
        let full = clean.render(QualityMode::Full).unwrap();
        assert!(full.contains("not_applicable"), "{full}");
        let summary = clean.render(QualityMode::Summary).unwrap();
        assert!(!summary.contains("not_applicable"), "{summary}");
    }

    #[test]
    fn json_shape() {
        let report = QualityReport::assess(&with_times(steady(30)), &context());
        let json = serde_json::to_value(&report).unwrap();
        assert!(
            ["comparable", "with_caveats", "unreliable"].contains(&json["grade"].as_str().unwrap())
        );
        let first = &json["checks"][0];
        assert_eq!(first["id"], "sample_sufficiency");
        assert_eq!(first["status"], "pass");
        let back: QualityReport = serde_json::from_value(json).unwrap();
        assert_eq!(back, report);
    }
}
