use serde::*;
use serde_json::to_vec_pretty;

use super::metadata::ExportMetadata;
use super::Exporter;
use crate::benchmark::benchmark_result::BenchmarkResult;
use crate::benchmark::relative_speed;
use crate::options::SortOrder;
use crate::stats::significance::{self, Significance};
use crate::util::units::Unit;

use anyhow::Result;

#[derive(Serialize, Debug)]
struct PerfratioSummary<'a> {
    /// Information about the perfratio run (version, command line, date, labels, system)
    #[serde(skip_serializing_if = "Option::is_none")]
    perfratio: Option<&'a ExportMetadata>,
    /// Backwards compatibility alias for older consumers
    #[serde(skip_serializing_if = "Option::is_none")]
    joulex: Option<&'a ExportMetadata>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    interrupted: bool,
    results: Vec<ResultEntry<'a>>,
}

#[derive(Serialize, Debug)]
struct ResultEntry<'a> {
    #[serde(flatten)]
    result: &'a BenchmarkResult,
    /// Mean relative to the fastest result (1.0 = fastest), as in the Markdown export.
    /// Omitted if it cannot be computed (e.g. a mean of zero).
    #[serde(skip_serializing_if = "Option::is_none")]
    relative_speed: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    relative_speed_stddev: Option<f64>,
    /// Significance of the difference to the reference (#215). Omitted for the
    /// reference itself and if the test is undefined (fewer than 3 runs, no per-run times).
    #[serde(skip_serializing_if = "Option::is_none")]
    significance: Option<Significance>,
    /// p-value of the bootstrap Welch test against the reference
    #[serde(skip_serializing_if = "Option::is_none")]
    p_value: Option<f64>,
}

#[derive(Default)]
pub struct JsonExporter {
    pub metadata: Option<ExportMetadata>,
}

impl Exporter for JsonExporter {
    fn serialize(
        &self,
        results: &[BenchmarkResult],
        reference: Option<&BenchmarkResult>,
        _unit: Option<Unit>,
        _sort_order: SortOrder,
    ) -> Result<Vec<u8>> {
        let relative = if results.is_empty() {
            None
        } else {
            let baseline = reference.unwrap_or_else(|| relative_speed::fastest_of(results));
            relative_speed::compute_with_check_from_reference(results, baseline, SortOrder::Command)
        };
        let reference_result = reference
            .or_else(|| (!results.is_empty()).then(|| relative_speed::fastest_of(results)));
        let results = results
            .iter()
            .enumerate()
            .map(|(i, result)| {
                let entry = relative.as_ref().map(|r| &r[i]);
                let assessment = reference_result
                    .filter(|r| !std::ptr::eq(*r, result))
                    .and_then(|r| significance::assess(r, result));
                ResultEntry {
                    result,
                    relative_speed: entry.map(|e| e.relative_speed).filter(|r| r.is_finite()),
                    // the baseline has no spread relative to itself
                    relative_speed_stddev: entry
                        .filter(|e| !e.is_reference)
                        .and_then(|e| e.relative_speed_stddev),
                    significance: assessment.map(|a| a.significance),
                    p_value: assessment.map(|a| a.p_value),
                }
            })
            .collect();

        let mut output = to_vec_pretty(&PerfratioSummary {
            perfratio: self.metadata.as_ref(),
            joulex: self.metadata.as_ref(),
            interrupted: crate::util::interrupt::interrupted(),
            results,
        });
        if let Ok(ref mut content) = output {
            content.push(b'\n');
        }

        Ok(output?)
    }
}
