use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::util::units::Second;

/// OS resource counters of a single run (from `rusage`, Unix only).
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
pub struct ResourceCounters {
    /// Voluntary context switches: the process blocked (I/O, locks, sleep)
    pub voluntary_ctx_switches: u64,
    /// Involuntary context switches: the process was preempted
    pub involuntary_ctx_switches: u64,
    /// Page faults served without I/O
    pub minor_faults: u64,
    /// Page faults that required I/O
    pub major_faults: u64,
    /// Block input operations (real disk reads)
    pub block_input_ops: u64,
    /// Block output operations (real disk writes)
    pub block_output_ops: u64,
}

/// Per-run resource counters of a benchmark, exported with `--resource-usage`.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceSeries {
    pub voluntary_ctx_switches: Vec<u64>,
    pub involuntary_ctx_switches: Vec<u64>,
    pub minor_faults: Vec<u64>,
    pub major_faults: Vec<u64>,
    pub block_input_ops: Vec<u64>,
    pub block_output_ops: Vec<u64>,
}

impl ResourceSeries {
    pub fn from_counters(counters: &[ResourceCounters]) -> Self {
        let column = |f: fn(&ResourceCounters) -> u64| counters.iter().map(f).collect();
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

    /// OS resource counters (None on Windows and for mocked runs)
    pub counters: Option<ResourceCounters>,

    /// Whether this run timed out
    pub timed_out: bool,

    /// Custom metrics extracted via --output-metric
    pub custom_metrics: BTreeMap<String, f64>,

    /// `--concurrency N` with N > 1: one entry per instance, in spawn order.
    /// The other fields are then the aggregate of the batch (see
    /// `docs/specs/concurrency.md`). Empty for a single instance.
    pub instances: Vec<InstanceTiming>,
}

#[test]
fn test_resource_series_from_counters() {
    let counters = [
        ResourceCounters {
            voluntary_ctx_switches: 1,
            minor_faults: 10,
            ..Default::default()
        },
        ResourceCounters {
            voluntary_ctx_switches: 2,
            block_output_ops: 7,
            ..Default::default()
        },
    ];
    let series = ResourceSeries::from_counters(&counters);
    assert_eq!(series.voluntary_ctx_switches, vec![1, 2]);
    assert_eq!(series.minor_faults, vec![10, 0]);
    assert_eq!(series.block_output_ops, vec![0, 7]);
    assert_eq!(series.major_faults, vec![0, 0]);
}
