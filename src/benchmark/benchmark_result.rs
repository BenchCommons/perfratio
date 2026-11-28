use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::metric::Metric;
use crate::util::units::Second;

/// Set of values that will be exported.
// NOTE: `serde` is used for JSON serialization, but not for CSV serialization due to the
// `parameters` map. Update `src/export/csv.rs` with new fields, as appropriate.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkResult {
    /// The full command line of the program that is being benchmarked
    pub command: String,

    /// The full command line of the program that is being benchmarked, possibly including a list of
    /// parameters that were not used in the command line template.
    #[serde(default, skip_serializing)]
    pub command_with_unused_parameters: String,

    /// The average run time
    pub mean: Second,

    /// The standard deviation of all run times. Not available if only one run has been performed
    pub stddev: Option<Second>,

    /// The median run time
    pub median: Second,

    /// 5th, 25th, 75th and 95th percentile of the run times (linear
    /// interpolation between closest ranks)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percentiles: Option<Percentiles>,

    /// Number of warmup runs performed by `--warmup auto`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warmup_runs: Option<u64>,

    /// `--duration`: the sampling budget in seconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_duration_seconds: Option<f64>,

    /// `--duration`: summed run time in seconds of the runs performed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_sampling_seconds: Option<f64>,

    /// `--duration`: number of runs performed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs_done: Option<u64>,

    /// Geometric mean of the run times (omitted if a run took 0 s)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometric_mean: Option<Second>,

    /// Time spent in user mode
    pub user: Second,

    /// Time spent in kernel mode
    pub system: Second,

    /// CPU utilization percentage: (user + system) / mean * 100.0
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_percent: Option<f64>,

    /// Minimum of all measured times
    pub min: Second,

    /// Maximum of all measured times
    pub max: Second,

    /// All run time measurements
    #[serde(skip_serializing_if = "Option::is_none")]
    pub times: Option<Vec<Second>>,

    /// User CPU time measurements for each run
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_times: Option<Vec<Second>>,

    /// System/kernel CPU time measurements for each run
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_times: Option<Vec<Second>>,

    /// Maximum memory usage of the process, in bytes
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_usage_byte: Option<Vec<u64>>,

    /// Average energy consumed in Joules
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mean_energy_joules: Option<f64>,

    /// Average power consumption in Watts (mean_energy_joules / mean_time)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mean_watts: Option<f64>,

    /// All energy measurements in Joules
    #[serde(skip_serializing_if = "Option::is_none")]
    pub energy_joules: Option<Vec<f64>>,

    /// Number of runs that produced an energy sample, set whenever an energy
    /// sampler was active; below the run count means partial coverage
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub energy_samples: Option<usize>,

    /// Idle power in Watts measured by `--energy-baseline`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_watts: Option<f64>,

    /// Average net energy in Joules: per run `max(energy - idle_watts * time, 0)`.
    /// `mean_energy_joules` stays the gross value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mean_net_energy_joules: Option<f64>,

    /// Energy per RAPL domain in Joules, one value per run (`--energy-domains`). Keys are
    /// `package`, `core`, `uncore`, `dram`, `psys`. The domains overlap, so they are not
    /// additive and `energy_joules` is not their sum. Only present when every run has it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub energy_domains_joules: Option<std::collections::BTreeMap<String, Vec<f64>>>,

    /// Estimated energy of the command or its cgroup (`--energy-attribution`, new in 1.43).
    /// A model output labelled `kind: estimated`; `mean_energy_joules` stays the measurement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub energy_attribution: Option<crate::energy::attribution::EnergyAttribution>,
    /// GPU utilization in percent per device (`gpu:<index>`) and run, from `--gpu`. Sampled
    /// when the run ends, so it describes the driver's last sample window (NVML: 1/6 s to
    /// 1 s), not exactly the run. Only present when every run has it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_utilization_percent: Option<std::collections::BTreeMap<String, Vec<f64>>>,

    /// Exit codes of all command invocations
    #[serde(default)]
    pub exit_codes: Vec<Option<i32>>,

    /// Parameter values for this benchmark
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameters: BTreeMap<String, String>,

    /// Benchmark runs omitted due to --omit-failed-runs
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omitted_failed_runs: Vec<OmittedRun>,

    /// Output validation (`--expect-output-sha256`, `--validate`); absent
    /// without those options. See `docs/specs/workload-validation.md`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation: Option<crate::validation::ValidationSummary>,

    /// Measurement quality report: checks and the derived grade. See
    /// `docs/specs/measurement-quality.md`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<crate::quality::QualityReport>,

    /// CPU throttling by the cgroup quota and temperature during the runs.
    /// Absent when neither could be read. See `docs/specs/cgroup-thermal.md`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interference: Option<crate::environment_watch::Interference>,

    /// Zero-based indices of the runs excluded from the statistics by
    /// --discard-outliers
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discarded_outliers: Vec<usize>,

    /// Per-run OS resource counters (context switches, page faults, block
    /// I/O), exported with --resource-usage on Unix
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<super::timing_result::ResourceSeries>,

    /// Per-run hardware counter totals (`--counters`), aligned with `times`;
    /// `null` where a run could not be read. See `docs/specs/hardware-counters.md`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counters: Option<BTreeMap<String, Vec<Option<u64>>>>,

    /// Mean, deviation, median and range of every counter over the runs
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counters_summary: Option<BTreeMap<String, MetricSummary>>,

    /// Runs in which at least one counter was scaled for multiplexing
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counters_multiplexed_runs: Option<usize>,

    /// Lowest `time_running / time_enabled` (0..=1) per counter that was
    /// multiplexed; counters that were always fully scheduled are absent
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counters_coverage: Option<BTreeMap<String, f64>>,

    /// Privilege levels counted: `all` (user + kernel) or `user`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counters_scope: Option<String>,

    /// Effective CPU frequency of each run in Hz (`cycles` / time on CPU),
    /// aligned with `times`; `null` where it could not be computed.
    /// See `docs/specs/effective-frequency.md`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_frequency_hz: Option<Vec<Option<f64>>>,

    /// Mean of `effective_frequency_hz` over runs with at least 5 ms on CPU
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mean_effective_frequency_hz: Option<f64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stddev_effective_frequency_hz: Option<f64>,

    /// Time that divides `cycles`: `task_clock`, `user_time` or `system_time`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_frequency_basis: Option<String>,

    /// Top-down analysis level 1 (`--topdown`): per-run fractions of the issue
    /// slots, aligned with `times`. See `docs/specs/topdown-tma.md`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topdown: Option<crate::topdown::TopdownSeries>,

    /// Mean and spread of the topdown classes and the largest stall class
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topdown_summary: Option<crate::topdown::TopdownSummary>,
    /// Per-run floating-point breakdown (`--simd`), aligned with `times`; see
    /// `docs/specs/simd-breakdown.md`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simd: Option<crate::simd::SimdSeries>,

    /// Mean, deviation, median and range of every SIMD bucket and of the
    /// vectorization ratio
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simd_summary: Option<BTreeMap<String, MetricSummary>>,

    /// Privilege levels counted for `simd`: `all` or `user`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simd_scope: Option<String>,

    /// Buckets that were scaled for multiplexing: runs and lowest coverage
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simd_scaling: Option<BTreeMap<String, crate::simd::SimdScaling>>,

    /// Number of planned runs if the benchmark was interrupted
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs_planned: Option<u64>,

    /// The first (cold) run, reported separately with `--first-run=separate`.
    /// It is not part of any other field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_run: Option<FirstRun>,

    /// Interference diagnostics of the run times (trend, multimodality,
    /// outlier-inflated variance)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<crate::stats::diagnostics::Diagnostics>,

    /// Outlier counts and percentages (time, memory, energy) by
    /// `--outlier-method`; absent with fewer than 4 runs
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outliers: Option<crate::outlier_detection::OutlierReport>,

    /// Which per-run parameter values each run used (`--parameter-sample`,
    /// `--aggregate-parameter-runs`), and the statistics per value
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_run_parameters: Option<PerRunParameterValues>,

    /// `--target-precision`: the target and the precision reached
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precision: Option<PrecisionReached>,
    /// `--target-precision-metrics`: the precision reached by each listed metric
    /// (new in 1.41, #369)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precision_metrics: Option<Vec<MetricPrecision>>,
    /// `--power` / `--target-effect`: minimum detectable effect and runs needed
    /// (new in 1.41, #367)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power: Option<PowerAnalysis>,

    /// The shell this command ran in, when commands use different shells
    /// (`--shell` given per command); "none" without a shell
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,

    /// Derived hardware-counter metrics (IPC, CPI, miss rates), see
    /// `docs/specs/derived-counter-metrics.md`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub efficiency: Option<crate::counters::Efficiency>,

    /// `--subtract`: the baseline whose mean was subtracted from every run
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<Baseline>,

    /// Whether this benchmark timed out (`--timeout <DURATION>`)
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub timed_out: bool,

    /// The configured timeout duration in seconds, if --timeout was used
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<Second>,

    /// Custom metrics extracted via --output-metric, one entry per run
    /// (`null` where an ignored failure left the metric out)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_metrics: Option<BTreeMap<String, Vec<Option<f64>>>>,

    /// Statistical summary of custom metrics
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_metrics_summary: Option<BTreeMap<String, MetricSummary>>,

    /// Metrics of the collectors (`--collector`, `--collector-plugin`) by `<collector>.<id>`:
    /// one entry per run for a plugin (`null` where it had no value), a single entry for the
    /// deterministic Valgrind run. See `docs/specs/collector-plugins.md` (new in 1.44).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collector_metrics: Option<BTreeMap<String, Vec<Option<f64>>>>,

    /// Statistical summary of `collector_metrics` (new in 1.44)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collector_metrics_summary: Option<BTreeMap<String, MetricSummary>>,

    /// `--region`: time inside each instrumented region, see
    /// `docs/specs/region-protocol.md`. `mean`, `times` and `time` fields above
    /// keep measuring the whole process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regions: Option<Vec<RegionResult>>,

    /// `--persistent`: `persistent` when the command ran as one long-lived process
    /// (`docs/specs/persistent-mode.md`). Absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,

    /// `--persistent`: warm-up phase and steady-state statistics of the iterations
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iterations: Option<crate::persistent::IterationsReport>,

    /// `--concurrency N` (only when N > 1): instances started simultaneously per
    /// run. With it present, `times` holds the makespan of each run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<usize>,

    /// `--concurrency`: wall time of every instance in seconds, one row per run
    /// aligned with `times`, instances in spawn order
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_times: Option<Vec<Vec<Second>>>,

    /// `--concurrency`: instances per second, `N / mean makespan`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub throughput: Option<f64>,

    /// `--concurrency`: percentiles of all instance times (`runs x N` samples)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_percentiles: Option<InstancePercentiles>,

    /// No longer written (1.19): package energy is shared by the instances, so
    /// it is not divided per instance (`docs/specs/process-tree.md`). Kept so
    /// that exports of schemaVersion 1.5 to 1.15 still load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub energy_per_instance_joules: Option<f64>,

    /// `--jobs N` (only when N > 1): number of workers that executed at least
    /// one timed run
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workers_used: Option<usize>,

    /// `--jobs N` (only when N > 1, then `true`): runs executed in parallel and
    /// may have disturbed each other, so the times are not comparable with
    /// sequential samples
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contended: Option<bool>,

    /// Scope of the energy values (with `--energy`): `system` (the package
    /// counter covers every process of the machine) or `batch` (with
    /// `--concurrency`, the energy of the whole batch over the makespan)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub energy_scope: Option<String>,

    /// `PERFRATIO_RUN_ID` of every run in `times`. With `--concurrency` it is
    /// the id of instance 0, which `--prepare` and `--conclude` also see.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_ids: Option<Vec<u64>>,

    /// `--concurrency`: `PERFRATIO_RUN_ID` of every instance, one row per run
    /// aligned with `instance_times`; the position in the row is the
    /// `PERFRATIO_WORKER_ID`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_run_ids: Option<Vec<Vec<u64>>>,

    /// Structure of the sample (workers, runs, instances, region repetitions)
    /// with the variance at each level (new in 1.37, #368)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hierarchy: Option<crate::stats::hierarchical::HierarchyReport>,

    /// Processes started by the command (or its prepare/conclude) outlived it
    /// in at least one run; they were killed (`docs/specs/process-tree.md`)
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub leftover_processes: bool,
}

/// Scope of the energy values of a benchmark. RAPL counts the whole package,
/// so the energy of a run is never the energy of one process: it is `system`
/// for a single instance and, with `--concurrency`, the energy of the whole
/// `batch` over its makespan. Package energy is never divided per instance.
pub fn energy_scope(concurrency: Option<usize>) -> &'static str {
    if concurrency.is_some_and(|n| n > 1) {
        "batch"
    } else {
        "system"
    }
}

/// Whether every run reported a region.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RegionStatus {
    /// Every run reported the region.
    Measured,
    /// Some runs did not report it (their entries are `null` and left out of the statistics).
    RegionMissing,
}

/// Time inside one instrumented region (`--region`), in seconds. The wall time
/// of one run is the sum of all start/stop pairs of the region in that run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RegionResult {
    pub name: String,
    pub status: RegionStatus,
    /// Statistics over the runs that reported the region.
    pub mean: Second,
    pub stddev: Option<Second>,
    pub min: Second,
    pub max: Second,
    /// Start/stop pairs per run, `null` where the run did not report the region.
    pub count_per_run: Vec<Option<u64>>,
    /// Region time of every run, `null` where the run did not report the region.
    pub times: Vec<Option<Second>>,
    /// Identifier of the SDK that reported, when it sent one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sdk: Option<String>,
    /// `--region-scope`: CPU time, energy and counters inside the region. Absent
    /// without the flag.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metrics: Vec<RegionMetric>,
}

/// One value measured inside a region (`--region-scope`): CPU time in seconds, energy in
/// joules or a counter, summed over the region's start/stop pairs of a run. The reads
/// happen when the SDK reports the markers, so they lag the markers by the marker
/// latency (`docs/specs/region-scope.md`). The whole-process values stay under
/// `results[]` and have no `scope`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RegionMetric {
    /// `cpu_user`, `cpu_system`, `energy` or a counter name (`instructions`, ...)
    pub name: String,
    /// `s`, `J` or `count`
    pub unit: String,
    /// Always `region`
    pub scope: String,
    pub mean: f64,
    pub stddev: Option<f64>,
    pub min: f64,
    pub max: f64,
    /// Value of every run, `null` where it could not be read.
    pub values: Vec<Option<f64>>,
}

impl RegionMetric {
    /// `None` when no run has a value.
    pub fn from_runs(name: &str, values: &[Option<f64>]) -> Option<Self> {
        let present: Vec<f64> = values.iter().flatten().copied().collect();
        if present.is_empty() {
            return None;
        }
        let mean = crate::stats::basic::mean(&present);
        Some(RegionMetric {
            name: name.to_string(),
            unit: crate::region_scope::unit(name).to_string(),
            scope: crate::region_scope::SCOPE.to_string(),
            mean,
            stddev: (present.len() > 1)
                .then(|| crate::stats::basic::standard_deviation(&present, Some(mean))),
            min: present.iter().copied().fold(f64::INFINITY, f64::min),
            max: present.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            values: values.to_vec(),
        })
    }
}

impl RegionResult {
    /// `None` when no run reported the region.
    pub fn from_runs(
        name: &str,
        times: &[Option<Second>],
        counts: Option<&Vec<Option<f64>>>,
        sdk: Option<String>,
    ) -> Option<Self> {
        let present: Vec<Second> = times.iter().flatten().copied().collect();
        if present.is_empty() {
            return None;
        }
        let mean = crate::stats::basic::mean(&present);
        let stddev = (present.len() > 1)
            .then(|| crate::stats::basic::standard_deviation(&present, Some(mean)));
        Some(RegionResult {
            name: name.to_string(),
            status: if present.len() == times.len() {
                RegionStatus::Measured
            } else {
                RegionStatus::RegionMissing
            },
            mean,
            stddev,
            min: present.iter().copied().fold(f64::INFINITY, f64::min),
            max: present.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            count_per_run: counts
                .map(|c| c.iter().map(|v| v.map(|v| v as u64)).collect())
                .unwrap_or_else(|| vec![None; times.len()]),
            times: times.to_vec(),
            sdk,
            metrics: Vec::new(),
        })
    }
}

/// Percentiles of the per-instance wall times of a `--concurrency` benchmark.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct InstancePercentiles {
    pub p50: Second,
    pub p95: Second,
    pub max: Second,
}

impl InstancePercentiles {
    /// From the (unsorted) instance times of all runs.
    pub fn new(times: &[Second]) -> Option<Self> {
        let mut sorted = times.to_vec();
        sorted.sort_by(f64::total_cmp);
        Some(InstancePercentiles {
            p50: crate::stats::summary::percentile(&sorted, 50.0)?,
            p95: crate::stats::summary::percentile(&sorted, 95.0)?,
            max: *sorted.last()?,
        })
    }
}

/// Instances per second: `concurrency / mean makespan` (`None` for a zero makespan).
pub fn throughput(concurrency: usize, mean_makespan: Second) -> Option<f64> {
    (mean_makespan > 0.0).then(|| concurrency as f64 / mean_makespan)
}

/// Statistical summary of a custom output metric
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetricSummary {
    pub mean: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stddev: Option<f64>,
    pub median: f64,
    pub min: f64,
    pub max: f64,
}

impl MetricSummary {
    pub fn from_values(values: &[f64]) -> Option<Self> {
        if values.is_empty() {
            return None;
        }
        let mean = crate::stats::basic::mean(values);
        let median = crate::stats::basic::median(values);
        let stddev = if values.len() > 1 {
            Some(crate::stats::basic::standard_deviation(values, Some(mean)))
        } else {
            None
        };
        let min = values.iter().copied().fold(f64::INFINITY, f64::min);
        let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        Some(Self {
            mean,
            stddev,
            median,
            min,
            max,
        })
    }
}

/// `--subtract CMD`: the measured baseline.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct Baseline {
    pub command: String,
    /// Mean wall-clock time in seconds
    pub mean: Second,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stddev: Option<Second>,
    /// Mean user and system time in seconds
    pub user: Second,
    pub system: Second,
    pub runs: usize,
    /// Runs of the benchmarked command that were faster than the baseline
    /// (clamped to 0)
    #[serde(default, skip_serializing_if = "is_zero")]
    pub clamped_runs: usize,
    /// Combined standard error of the net mean:
    /// sqrt(cmd_stderr^2 + base_stderr^2) = sqrt(var_cmd / n_cmd + var_base / n_base)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_mean_stderr: Option<Second>,
    /// Mean energy consumed by the baseline in Joules (when --energy is active)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub energy_joules: Option<f64>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// `--power` / `--target-effect`: what the runs made can detect (#367,
/// `docs/specs/statistical-power.md`). The model is a two-sided two-sample
/// t-test between this command and an identical one (same standard deviation
/// and number of runs).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PowerAnalysis {
    /// Level of the test (`--significance-level`)
    pub significance_level: f64,
    /// Probability of detecting the effect (`--power`, default 0.8)
    pub power: f64,
    /// Runs that count (without an excluded first run)
    pub runs: usize,
    /// Smallest change of the mean the runs made detect with `power`, relative to
    /// the mean (0.05 = 5%); `null` without at least 2 runs and a positive mean
    pub minimum_detectable_effect: Option<f64>,
    /// `--target-effect` as a fraction of the mean
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_effect: Option<f64>,
    /// Runs per command that detect `target_effect` with `power`, for the
    /// standard deviation seen
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs_needed: Option<u64>,
    /// Whether `runs >= runs_needed`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub met: Option<bool>,
    /// Whether `--target-effect` decided when the runs stopped
    pub stopping_rule: bool,
}

impl PowerAnalysis {
    /// The line of the human-readable report
    pub fn summary(&self) -> String {
        let mut line = match self.minimum_detectable_effect {
            Some(mde) => format!(
                "detects ±{:.1}% with {:.0}% power (α={}, {} runs, vs an identical command)",
                mde * 100.0,
                self.power * 100.0,
                self.significance_level,
                self.runs
            ),
            None => "minimum detectable effect not available".to_string(),
        };
        if let Some(target) = self.target_effect {
            match self.runs_needed {
                Some(needed) => line.push_str(&format!(
                    "; ±{:.1}% needs {} runs{}",
                    target * 100.0,
                    needed,
                    if self.met == Some(true) {
                        ""
                    } else {
                        " (not reached)"
                    }
                )),
                None => line.push_str(&format!(
                    "; ±{:.1}% runs needed not available",
                    target * 100.0
                )),
            }
        }
        line
    }
}

/// `--target-precision-metrics`: how precisely the mean of one metric is known
/// (#369, `docs/specs/multi-metric-stopping.md`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct MetricPrecision {
    pub metric: crate::options::PrecisionMetric,
    /// The target half-width, relative to the mean (the one of `--target-precision`)
    pub target: f64,
    /// The half-width reached; `None` without 2 samples or a positive mean
    pub reached: Option<f64>,
    /// Samples of the metric that count
    pub runs: usize,
    /// Whether the target was reached
    pub met: bool,
}

/// `--target-precision`: how precisely the mean is known.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct PrecisionReached {
    /// The target half-width of the confidence interval, relative to the mean
    pub target: f64,
    /// The half-width reached (`None` if it could not be computed, e.g. with
    /// fewer than 2 runs)
    pub reached: Option<f64>,
    /// The confidence level of the interval
    pub confidence: f64,
    /// Whether the target was reached
    pub met: bool,
    /// The rule that decides the interval is narrow enough: `fixed_ci` or
    /// `confidence_sequence` (new in 1.22, #322)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopping_rule: Option<crate::stats::sequential::StoppingRule>,
    /// Half-width of that rule's interval, relative to the mean; the same value as
    /// `reached` (new in 1.22, #322)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub achieved_half_width: Option<f64>,
}

/// Per-run parameter values, aligned with `times`, and statistics per value.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct PerRunParameterValues {
    /// For each variable, the value of every run (aligned with `times`)
    pub values: BTreeMap<String, Vec<String>>,
    /// For each variable and value: statistics of the runs that used it
    pub stats: BTreeMap<String, BTreeMap<String, ValueStats>>,
}

/// Run time statistics of the runs that used one parameter value.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct ValueStats {
    pub mean: Second,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stddev: Option<Second>,
    pub runs: usize,
}

impl PerRunParameterValues {
    /// From the values of each run (as (name, value) pairs) and the run times.
    pub fn new(per_run: &[Vec<(String, String)>], times: &[Second]) -> Option<Self> {
        if per_run.is_empty() || per_run.iter().all(Vec::is_empty) || per_run.len() != times.len() {
            return None;
        }
        let mut values: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut by_value: BTreeMap<String, BTreeMap<String, Vec<Second>>> = BTreeMap::new();
        for (run, &time) in per_run.iter().zip(times) {
            for (name, value) in run {
                values.entry(name.clone()).or_default().push(value.clone());
                by_value
                    .entry(name.clone())
                    .or_default()
                    .entry(value.clone())
                    .or_default()
                    .push(time);
            }
        }
        let stats = by_value
            .into_iter()
            .map(|(name, groups)| {
                let groups = groups
                    .into_iter()
                    .map(|(value, times)| {
                        let mean = crate::stats::basic::mean(&times);
                        let stddev = (times.len() > 1)
                            .then(|| crate::stats::basic::standard_deviation(&times, Some(mean)));
                        (
                            value,
                            ValueStats {
                                mean,
                                stddev,
                                runs: times.len(),
                            },
                        )
                    })
                    .collect();
                (name, groups)
            })
            .collect();
        Some(PerRunParameterValues { values, stats })
    }
}

/// The first (cold) run of a benchmark (`--first-run=separate`).
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct FirstRun {
    /// Wall-clock time in seconds
    pub time: Second,
    /// User and system time in seconds (not available for a warmup run)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<Second>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<Second>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_usage_byte: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub energy_joules: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// True if this was the first warmup run (`--warmup` was used)
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub warmup: bool,
}

/// Percentiles of the run times, in seconds.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Percentiles {
    pub p05: Second,
    pub p25: Second,
    pub p75: Second,
    pub p95: Second,
}

/// Information about a benchmark run that was omitted due to failure.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OmittedRun {
    /// Zero-based index of the benchmark run
    pub index: usize,
    /// The exit code of the failed command, or None if terminated by signal
    pub exit_code: Option<i32>,
}

impl BenchmarkResult {
    /// Returns true if any run had a non-zero or missing exit code.
    pub fn has_failure(&self) -> bool {
        self.exit_codes.iter().any(|code| *code != Some(0))
    }

    /// The region selected by `--metric region:<name>`.
    fn region_of(&self, metric: Metric) -> Option<&RegionResult> {
        let name = metric.region_name()?;
        self.regions.as_ref()?.iter().find(|r| r.name == name)
    }

    /// A note when runs failed output validation (`--expect-output-sha256`,
    /// `--validate`), for the delta table and `--compare`: such a result may
    /// have been fast because it computed the wrong thing.
    pub fn validation_note(&self) -> Option<String> {
        let v = self.validation.as_ref().filter(|v| v.runs_invalid > 0)?;
        Some(format!(
            "'{}': {} of {} validated runs failed output validation ({} omitted); the \
             comparison may not be meaningful.",
            self.command, v.runs_invalid, v.runs_checked, v.runs_omitted
        ))
    }

    /// Extracts samples corresponding to the specified metric.
    pub fn primary_samples(&self, metric: Metric) -> Option<Vec<f64>> {
        match metric {
            Metric::Wall => self.times.clone(),
            Metric::User => self.user_times.clone(),
            Metric::System => self.system_times.clone(),
            Metric::Cpu => {
                if let (Some(u), Some(s)) = (&self.user_times, &self.system_times) {
                    Some(u.iter().zip(s.iter()).map(|(&a, &b)| a + b).collect())
                } else {
                    None
                }
            }
            Metric::Energy => self.energy_joules.clone(),
            Metric::Memory => self
                .memory_usage_byte
                .as_ref()
                .map(|m| m.iter().map(|&b| b as f64).collect()),
            Metric::Instructions | Metric::Cycles => {
                let samples: Vec<f64> = self
                    .counters
                    .as_ref()?
                    .get(metric.name())?
                    .iter()
                    .flatten()
                    .map(|&v| v as f64)
                    .collect();
                (!samples.is_empty()).then_some(samples)
            }
            Metric::Custom(_) if metric.is_region() => {
                let samples: Vec<f64> = self
                    .region_of(metric)?
                    .times
                    .iter()
                    .flatten()
                    .copied()
                    .collect();
                (!samples.is_empty()).then_some(samples)
            }
            Metric::Custom(name) => {
                let samples: Vec<f64> = self
                    .custom_metrics
                    .as_ref()?
                    .get(name)?
                    .iter()
                    .flatten()
                    .copied()
                    .collect();
                (!samples.is_empty()).then_some(samples)
            }
        }
    }

    /// Computes the mean value for the specified metric.
    ///
    /// Returns `None` when the result carries no data for the metric (e.g. no
    /// energy reading), so that such a result is never ranked on a fake 0.
    pub fn primary_mean(&self, metric: Metric) -> Option<f64> {
        match metric {
            Metric::Wall => Some(self.mean),
            Metric::User => Some(self.user),
            Metric::System => Some(self.system),
            Metric::Cpu => Some(self.user + self.system),
            Metric::Energy => self.mean_energy_joules,
            Metric::Memory => {
                let mem = self.memory_usage_byte.as_ref()?;
                if mem.is_empty() {
                    return None;
                }
                let sum: u64 = mem.iter().sum();
                Some(sum as f64 / mem.len() as f64)
            }
            Metric::Instructions | Metric::Cycles => self
                .counters_summary
                .as_ref()?
                .get(metric.name())
                .map(|s| s.mean),
            Metric::Custom(_) if metric.is_region() => self.region_of(metric).map(|r| r.mean),
            Metric::Custom(name) => self
                .custom_metrics_summary
                .as_ref()?
                .get(name)
                .map(|s| s.mean),
        }
    }

    /// The central value of the metric under the installed `--estimator`: the
    /// mean by default (exactly `primary_mean`), the median or a percentile of
    /// the per-run samples otherwise. `None` when there is no data for the
    /// metric, or no per-run samples to take a median or percentile of
    /// (e.g. imported results without `times`); never a made-up value.
    pub fn primary_center(&self, metric: Metric) -> Option<f64> {
        self.primary_center_with(metric, crate::stats::settings::current().estimator)
    }

    /// [`Self::primary_center`] with an explicit estimator.
    pub fn primary_center_with(
        &self,
        metric: Metric,
        estimator: crate::stats::settings::Estimator,
    ) -> Option<f64> {
        use crate::stats::settings::Estimator;
        if estimator.is_mean() {
            return self.primary_mean(metric);
        }
        self.primary_mean(metric)?;
        match self.primary_samples(metric) {
            Some(samples) if !samples.is_empty() => Some(estimator.estimate(&samples)),
            _ if metric == Metric::Wall && estimator == Estimator::Median => Some(self.median),
            _ => None,
        }
    }

    /// Computes the standard deviation for the specified metric.
    pub fn primary_stddev(&self, metric: Metric) -> Option<f64> {
        match metric {
            Metric::Wall => self.stddev,
            Metric::User => self.user_times.as_ref().and_then(|times| {
                if times.len() > 1 {
                    Some(crate::stats::basic::standard_deviation(
                        times,
                        Some(self.user),
                    ))
                } else {
                    None
                }
            }),
            Metric::System => self.system_times.as_ref().and_then(|times| {
                if times.len() > 1 {
                    Some(crate::stats::basic::standard_deviation(
                        times,
                        Some(self.system),
                    ))
                } else {
                    None
                }
            }),
            Metric::Cpu => self.primary_samples(Metric::Cpu).and_then(|samples| {
                if samples.len() > 1 {
                    Some(crate::stats::basic::standard_deviation(
                        &samples,
                        Some(self.user + self.system),
                    ))
                } else {
                    None
                }
            }),
            Metric::Energy => self.energy_joules.as_ref().and_then(|samples| {
                if samples.len() > 1 {
                    Some(crate::stats::basic::standard_deviation(
                        samples,
                        self.mean_energy_joules,
                    ))
                } else {
                    None
                }
            }),
            Metric::Memory => self.primary_samples(Metric::Memory).and_then(|samples| {
                if samples.len() > 1 {
                    let mean = self.primary_mean(Metric::Memory)?;
                    Some(crate::stats::basic::standard_deviation(
                        &samples,
                        Some(mean),
                    ))
                } else {
                    None
                }
            }),
            Metric::Instructions | Metric::Cycles => self
                .counters_summary
                .as_ref()?
                .get(metric.name())
                .and_then(|s| s.stddev),
            Metric::Custom(_) if metric.is_region() => self.region_of(metric)?.stddev,
            Metric::Custom(name) => self
                .custom_metrics_summary
                .as_ref()?
                .get(name)
                .and_then(|s| s.stddev),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result_with_exit_codes(codes: Vec<Option<i32>>) -> BenchmarkResult {
        BenchmarkResult {
            exit_codes: codes,
            ..Default::default()
        }
    }

    #[test]
    fn instance_percentiles_and_throughput() {
        let p = InstancePercentiles::new(&[0.4, 0.1, 0.3, 0.2]).unwrap();
        assert_eq!(p.max, 0.4);
        assert!((p.p50 - 0.25).abs() < 1e-12);
        assert!((p.p95 - 0.385).abs() < 1e-12);
        assert_eq!(InstancePercentiles::new(&[]), None);
        assert_eq!(throughput(8, 0.5), Some(16.0));
        assert_eq!(throughput(8, 0.0), None);
    }

    #[test]
    fn test_has_failure_all_success() {
        let r = result_with_exit_codes(vec![Some(0), Some(0), Some(0)]);
        assert!(!r.has_failure());
    }

    #[test]
    fn test_has_failure_with_nonzero() {
        let r = result_with_exit_codes(vec![Some(0), Some(1), Some(0)]);
        assert!(r.has_failure());
    }

    #[test]
    fn test_has_failure_with_signal_kill() {
        let r = result_with_exit_codes(vec![Some(0), None]);
        assert!(r.has_failure());
    }

    #[test]
    fn test_has_failure_empty() {
        let r = result_with_exit_codes(vec![]);
        assert!(!r.has_failure());
    }

    #[test]
    fn test_omitted_failed_runs_serde() {
        let r = BenchmarkResult {
            command: "cmd".into(),
            times: Some(vec![0.1, 0.2]),
            exit_codes: vec![Some(0), Some(0)],
            omitted_failed_runs: vec![OmittedRun {
                index: 1,
                exit_code: Some(42),
            }],
            ..Default::default()
        };
        assert!(!r.has_failure());

        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"omitted_failed_runs\":[{\"index\":1,\"exit_code\":42}]"));

        let deserialized: BenchmarkResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, r);
        assert!(!deserialized.has_failure());
    }

    #[test]
    fn test_omitted_failed_runs_empty_skips_serializing() {
        let r = BenchmarkResult {
            command: "cmd".into(),
            exit_codes: vec![Some(0)],
            ..Default::default()
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("omitted_failed_runs"));
    }
}

#[test]
fn per_run_parameter_values_and_stats() {
    let run = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(n, v)| (n.to_string(), v.to_string()))
            .collect()
    };
    let per_run = [
        run(&[("f", "a"), ("t", "1")]),
        run(&[("f", "b"), ("t", "1")]),
        run(&[("f", "a"), ("t", "2")]),
    ];
    let values = PerRunParameterValues::new(&per_run, &[1.0, 2.0, 3.0]).unwrap();
    assert_eq!(values.values["f"], ["a", "b", "a"]);
    assert_eq!(values.values["t"], ["1", "1", "2"]);
    let a = &values.stats["f"]["a"];
    assert_eq!((a.mean, a.runs), (2.0, 2));
    assert!((a.stddev.unwrap() - std::f64::consts::SQRT_2).abs() < 1e-12);
    assert_eq!(values.stats["f"]["b"].stddev, None);

    // Nothing without per-run parameters, or if misaligned
    assert_eq!(
        PerRunParameterValues::new(&[vec![], vec![]], &[1.0, 2.0]),
        None
    );
    assert_eq!(PerRunParameterValues::new(&per_run, &[1.0]), None);
}

#[test]
fn test_primary_samples_and_metrics() {
    let result = BenchmarkResult {
        mean: 10.0,
        stddev: Some(1.0),
        user: 6.0,
        system: 4.0,
        times: Some(vec![9.0, 11.0]),
        user_times: Some(vec![5.5, 6.5]),
        system_times: Some(vec![3.5, 4.5]),
        memory_usage_byte: Some(vec![1000, 2000]),
        mean_energy_joules: Some(50.0),
        energy_joules: Some(vec![45.0, 55.0]),
        ..Default::default()
    };

    // Primary samples
    assert_eq!(result.primary_samples(Metric::Wall), Some(vec![9.0, 11.0]));
    assert_eq!(result.primary_samples(Metric::User), Some(vec![5.5, 6.5]));
    assert_eq!(result.primary_samples(Metric::System), Some(vec![3.5, 4.5]));
    assert_eq!(result.primary_samples(Metric::Cpu), Some(vec![9.0, 11.0]));
    assert_eq!(
        result.primary_samples(Metric::Energy),
        Some(vec![45.0, 55.0])
    );
    assert_eq!(
        result.primary_samples(Metric::Memory),
        Some(vec![1000.0, 2000.0])
    );

    // Primary mean
    assert_eq!(result.primary_mean(Metric::Wall), Some(10.0));
    assert_eq!(result.primary_mean(Metric::User), Some(6.0));
    assert_eq!(result.primary_mean(Metric::System), Some(4.0));
    assert_eq!(result.primary_mean(Metric::Cpu), Some(10.0));
    assert_eq!(result.primary_mean(Metric::Energy), Some(50.0));
    assert_eq!(result.primary_mean(Metric::Memory), Some(1500.0));

    // Primary stddev
    assert!(result.primary_stddev(Metric::Wall).is_some());
    assert!(result.primary_stddev(Metric::User).is_some());
    assert!(result.primary_stddev(Metric::System).is_some());
    assert!(result.primary_stddev(Metric::Cpu).is_some());
    assert!(result.primary_stddev(Metric::Energy).is_some());
    assert!(result.primary_stddev(Metric::Memory).is_some());
}

#[test]
fn test_energy_scope_is_never_per_instance() {
    assert_eq!(energy_scope(None), "system");
    assert_eq!(energy_scope(Some(1)), "system");
    assert_eq!(energy_scope(Some(4)), "batch");
}
