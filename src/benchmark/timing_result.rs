use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::util::units::Second;

/// OS resource counters of a single run. A counter the platform cannot report is
/// `None` (unavailable), never 0. See `docs/specs/windows-resource-usage.md`.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
pub struct ResourceCounters {
    /// Voluntary context switches: the process blocked (I/O, locks, sleep)
    pub voluntary_ctx_switches: Option<u64>,
    /// Involuntary context switches: the process was preempted
    pub involuntary_ctx_switches: Option<u64>,
    /// Page faults served without I/O (Windows: all page faults, soft and hard)
    pub minor_faults: Option<u64>,
    /// Page faults that required I/O
    pub major_faults: Option<u64>,
    /// Block input operations (Windows: read operations issued)
    pub block_input_ops: Option<u64>,
    /// Block output operations (Windows: write operations issued)
    pub block_output_ops: Option<u64>,
}

impl ResourceCounters {
    /// Counters as Windows reports them: job object accounting gives the total
    /// page fault count and the read/write operation counts of the whole process
    /// tree. Windows has no context switch counters per process and does not
    /// split page faults into minor and major, so those stay unavailable.
    pub fn from_windows(page_faults: u64, read_ops: u64, write_ops: u64) -> Self {
        ResourceCounters {
            voluntary_ctx_switches: None,
            involuntary_ctx_switches: None,
            minor_faults: Some(page_faults),
            major_faults: None,
            block_input_ops: Some(read_ops),
            block_output_ops: Some(write_ops),
        }
    }
}

/// Per-run resource counters of a benchmark, exported with `--resource-usage`.
/// A column is `None` (omitted from the JSON) when the platform cannot report it.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceSeries {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voluntary_ctx_switches: Option<Vec<u64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub involuntary_ctx_switches: Option<Vec<u64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minor_faults: Option<Vec<u64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub major_faults: Option<Vec<u64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_input_ops: Option<Vec<u64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_output_ops: Option<Vec<u64>>,
}

impl ResourceSeries {
    pub fn from_counters(counters: &[ResourceCounters]) -> Self {
        let column = |f: fn(&ResourceCounters) -> Option<u64>| -> Option<Vec<u64>> {
            counters.iter().map(f).collect()
        };
        ResourceSeries {
            voluntary_ctx_switches: column(|c| c.voluntary_ctx_switches),
            involuntary_ctx_switches: column(|c| c.involuntary_ctx_switches),
            minor_faults: column(|c| c.minor_faults),
            major_faults: column(|c| c.major_faults),
            block_input_ops: column(|c| c.block_input_ops),
            block_output_ops: column(|c| c.block_output_ops),
        }
    }
}

/// One instance of a `--concurrency N` batch.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct InstanceTiming {
    /// Wall clock time of this instance, from its own start to its own exit
    pub time_real: Second,
    /// Exit code of this instance (`None` if killed by a signal)
    pub exit_code: Option<i32>,
}

/// Results from timing a single command
#[derive(Debug, Default, Clone)]
pub struct TimingResult {
    /// Wall clock time
    pub time_real: Second,

    /// Total wall clock time including process spawn overhead (used for scheduling/ETA)
    pub time_total: Second,

    /// Time spent in user mode
    pub time_user: Second,

    /// Time spent in kernel mode
    pub time_system: Second,

    /// Maximum amount of memory used, in bytes
    pub memory_usage_byte: u64,

    /// Energy consumed during execution in Joules (if measured)
    pub energy_joules: Option<f64>,

    /// OS resource counters (None for mocked runs)
    pub counters: Option<ResourceCounters>,

    /// Hardware counter totals (`--counters`, None when not collected)
    pub hw_counters: Option<crate::perf_counters::HwReading>,

    /// Memory bandwidth and NUMA placement (`--membw`, None when not collected)
    pub membw: Option<crate::membw::Run>,

    /// Whether this run timed out
    pub timed_out: bool,

    /// Custom metrics extracted via --output-metric
    pub custom_metrics: BTreeMap<String, f64>,

    /// Metrics of the collectors (`--collector`, `--collector-plugin`), `<collector>.<id>`
    pub collector_metrics: BTreeMap<String, f64>,

    /// Identifier of the region SDK that reported (`--region`)
    pub region_sdk: Option<String>,
    /// Stdout of the run when it is to be validated (`--expect-output-sha256`,
    /// `--validate`); `None` otherwise
    pub stdout: Option<Vec<u8>>,

    /// `--concurrency N` with N > 1: one entry per instance, in spawn order.
    /// The other fields are then the aggregate of the batch (see
    /// `docs/specs/concurrency.md`). Empty for a single instance.
    pub instances: Vec<InstanceTiming>,

    /// Descendants of the command outlived it (`docs/specs/process-tree.md`)
    pub leftover_processes: bool,
}

#[test]
fn test_resource_series_from_counters() {
    let counters = [
        ResourceCounters {
            voluntary_ctx_switches: Some(1),
            minor_faults: Some(10),
            ..Default::default()
        },
        ResourceCounters {
            voluntary_ctx_switches: Some(2),
            minor_faults: Some(0),
            ..Default::default()
        },
    ];
    let series = ResourceSeries::from_counters(&counters);
    assert_eq!(series.voluntary_ctx_switches, Some(vec![1, 2]));
    assert_eq!(series.minor_faults, Some(vec![10, 0]));
    // never reported: unavailable, not zero
    assert_eq!(series.major_faults, None);
}

#[test]
fn test_windows_counters_leave_unsupported_fields_unavailable() {
    let c = ResourceCounters::from_windows(120, 7, 3);
    assert_eq!(c.voluntary_ctx_switches, None);
    assert_eq!(c.involuntary_ctx_switches, None);
    assert_eq!(c.major_faults, None);
    assert_eq!(c.minor_faults, Some(120));
    let series = ResourceSeries::from_counters(&[c, c]);
    let json = serde_json::to_value(&series).unwrap();
    assert_eq!(json["minor_faults"], serde_json::json!([120, 120]));
    assert_eq!(json["block_input_ops"], serde_json::json!([7, 7]));
    assert!(json.get("voluntary_ctx_switches").is_none());
    assert!(json.get("major_faults").is_none());
}
