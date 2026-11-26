use serde::*;
use serde_json::to_vec_pretty;

use super::metadata::ExportMetadata;
use super::Exporter;
use crate::benchmark::benchmark_result::BenchmarkResult;
use crate::benchmark::relative_speed;
use crate::metric::Metric;
use crate::options::SortOrder;
use crate::pareto::{Analysis, ParetoConfig};
use crate::stats::significance::{self, Significance};
use crate::util::units::Unit;

use anyhow::Result;

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct PrimaryMetricStats {
    pub metric: Metric,
    pub mean: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stddev: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub samples: Option<Vec<f64>>,
}

/// Version of the JSON output contract (`docs/specs/output-schema-contract.md`).
pub const SCHEMA_VERSION: &str = "1.22";

#[derive(Serialize, Debug)]
struct PerfratioSummary<'a> {
    /// Version of the output contract; must stay the first key
    #[serde(rename = "schemaVersion")]
    schema_version: &'static str,
    /// Metric used for ranking and relative speed (`wall` unless `--metric` is set)
    metric: Metric,
    /// Information about the perfratio run (version, command line, date, labels, system)
    #[serde(skip_serializing_if = "Option::is_none")]
    perfratio: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    interrupted: bool,
    /// Result of `--optimize` (object, or array with one entry per search)
    #[serde(skip_serializing_if = "Option::is_none")]
    optimize: Option<serde_json::Value>,
    /// Pareto analysis of `--pareto` (new in 1.16, #326)
    #[serde(skip_serializing_if = "Option::is_none")]
    pareto: Option<Analysis>,
    results: Vec<ResultEntry<'a>>,
}

#[derive(Serialize, Debug)]
struct ResultEntry<'a> {
    #[serde(flatten)]
    result: &'a BenchmarkResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    primary_metric: Option<PrimaryMetricStats>,
    /// Mean relative to the fastest result (1.0 = fastest), as in the Markdown export.
    /// Omitted if it cannot be computed (e.g. a mean of zero).
    #[serde(skip_serializing_if = "Option::is_none")]
    relative_speed: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    relative_speed_stddev: Option<f64>,
    /// Bootstrap interval `[low, high]` of `relative_speed` at
    /// `perfratio.statistics.confidence_level`, for estimators other than the
    /// mean (new in 1.21, #320)
    #[serde(skip_serializing_if = "Option::is_none")]
    relative_speed_ci: Option<[f64; 2]>,
    /// Significance of the difference to the reference (#215). Omitted for the
    /// reference itself and if the test is undefined (fewer than 3 runs, no per-run times).
    #[serde(skip_serializing_if = "Option::is_none")]
    significance: Option<Significance>,
    /// p-value of the bootstrap Welch test against the reference
    #[serde(skip_serializing_if = "Option::is_none")]
    p_value: Option<f64>,
    /// `p_value` after `--correction` (new in 1.22, #321); `significance`
    /// is decided on this value. Omitted without a correction.
    #[serde(skip_serializing_if = "Option::is_none")]
    p_value_adjusted: Option<f64>,
}

/// The `perfratio` object: the run metadata plus the descriptor of every metric (#317).
fn with_metrics(metadata: &ExportMetadata, results: &[BenchmarkResult]) -> serde_json::Value {
    let mut value = serde_json::to_value(metadata).unwrap_or(serde_json::Value::Null);
    if let (Some(object), Ok(metrics)) = (
        value.as_object_mut(),
        serde_json::to_value(crate::metric_registry::descriptors(results, metadata)),
    ) {
        object.insert("metrics".into(), metrics);
    }
    value
}

/// `perfratio.statistics.correction` (new in 1.22, #321), only with `--correction`.
fn add_correction(
    perfratio: &mut serde_json::Value,
    settings: &crate::stats::settings::StatSettings,
    assessments: &significance::FamilyAssessment,
) {
    if settings.correction.is_none() {
        return;
    }
    if let Some(statistics) = perfratio
        .get_mut("statistics")
        .and_then(|s| s.as_object_mut())
    {
        statistics.insert(
            "correction".into(),
            serde_json::json!({
                "method": settings.correction.name(),
                "family": settings.family.name(),
                "family_size": assessments.family_size,
                "alpha": settings.significance_level,
            }),
        );
    }
}

#[derive(Default)]
pub struct JsonExporter {
    pub metadata: Option<ExportMetadata>,
    /// Result of `--optimize`, set once the search is done
    pub optimize: std::sync::Mutex<Option<serde_json::Value>>,
    /// Pareto analysis of the results being serialized
    pub pareto: std::sync::Mutex<Option<Analysis>>,
}

impl Exporter for JsonExporter {
    fn serialize(
        &self,
        results: &[BenchmarkResult],
        reference: Option<&BenchmarkResult>,
        _unit: Option<Unit>,
        _sort_order: SortOrder,
        metric: Metric,
    ) -> Result<Vec<u8>> {
        let relative = if results.is_empty() {
            None
        } else {
            let baseline = reference.unwrap_or_else(|| relative_speed::best_of(results, metric));
            relative_speed::compute_with_check_from_reference(
                results,
                baseline,
                SortOrder::Command,
                metric,
            )
        };
        let reference_result = reference
            .or_else(|| (!results.is_empty()).then(|| relative_speed::fastest_of(results)));
        let settings = crate::stats::settings::current();
        let assessments = reference_result.map(|reference| {
            let all: Vec<&BenchmarkResult> = results.iter().collect();
            significance::assess_family(reference, &all, &settings)
        });
        let mut perfratio = self.metadata.as_ref().map(|m| with_metrics(m, results));
        if let (Some(value), Some(assessments)) = (perfratio.as_mut(), assessments.as_ref()) {
            add_correction(value, &settings, assessments);
        }
        let results = results
            .iter()
            .enumerate()
            .map(|(i, result)| {
                let entry = relative.as_ref().map(|r| &r[i]);
                // omitted when the result has no data for the metric
                let primary_metric = if metric != Metric::Wall {
                    result.primary_mean(metric).map(|mean| PrimaryMetricStats {
                        metric,
                        mean,
                        stddev: result.primary_stddev(metric),
                        samples: result.primary_samples(metric),
                    })
                } else {
                    None
                };
                let assessment = assessments.as_ref().and_then(|a| a.get(result));
                ResultEntry {
                    result,
                    primary_metric,
                    relative_speed: entry.map(|e| e.relative_speed).filter(|r| r.is_finite()),
                    // the baseline has no spread relative to itself
                    relative_speed_stddev: entry
                        .filter(|e| !e.is_reference)
                        .and_then(|e| e.relative_speed_stddev),
                    relative_speed_ci: entry
                        .filter(|e| !e.is_reference)
                        .and_then(|e| e.relative_speed_ci)
                        .map(|(lo, hi)| [lo, hi]),
                    significance: assessment.map(|a| a.significance),
                    p_value: assessment.map(|a| a.p_value),
                    p_value_adjusted: assessment.and_then(|a| a.p_adjusted),
                }
            })
            .collect();

        let mut output = to_vec_pretty(&PerfratioSummary {
            schema_version: SCHEMA_VERSION,
            metric,
            perfratio,
            interrupted: crate::util::interrupt::interrupted(),
            optimize: self
                .optimize
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            pareto: self
                .pareto
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            results,
        });
        if let Ok(ref mut content) = output {
            content.push(b'\n');
        }

        Ok(output?)
    }

    fn set_optimize(&self, value: Option<&serde_json::Value>) {
        *self.optimize.lock().unwrap_or_else(|e| e.into_inner()) = value.cloned();
    }

    fn set_pareto(&self, analysis: Option<(&ParetoConfig, &Analysis)>) {
        *self.pareto.lock().unwrap_or_else(|e| e.into_inner()) = analysis.map(|(_, a)| a.clone());
    }
}
