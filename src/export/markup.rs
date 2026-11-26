use crate::benchmark::relative_speed::BenchmarkResultWithRelativeSpeed;
use crate::benchmark::{benchmark_result::BenchmarkResult, relative_speed};
use crate::options::SortOrder;
use crate::output::format::format_duration_value;
use crate::util::units::Unit;

use super::Exporter;
use anyhow::Result;

type EfficiencyField = fn(&BenchmarkResult) -> Option<f64>;

/// Derived counter metrics shown as extra table columns, see
/// `docs/specs/derived-counter-metrics.md`.
const EFFICIENCY_COLUMNS: [(&str, EfficiencyField); 4] = [
    ("IPC", |r| r.efficiency.and_then(|e| e.ipc)),
    ("CPI", |r| r.efficiency.and_then(|e| e.cpi)),
    ("Cache miss rate [%]", |r| {
        r.efficiency
            .and_then(|e| e.cache_miss_rate)
            .map(|v| v * 100.0)
    }),
    ("Branch miss rate [%]", |r| {
        r.efficiency
            .and_then(|e| e.branch_miss_rate)
            .map(|v| v * 100.0)
    }),
];

fn format_efficiency(value: f64) -> String {
    format!("{value:.2}")
}

pub enum Alignment {
    Left,
    Right,
}

pub trait MarkupExporter {
    fn table_results(&self, entries: &[BenchmarkResultWithRelativeSpeed], unit: Unit) -> String {
        // prepare table header strings
        let notation = format!("[{}]", unit.short_name());

        // Derived counter columns (#213) only when `--counters` produced them
        let efficiency_columns: Vec<(&str, EfficiencyField)> = EFFICIENCY_COLUMNS
            .iter()
            .copied()
            .filter(|(_, get)| entries.iter().any(|e| get(e.result).is_some()))
            .collect();

        // prepare table cells alignment
        let mut cells_alignment = vec![
            Alignment::Left,
            Alignment::Right,
            Alignment::Right,
            Alignment::Right,
            Alignment::Right,
        ];
        cells_alignment.extend(efficiency_columns.iter().map(|_| Alignment::Right));

        // emit table header format
        let mut table = self.table_header(&cells_alignment);

        // emit table header data
        let mut header: Vec<String> = vec![
            "Command".into(),
            format!("Mean {notation}"),
            format!("Min {notation}"),
            format!("Max {notation}"),
            "Relative".into(),
        ];
        header.extend(efficiency_columns.iter().map(|(name, _)| name.to_string()));
        let header: Vec<&str> = header.iter().map(String::as_str).collect();
        table.push_str(&self.table_row(&header));

        // emit horizontal line
        table.push_str(&self.table_divider(&cells_alignment));

        for entry in entries {
            let measurement = &entry.result;
            // prepare data row strings
            // Each exporter escapes the command for its own markup in `command()`.
            let cmd_str = measurement.command_with_unused_parameters.as_str();
            let (mean_str, stddev_value) = crate::output::format::format_mean_stddev_values(
                measurement.mean,
                measurement.stddev,
                unit,
            );
            let stddev_str = if let Some(stddev) = stddev_value {
                format!(" ± {stddev}")
            } else {
                "".into()
            };
            let mean_cell = if measurement.timed_out {
                let timeout_sec = measurement.timeout.unwrap_or(measurement.mean);
                let (timeout_val, _) = format_duration_value(timeout_sec, Some(unit));
                format!(">{timeout_val} (timeout)")
            } else {
                format!("{mean_str}{stddev_str}")
            };
            let min_str = format_duration_value(measurement.min, Some(unit)).0;
            let max_str = format_duration_value(measurement.max, Some(unit)).0;
            let (min_cell, max_cell) = if measurement.timed_out
                && measurement.times.as_ref().is_none_or(|t| t.is_empty())
            {
                ("n/a".into(), "n/a".into())
            } else {
                (min_str, max_str)
            };
            let rel_str = if entry.relative_speed.is_finite() {
                format!("{:.2}", entry.relative_speed)
            } else {
                "n/a".into()
            };
            let rel_stddev_str = if entry.is_reference {
                "".into()
            } else if let Some(stddev) = entry.relative_speed_stddev {
                if stddev.is_finite() {
                    format!(" ± {stddev:.2}")
                } else {
                    "".into()
                }
            } else {
                "".into()
            };

            // prepare table row entries
            let mut cells = vec![
                self.command(cmd_str),
                mean_cell,
                min_cell,
                max_cell,
                format!("{rel_str}{rel_stddev_str}"),
            ];
            cells.extend(efficiency_columns.iter().map(|(_, get)| {
                get(measurement).map_or_else(|| "n/a".to_string(), format_efficiency)
            }));
            let cells: Vec<&str> = cells.iter().map(String::as_str).collect();
            table.push_str(&self.table_row(&cells))
        }

        // emit table footer format
        table.push_str(&self.table_footer(&cells_alignment));

        table
    }

    fn table_row(&self, cells: &[&str]) -> String;

    fn table_divider(&self, cell_aligmnents: &[Alignment]) -> String;

    fn table_header(&self, _cell_aligmnents: &[Alignment]) -> String {
        "".to_string()
    }

    fn table_footer(&self, _cell_aligmnents: &[Alignment]) -> String {
        "".to_string()
    }

    /// Render a (possibly untrusted) command string as an inline-code table cell.
    /// Implementations must make sure the text cannot close the code span or the
    /// table cell (see #25).
    fn command(&self, cmd: &str) -> String;

    /// A section heading for one command (used by the per-run exports).
    fn heading(&self, cmd: &str) -> String;
}

pub(super) fn determine_unit_from_results(results: &[BenchmarkResult]) -> Unit {
    if let Some(first_result) = results.first() {
        // Use the first BenchmarkResult entry to determine the unit for all entries.
        format_duration_value(first_result.mean, None).1
    } else {
        // Default to `Second`.
        Unit::Second
    }
}

impl<T: MarkupExporter> Exporter for T {
    fn serialize(
        &self,
        results: &[BenchmarkResult],
        reference: Option<&BenchmarkResult>,
        unit: Option<Unit>,
        sort_order: SortOrder,
        metric: crate::metric::Metric,
    ) -> Result<Vec<u8>> {
        let unit = unit.unwrap_or_else(|| determine_unit_from_results(results));
        let entries = if results.is_empty() {
            Vec::new()
        } else {
            let baseline = reference.unwrap_or_else(|| relative_speed::best_of(results, metric));
            relative_speed::compute_with_check_from_reference(results, baseline, sort_order, metric)
                .unwrap_or_else(|| {
                    relative_speed::compute_without_ratios(results, baseline, sort_order, metric)
                })
        };

        let table = self.table_results(&entries, unit);
        Ok(table.as_bytes().to_vec())
    }
}
