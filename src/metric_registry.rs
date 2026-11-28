//! Metric descriptor registry (#317): one table that describes every metric of the JSON export
//! (`perfratio.metrics[]`, schemaVersion 1.20). See `docs/specs/metric-descriptors.md`.
//!
//! A descriptor says what a metric is (`unit`, `direction`, `scope`, `source`, `kind`) and
//! whether this run could measure it (`status`, `reason`). It is generated from [`REGISTRY`]
//! plus the keys that are only known at run time (counter names, `--output-metric` names,
//! energy domains), so a new exported metric needs a registry entry. `NON_METRIC_FIELDS` lists
//! the result keys that are not metrics; a test checks that every key of the result schema is
//! in one of the two lists.
//!
//! An unavailable metric keeps its descriptor (with the reason) and has no values in the
//! results: it is never written as `0`.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::benchmark::benchmark_result::{BenchmarkResult, RegionStatus};
use crate::export::metadata::{ExportMetadata, MeasurementStatus, Measurements};

/// Which way is better. Ranking code that needs it reads it from here.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    LowerIsBetter,
    HigherIsBetter,
    Neutral,
}

/// What the value covers.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Process,
    ProcessTree,
    Batch,
    Package,
    System,
    Region,
}

/// How the value was obtained.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Measured,
    Estimated,
    Simulated,
    Derived,
}

/// Availability of a metric in this export (open enum: consumers must tolerate new values).
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Available,
    Partial,
    Unsupported,
    PermissionDenied,
    BelowResolution,
    Contaminated,
    Invalid,
    NotRequested,
}

/// The measurement group that decides the status of a registry entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Time,
    Memory,
    Energy,
    Resources,
    Counters,
    Topdown,
    Simd,
    /// Only listed when some result carries one of its fields
    Concurrency,
}

/// Static part of a descriptor.
#[derive(Debug, Clone, Copy)]
pub struct Spec {
    pub id: &'static str,
    pub unit: &'static str,
    pub direction: Direction,
    pub scope: Scope,
    pub source: &'static str,
    pub kind: Kind,
    pub group: Group,
    /// Keys of a result (`results[]`) that carry this metric
    pub fields: &'static [&'static str],
}

use Direction::*;
use Group::*;
use Kind::*;

const RUSAGE: &str = "os.rusage";
const PERF: &str = "linux.perf_event_open";

#[allow(clippy::too_many_arguments)]
const fn spec(
    id: &'static str,
    unit: &'static str,
    direction: Direction,
    scope: Scope,
    source: &'static str,
    kind: Kind,
    group: Group,
    fields: &'static [&'static str],
) -> Spec {
    Spec {
        id,
        unit,
        direction,
        scope,
        source,
        kind,
        group,
        fields,
    }
}

/// Every fixed metric of the export. Metrics named at run time are built by [`descriptors`].
pub const REGISTRY: &[Spec] = &[
    spec(
        "time.wall",
        "s",
        LowerIsBetter,
        Scope::ProcessTree,
        "perfratio.timer",
        Measured,
        Time,
        &[
            "mean",
            "stddev",
            "median",
            "min",
            "max",
            "times",
            "percentiles",
            "geometric_mean",
            "first_run",
        ],
    ),
    spec(
        "time.user",
        "s",
        LowerIsBetter,
        Scope::ProcessTree,
        RUSAGE,
        Measured,
        Time,
        &["user", "user_times"],
    ),
    spec(
        "time.system",
        "s",
        LowerIsBetter,
        Scope::ProcessTree,
        RUSAGE,
        Measured,
        Time,
        &["system", "system_times"],
    ),
    spec(
        "cpu.utilization",
        "percent",
        Neutral,
        Scope::ProcessTree,
        "perfratio.derived",
        Derived,
        Time,
        &["cpu_percent"],
    ),
    spec(
        "memory.peak_rss",
        "bytes",
        LowerIsBetter,
        Scope::ProcessTree,
        RUSAGE,
        Measured,
        Memory,
        &["memory_usage_byte"],
    ),
    spec(
        "energy.total",
        "J",
        LowerIsBetter,
        Scope::System,
        "",
        Measured,
        Energy,
        &["mean_energy_joules", "energy_joules"],
    ),
    spec(
        "energy.net",
        "J",
        LowerIsBetter,
        Scope::System,
        "",
        Derived,
        Energy,
        &["mean_net_energy_joules"],
    ),
    spec(
        "power.average",
        "W",
        LowerIsBetter,
        Scope::System,
        "",
        Derived,
        Energy,
        &["mean_watts"],
    ),
    spec(
        "power.idle",
        "W",
        Neutral,
        Scope::System,
        "",
        Measured,
        Energy,
        &["idle_watts"],
    ),
    spec(
        "resources.voluntary_ctx_switches",
        "count",
        LowerIsBetter,
        Scope::ProcessTree,
        RUSAGE,
        Measured,
        Resources,
        &["resources"],
    ),
    spec(
        "resources.involuntary_ctx_switches",
        "count",
        LowerIsBetter,
        Scope::ProcessTree,
        RUSAGE,
        Measured,
        Resources,
        &["resources"],
    ),
    spec(
        "resources.minor_faults",
        "count",
        LowerIsBetter,
        Scope::ProcessTree,
        RUSAGE,
        Measured,
        Resources,
        &["resources"],
    ),
    spec(
        "resources.major_faults",
        "count",
        LowerIsBetter,
        Scope::ProcessTree,
        RUSAGE,
        Measured,
        Resources,
        &["resources"],
    ),
    spec(
        "resources.block_input_ops",
        "count",
        LowerIsBetter,
        Scope::ProcessTree,
        RUSAGE,
        Measured,
        Resources,
        &["resources"],
    ),
    spec(
        "resources.block_output_ops",
        "count",
        LowerIsBetter,
        Scope::ProcessTree,
        RUSAGE,
        Measured,
        Resources,
        &["resources"],
    ),
    spec(
        "cpu.ipc",
        "ratio",
        HigherIsBetter,
        Scope::ProcessTree,
        "perfratio.derived",
        Derived,
        Counters,
        &["efficiency"],
    ),
    spec(
        "cpu.cpi",
        "ratio",
        LowerIsBetter,
        Scope::ProcessTree,
        "perfratio.derived",
        Derived,
        Counters,
        &["efficiency"],
    ),
    spec(
        "cache.miss_rate",
        "ratio",
        LowerIsBetter,
        Scope::ProcessTree,
        "perfratio.derived",
        Derived,
        Counters,
        &["efficiency"],
    ),
    spec(
        "branch.miss_rate",
        "ratio",
        LowerIsBetter,
        Scope::ProcessTree,
        "perfratio.derived",
        Derived,
        Counters,
        &["efficiency"],
    ),
    spec(
        "cpu.effective_frequency",
        "Hz",
        Neutral,
        Scope::ProcessTree,
        "perfratio.derived",
        Derived,
        Counters,
        &[
            "effective_frequency_hz",
            "mean_effective_frequency_hz",
            "stddev_effective_frequency_hz",
        ],
    ),
    spec(
        "topdown.retiring",
        "ratio",
        HigherIsBetter,
        Scope::ProcessTree,
        PERF,
        Derived,
        Topdown,
        &["topdown", "topdown_summary"],
    ),
    spec(
        "topdown.bad_speculation",
        "ratio",
        LowerIsBetter,
        Scope::ProcessTree,
        PERF,
        Derived,
        Topdown,
        &["topdown", "topdown_summary"],
    ),
    spec(
        "topdown.frontend_bound",
        "ratio",
        LowerIsBetter,
        Scope::ProcessTree,
        PERF,
        Derived,
        Topdown,
        &["topdown", "topdown_summary"],
    ),
    spec(
        "topdown.backend_bound",
        "ratio",
        LowerIsBetter,
        Scope::ProcessTree,
        PERF,
        Derived,
        Topdown,
        &["topdown", "topdown_summary"],
    ),
    spec(
        "simd.scalar_fp",
        "count",
        Neutral,
        Scope::ProcessTree,
        PERF,
        Measured,
        Simd,
        &["simd", "simd_summary", "simd_scaling"],
    ),
    spec(
        "simd.simd_128",
        "count",
        Neutral,
        Scope::ProcessTree,
        PERF,
        Measured,
        Simd,
        &["simd", "simd_summary", "simd_scaling"],
    ),
    spec(
        "simd.simd_256",
        "count",
        Neutral,
        Scope::ProcessTree,
        PERF,
        Measured,
        Simd,
        &["simd", "simd_summary", "simd_scaling"],
    ),
    spec(
        "simd.simd_512",
        "count",
        Neutral,
        Scope::ProcessTree,
        PERF,
        Measured,
        Simd,
        &["simd", "simd_summary", "simd_scaling"],
    ),
    spec(
        "simd.vectorization_ratio",
        "ratio",
        HigherIsBetter,
        Scope::ProcessTree,
        "perfratio.derived",
        Derived,
        Simd,
        &["simd", "simd_summary", "simd_scaling"],
    ),
    spec(
        "batch.throughput",
        "runs/s",
        HigherIsBetter,
        Scope::Batch,
        "perfratio.derived",
        Derived,
        Concurrency,
        &["throughput"],
    ),
    spec(
        "batch.instance_time",
        "s",
        LowerIsBetter,
        Scope::Process,
        "perfratio.timer",
        Measured,
        Concurrency,
        &["instance_times", "instance_percentiles"],
    ),
];

/// Result keys that carry no metric: identity, configuration, run bookkeeping, diagnostics
/// about the measurement itself. Together with `REGISTRY[].fields` and the dynamic metrics
/// (`counters*`, `custom_metrics*`, `energy_domains_joules`) this must cover every key of
/// `$defs/result` in the JSON Schema.
pub const NON_METRIC_FIELDS: &[&str] = &[
    "command",
    "mode",
    "iterations",
    "command_with_unused_parameters",
    "parameters",
    "per_run_parameters",
    "shell",
    "exit_codes",
    "timed_out",
    "timeout",
    "warmup_runs",
    "runs_done",
    "runs_planned",
    "target_duration_seconds",
    "total_sampling_seconds",
    "omitted_failed_runs",
    "discarded_outliers",
    "diagnostics",
    "outliers",
    "precision",
    "baseline",
    "validation",
    "quality",
    "interference",
    "significance",
    "p_value",
    "p_value_adjusted",
    "effect",
    "classification",
    "equivalence_p_value",
    "power",
    "precision_metrics",
    "hierarchy",
    "paired",
    "relative_speed",
    "relative_speed_stddev",
    "relative_speed_ci",
    "primary_metric",
    "energy_samples",
    "energy_scope",
    "counters_scope",
    "counters_coverage",
    "counters_multiplexed_runs",
    "effective_frequency_basis",
    "simd_scope",
    "concurrency",
    "run_ids",
    "instance_run_ids",
    "leftover_processes",
    "energy_per_instance_joules",
    "workers_used",
    "contended",
];

/// Result keys whose names inside the object are metrics decided at run time.
pub const DYNAMIC_FIELDS: &[&str] = &[
    "counters",
    "counters_summary",
    "custom_metrics",
    "custom_metrics_summary",
    "energy_domains_joules",
    "gpu_utilization_percent",
    "regions",
];

/// One entry of `perfratio.metrics[]`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MetricDescriptor {
    pub id: String,
    pub unit: String,
    pub direction: Direction,
    pub scope: Scope,
    pub source: String,
    pub kind: Kind,
    pub status: Status,
    /// Why the metric is `unsupported`, `permission_denied` or `partial` (open enum, same
    /// tokens as `perfratio.measurements.*_unavailable_reason`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Keys of `results[]` that hold the values
    pub fields: Vec<String>,
}

/// Reasons that mean "the OS refused", the others mean "this hardware or platform cannot".
fn is_permission(reason: &str) -> bool {
    matches!(
        reason,
        "perf_paranoid" | "kernel_scope_denied" | "rapl_unreadable" | "emi_unreadable"
    )
}

/// Maps a group status and its reason to a metric status.
pub fn status_of(status: MeasurementStatus, reason: Option<&str>) -> Status {
    match status {
        MeasurementStatus::Measured => Status::Available,
        MeasurementStatus::Partial => Status::Partial,
        MeasurementStatus::NotRequested => Status::NotRequested,
        MeasurementStatus::Unavailable => match reason {
            Some(r) if is_permission(r) => Status::PermissionDenied,
            _ => Status::Unsupported,
        },
    }
}

fn group_status(group: Group, m: &Measurements) -> (MeasurementStatus, Option<&'static str>) {
    match group {
        Time => (m.time, None),
        Memory => (m.memory, None),
        Energy => (m.energy, m.energy_unavailable_reason),
        Resources => (m.resources, None),
        Counters => (m.counters, m.counters_unavailable_reason),
        Topdown => (m.topdown, m.topdown_unavailable_reason),
        Simd => (m.simd, m.simd_unavailable_reason),
        Concurrency => (MeasurementStatus::Measured, None),
    }
}

/// `task-clock` is in nanoseconds, the other counters are event counts.
fn counter_unit(key: &str) -> &'static str {
    let base = key.split(':').next().unwrap_or(key);
    if base == "task-clock" {
        "ns"
    } else {
        "count"
    }
}

fn has_field(results: &[BenchmarkResult], field: &str) -> bool {
    results
        .iter()
        .filter_map(|r| serde_json::to_value(r).ok())
        .any(|v| v.get(field).is_some_and(|f| !f.is_null()))
}

fn keys_of(results: &[BenchmarkResult], pick: fn(&BenchmarkResult) -> Vec<String>) -> Vec<String> {
    let set: BTreeSet<String> = results.iter().flat_map(pick).collect();
    set.into_iter().collect()
}

/// Builds `perfratio.metrics[]` for an export: the registry entries of every group that was
/// requested (time and memory always), plus the metrics named at run time.
pub fn descriptors(
    results: &[BenchmarkResult],
    metadata: &ExportMetadata,
) -> Vec<MetricDescriptor> {
    let m = &metadata.measurements;
    let mut out = Vec::new();

    let energy_scope = results
        .iter()
        .find_map(|r| r.energy_scope.clone())
        .map(|s| {
            if s == "batch" {
                Scope::Batch
            } else {
                Scope::System
            }
        })
        .unwrap_or(Scope::System);
    let energy_source = metadata.energy_backend.unwrap_or("unknown");

    for spec in REGISTRY {
        let (group, reason) = group_status(spec.group, m);
        let status = status_of(group, reason);
        let listed = match spec.group {
            Concurrency => spec.fields.iter().any(|f| has_field(results, f)),
            // derived rates exist only when counters were requested
            _ => status != Status::NotRequested,
        };
        if !listed {
            continue;
        }
        let energy = spec.group == Energy;
        out.push(MetricDescriptor {
            id: spec.id.to_string(),
            unit: spec.unit.to_string(),
            direction: spec.direction,
            scope: if energy { energy_scope } else { spec.scope },
            source: if energy {
                energy_source.to_string()
            } else {
                spec.source.to_string()
            },
            kind: spec.kind,
            status,
            reason: reason.map(str::to_string),
            fields: spec.fields.iter().map(|f| f.to_string()).collect(),
        });
    }

    // energy domains named by the backend
    for domain in keys_of(results, |r| {
        r.energy_domains_joules
            .as_ref()
            .map(|d| d.keys().cloned().collect())
            .unwrap_or_default()
    }) {
        out.push(MetricDescriptor {
            id: format!("energy.domain.{domain}"),
            unit: "J".into(),
            direction: LowerIsBetter,
            scope: Scope::Package,
            source: energy_source.into(),
            kind: Measured,
            status: Status::Available,
            reason: None,
            fields: vec!["energy_domains_joules".into()],
        });
    }

    // counters that were read, then the ones requested but not openable (never written as 0)
    let counter_fields = || vec!["counters".to_string(), "counters_summary".to_string()];
    let counter_keys = keys_of(results, |r| {
        r.counters
            .as_ref()
            .map(|c| c.keys().cloned().collect())
            .unwrap_or_default()
    });
    for key in &counter_keys {
        let partial_run = results
            .iter()
            .filter_map(|r| r.counters.as_ref()?.get(key))
            .any(|runs| runs.iter().any(Option::is_none));
        out.push(MetricDescriptor {
            id: format!("counter.{key}"),
            unit: counter_unit(key).into(),
            direction: LowerIsBetter,
            scope: Scope::ProcessTree,
            source: PERF.into(),
            kind: Measured,
            status: if partial_run {
                Status::Partial
            } else {
                Status::Available
            },
            reason: None,
            fields: counter_fields(),
        });
    }
    let counters_status = status_of(m.counters, m.counters_unavailable_reason);
    if let Some(events) = &m.counters_unavailable_events {
        for key in events.iter().filter(|k| !counter_keys.contains(k)) {
            out.push(MetricDescriptor {
                id: format!("counter.{key}"),
                unit: counter_unit(key).into(),
                direction: LowerIsBetter,
                scope: Scope::ProcessTree,
                source: PERF.into(),
                kind: Measured,
                status: match m.counters_unavailable_reason {
                    Some(r) if is_permission(r) => Status::PermissionDenied,
                    _ => Status::Unsupported,
                },
                reason: m.counters_unavailable_reason.map(str::to_string),
                fields: counter_fields(),
            });
        }
    } else if counter_keys.is_empty() && counters_status != Status::NotRequested {
        // the whole group failed before any event name was known
        out.push(MetricDescriptor {
            id: "counter".into(),
            unit: "count".into(),
            direction: LowerIsBetter,
            scope: Scope::ProcessTree,
            source: PERF.into(),
            kind: Measured,
            status: counters_status,
            reason: m.counters_unavailable_reason.map(str::to_string),
            fields: counter_fields(),
        });
    }

    // `--output-metric`: unitless and lower is better unless declared (`NAME[unit,higher]`)
    for name in keys_of(results, |r| {
        r.custom_metrics
            .as_ref()
            .map(|c| c.keys().cloned().collect())
            .unwrap_or_default()
    }) {
        let missing = results
            .iter()
            .filter_map(|r| r.custom_metrics.as_ref()?.get(&name))
            .any(|runs| runs.iter().any(Option::is_none));
        let meta = crate::metric::custom_meta(&name);
        // `--work` and the energy-delay products (#373), computed from the measured series
        let derived = crate::efficiency::is_derived(&name);
        out.push(MetricDescriptor {
            id: format!("custom.{name}"),
            unit: meta
                .as_ref()
                .and_then(|m| m.unit.clone())
                .unwrap_or_else(|| "unitless".into()),
            direction: if meta.as_ref().is_some_and(|m| m.higher_is_better) {
                HigherIsBetter
            } else {
                LowerIsBetter
            },
            scope: Scope::Process,
            source: if derived {
                "perfratio.derived".into()
            } else {
                "command_output".into()
            },
            kind: if derived { Derived } else { Measured },
            status: if missing {
                Status::Partial
            } else {
                Status::Available
            },
            reason: missing.then(|| "no_match_in_some_runs".to_string()),
            fields: vec!["custom_metrics".into(), "custom_metrics_summary".into()],
        });
    }

    // `--region`: seconds inside each instrumented region, scoped to the region
    for name in keys_of(results, |r| {
        r.regions
            .iter()
            .flatten()
            .map(|region| region.name.clone())
            .collect()
    }) {
        let missing = results
            .iter()
            .filter_map(|r| {
                r.regions
                    .as_ref()?
                    .iter()
                    .find(|region| region.name == name)
            })
            .any(|region| region.status == RegionStatus::RegionMissing);
        out.push(MetricDescriptor {
            id: format!("region.{name}"),
            unit: "seconds".into(),
            direction: LowerIsBetter,
            scope: Scope::Region,
            source: "region_sdk".into(),
            kind: Measured,
            status: if missing {
                Status::Partial
            } else {
                Status::Available
            },
            reason: missing.then(|| "region_missing".to_string()),
            fields: vec!["regions".into()],
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn ids_are_unique_and_lowercase_dotted() {
        let ids: BTreeSet<_> = REGISTRY.iter().map(|s| s.id).collect();
        assert_eq!(ids.len(), REGISTRY.len());
        for id in ids {
            assert!(id.contains('.') && id == id.to_lowercase(), "{id}");
        }
    }

    #[test]
    fn higher_is_better_is_declared_for_rates() {
        let dir = |id: &str| REGISTRY.iter().find(|s| s.id == id).unwrap().direction;
        assert_eq!(dir("cpu.ipc"), HigherIsBetter);
        assert_eq!(dir("batch.throughput"), HigherIsBetter);
        assert_eq!(dir("time.wall"), LowerIsBetter);
    }

    #[test]
    fn registry_fields_do_not_overlap_the_non_metric_list() {
        for spec in REGISTRY {
            for f in spec.fields {
                assert!(!NON_METRIC_FIELDS.contains(f), "{f} in {}", spec.id);
            }
        }
    }

    #[test]
    fn group_status_maps_reasons() {
        use MeasurementStatus::*;
        assert_eq!(status_of(Measured, None), Status::Available);
        assert_eq!(status_of(NotRequested, None), Status::NotRequested);
        assert_eq!(status_of(Partial, Some("no_pmu")), Status::Partial);
        assert_eq!(
            status_of(Unavailable, Some("perf_paranoid")),
            Status::PermissionDenied
        );
        assert_eq!(
            status_of(Unavailable, Some("rapl_not_found")),
            Status::Unsupported
        );
        assert_eq!(status_of(Unavailable, None), Status::Unsupported);
    }

    #[test]
    fn counter_units() {
        assert_eq!(counter_unit("task-clock:u"), "ns");
        assert_eq!(counter_unit("instructions"), "count");
    }
}
