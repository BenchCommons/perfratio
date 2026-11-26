//! Hardware performance counters per benchmark run (`--counters`, #49).
//!
//! Linux reads the counters with `perf_event_open`. One file descriptor per
//! event (no `PERF_FORMAT_GROUP`, which does not work with `inherit`),
//! opened per run on the thread that spawns the command, disabled and with
//! `enable_on_exec` + `inherit`. Because the spawning thread never calls
//! `exec`, it is never counted; the child's inherited copies switch on at its
//! `exec` and their totals are added to the parent's fd when the child exits.
//! Multiplexing is corrected with [`crate::pmu_scaling`], availability is
//! decided with [`crate::pmu`], derived rates come from [`crate::counters`].
//!
//! macOS and Windows have no usable counters yet: the result is reported as
//! `unavailable` with a reason. On macOS the private `kperf`/`kpc` framework
//! rejects every call of an unprivileged process (`kpc_force_all_ctrs_set`,
//! `kpc_set_counting` and `kpc_get_thread_counters` return -1 without root,
//! tested on Apple M4) and its counters are per thread, not inherited.
//!
//! Everything except the Linux syscall wrappers is pure and tested on all
//! platforms.

use crate::counters::{CounterSample, Efficiency};
use crate::frequency::{self, Basis};
use crate::pmu::{self, PmuStatus};
use crate::pmu_scaling::{multiplex_warning, ScaledCounter};
use crate::stats::basic;
use std::collections::BTreeMap;

/// Names accepted by `--counters`, in the order of the default list.
pub const DEFAULT_COUNTERS: &str =
    "instructions,cycles,cache-references,cache-misses,branches,branch-misses";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CounterKind {
    Instructions,
    Cycles,
    CacheReferences,
    CacheMisses,
    Branches,
    BranchMisses,
    /// Software events (`PERF_TYPE_SOFTWARE`, #220): no PMU needed
    CpuMigrations,
    ContextSwitches,
    PageFaults,
    /// Nanoseconds the task tree was on a CPU. Also the divisor of the
    /// effective frequency (#222)
    TaskClock,
}

impl CounterKind {
    /// The hardware events, which are also the default list.
    pub const ALL: [CounterKind; 6] = [
        CounterKind::Instructions,
        CounterKind::Cycles,
        CounterKind::CacheReferences,
        CounterKind::CacheMisses,
        CounterKind::Branches,
        CounterKind::BranchMisses,
    ];

    /// The software events (`--counters=sw` selects the first two).
    pub const SOFTWARE: [CounterKind; 4] = [
        CounterKind::CpuMigrations,
        CounterKind::ContextSwitches,
        CounterKind::PageFaults,
        CounterKind::TaskClock,
    ];

    /// Software events need no PMU, but the scheduler events are counted from
    /// kernel context, so they cannot be scoped to user space.
    pub fn is_software(self) -> bool {
        Self::SOFTWARE.contains(&self)
    }

    pub fn name(self) -> &'static str {
        match self {
            CounterKind::Instructions => "instructions",
            CounterKind::Cycles => "cycles",
            CounterKind::CacheReferences => "cache-references",
            CounterKind::CacheMisses => "cache-misses",
            CounterKind::Branches => "branches",
            CounterKind::BranchMisses => "branch-misses",
            CounterKind::CpuMigrations => "cpu-migrations",
            CounterKind::ContextSwitches => "context-switches",
            CounterKind::PageFaults => "page-faults",
            CounterKind::TaskClock => "task-clock",
        }
    }

    /// Short name for the terminal line.
    fn short(self) -> &'static str {
        match self {
            CounterKind::Instructions => "instr",
            CounterKind::Cycles => "cycles",
            CounterKind::CacheReferences => "cache-ref",
            CounterKind::CacheMisses => "cache-miss",
            CounterKind::Branches => "branches",
            CounterKind::BranchMisses => "branch-miss",
            CounterKind::CpuMigrations => "cpu-migr",
            CounterKind::ContextSwitches => "ctx-sw",
            CounterKind::PageFaults => "page-faults",
            CounterKind::TaskClock => "task-clock",
        }
    }

    /// `PERF_TYPE_HARDWARE` (0) or `PERF_TYPE_SOFTWARE` (1)
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn perf_type(self) -> u32 {
        u32::from(self.is_software())
    }

    /// `PERF_COUNT_HW_*` or `PERF_COUNT_SW_*` value, depending on [`Self::perf_type`]
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn hw_config(self) -> u64 {
        match self {
            CounterKind::TaskClock => 1,
            CounterKind::PageFaults => 2,
            CounterKind::ContextSwitches => 3,
            CounterKind::CpuMigrations => 4,
            CounterKind::Cycles => 0,
            CounterKind::Instructions => 1,
            CounterKind::CacheReferences => 2,
            CounterKind::CacheMisses => 3,
            CounterKind::Branches => 4,
            CounterKind::BranchMisses => 5,
        }
    }
}

/// Parses a comma-separated `--counters` list. Duplicates are dropped, the
/// order is kept. Names are lowercase. Shorthands: `default` (the hardware
/// list), `sw` (`cpu-migrations,context-switches`) and `frequency` (`cycles`;
/// `task-clock` is added automatically).
pub fn parse_list(list: &str) -> Result<Vec<CounterKind>, String> {
    let mut kinds = Vec::new();
    let mut push = |kind: CounterKind| {
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    };
    for name in list.split(',') {
        match name.trim() {
            "default" => CounterKind::ALL.into_iter().for_each(&mut push),
            "sw" => {
                push(CounterKind::CpuMigrations);
                push(CounterKind::ContextSwitches);
            }
            "frequency" => push(CounterKind::Cycles),
            name => {
                let kind = CounterKind::ALL
                    .into_iter()
                    .chain(CounterKind::SOFTWARE)
                    .find(|k| k.name() == name)
                    .ok_or_else(|| {
                        let valid: Vec<&str> = CounterKind::ALL
                            .iter()
                            .chain(CounterKind::SOFTWARE.iter())
                            .map(|k| k.name())
                            .collect();
                        format!(
                            "unknown counter '{name}' for '--counters' (valid: {}; shorthands: \
                             default, sw, frequency)",
                            valid.join(", ")
                        )
                    })?;
                push(kind);
            }
        }
    }
    Ok(kinds)
}

/// Which privilege levels are counted. The default is `All` (user + kernel,
/// like `perf stat`), see decision D1 in `pmu-extensions-common.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// User and kernel space. Needs `perf_event_paranoid <= 1` or `CAP_PERFMON`.
    All,
    /// User space only (`exclude_kernel`)
    User,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::All => "all",
            Scope::User => "user",
        }
    }
}

/// Why no counter could be used (open enum in the JSON export).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnavailableReason {
    UnsupportedPlatform,
    PerfParanoid,
    NoPmu,
    /// Software scheduler events need kernel-scope access (`perf_event_paranoid <= 1`)
    KernelScopeDenied,
    Other,
}

impl UnavailableReason {
    pub fn as_str(self) -> &'static str {
        match self {
            UnavailableReason::UnsupportedPlatform => "unsupported_platform",
            UnavailableReason::PerfParanoid => "perf_paranoid",
            UnavailableReason::NoPmu => "no_pmu",
            UnavailableReason::KernelScopeDenied => "kernel_scope_denied",
            UnavailableReason::Other => "other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    Measured,
    /// Some requested events cannot be opened
    Partial(Vec<CounterKind>),
    Unavailable(UnavailableReason),
}

/// The resolved `--counters` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CounterConfig {
    pub requested: Vec<CounterKind>,
    /// Events that are really collected, a subset of `requested`
    pub active: Vec<CounterKind>,
    pub scope: Scope,
    pub availability: Availability,
    /// Why events are missing when `availability` is `Partial`
    pub partial_reason: Option<UnavailableReason>,
    /// `task-clock` that was added only to compute the effective frequency:
    /// collected, but not exported or printed as a counter
    pub hidden: Vec<CounterKind>,
    /// `--debug-mode`: synthetic values, nothing is opened
    pub mock: bool,
}

impl CounterConfig {
    pub fn is_active(&self) -> bool {
        !self.active.is_empty()
    }
}

/// Raw counts of one run, one entry per active event (`None`: not readable).
pub type HwReading = Vec<Option<ScaledCounter>>;

const EPERM: i32 = 1;
const EACCES: i32 = 13;

fn is_denied(errno: i32) -> bool {
    matches!(errno, EPERM | EACCES)
}

/// Why one event could not be opened.
fn reason_for(kind: CounterKind, errno: i32, paranoid: Option<i32>) -> UnavailableReason {
    if kind.is_software() && is_denied(errno) {
        return UnavailableReason::KernelScopeDenied;
    }
    match pmu::decide(paranoid, Some(pmu::Probe::Failed(errno))) {
        PmuStatus::Denied { .. } => UnavailableReason::PerfParanoid,
        PmuStatus::Unsupported => UnavailableReason::NoPmu,
        _ => UnavailableReason::Other,
    }
}

/// Adds `task-clock` when `cycles` is requested (the divisor of the effective
/// frequency, #222). Returns the events to open and the one that stays hidden.
fn with_frequency_divisor(mut requested: Vec<CounterKind>) -> (Vec<CounterKind>, Vec<CounterKind>) {
    let mut hidden = Vec::new();
    if requested.contains(&CounterKind::Cycles) && !requested.contains(&CounterKind::TaskClock) {
        requested.push(CounterKind::TaskClock);
        hidden.push(CounterKind::TaskClock);
    }
    (requested, hidden)
}

/// Pure planning step: opens nothing itself, `probe` tries one event.
/// `hidden` events are collected but a failure to open them is silent (the
/// frequency is then unavailable). Software events are always probed with
/// scope `All`: the kernel does not count them with `exclude_kernel`.
/// Returns the config and the warnings to print once.
pub fn plan(
    requested: Vec<CounterKind>,
    hidden: Vec<CounterKind>,
    paranoid: Option<i32>,
    mut probe: impl FnMut(CounterKind, Scope) -> Result<(), i32>,
) -> (CounterConfig, Vec<String>) {
    let mut warnings = Vec::new();
    let mut scope = Scope::All;
    // Kernel-side counting is denied at the default paranoid level 2:
    // fall back to user space and say so (never report user-only as `all`).
    if let Some(&first) = requested.iter().find(|k| !k.is_software()) {
        if matches!(probe(first, Scope::All), Err(EPERM | EACCES)) {
            scope = Scope::User;
            warnings.push(format!(
                "counting user-space events only (kernel counting is denied, perf_event_paranoid={}). \
                 Run `sudo sysctl kernel.perf_event_paranoid=1` or grant CAP_PERFMON to count kernel \
                 instructions too.",
                paranoid.map_or("unknown".to_string(), |p| p.to_string())
            ));
        }
    }
    let mut active = Vec::new();
    let mut failed: Vec<(CounterKind, i32)> = Vec::new();
    for &kind in &requested {
        let event_scope = if kind.is_software() {
            Scope::All
        } else {
            scope
        };
        match probe(kind, event_scope) {
            Ok(()) => active.push(kind),
            Err(errno) => {
                if !hidden.contains(&kind) {
                    failed.push((kind, errno));
                }
            }
        }
    }
    let mut partial_reason = None;
    let availability = if failed.is_empty() {
        Availability::Measured
    } else {
        let (first_kind, first_errno) = failed[0];
        let reason = reason_for(first_kind, first_errno, paranoid);
        let names: Vec<&str> = failed.iter().map(|f| f.0.name()).collect();
        if active.iter().all(|k| hidden.contains(k)) {
            let scheduler_only = failed.iter().all(|f| f.0.is_software());
            warnings.push(if scheduler_only {
                format!(
                    "software counters ({}) need kernel-scope access, which is denied \
                     (perf_event_paranoid={}); continuing without them. Run `sudo sysctl \
                     kernel.perf_event_paranoid=1` or grant CAP_PERFMON.",
                    names.join(", "),
                    paranoid.map_or("unknown".to_string(), |p| p.to_string())
                )
            } else {
                let status = pmu::decide(paranoid, Some(pmu::Probe::Failed(first_errno)));
                format!(
                    "hardware counters are not available, continuing without them: {}",
                    pmu::diagnostic(&status, "perfratio")
                        .unwrap_or_else(|| pmu::summary(&status))
                        .trim_end()
                )
            });
            active.clear();
            Availability::Unavailable(reason)
        } else {
            warnings.push(format!(
                "these counters are not available and are skipped: {}",
                names.join(", ")
            ));
            partial_reason = Some(reason);
            Availability::Partial(failed.iter().map(|f| f.0).collect())
        }
    };
    (
        CounterConfig {
            requested,
            active,
            scope,
            availability,
            partial_reason,
            hidden,
            mock: false,
        },
        warnings,
    )
}

/// Resolves a `--counters` request against the machine. Returns the config and
/// the warnings to print (once, before the first benchmark).
pub fn resolve(requested: Vec<CounterKind>, debug_mode: bool) -> (CounterConfig, Vec<String>) {
    let (requested, hidden) = with_frequency_divisor(requested);
    if debug_mode {
        return (
            CounterConfig {
                active: requested.clone(),
                requested,
                scope: Scope::All,
                availability: Availability::Measured,
                partial_reason: None,
                hidden,
                mock: true,
            },
            vec![],
        );
    }
    resolve_native(requested, hidden)
}

#[cfg(target_os = "linux")]
fn resolve_native(
    requested: Vec<CounterKind>,
    hidden: Vec<CounterKind>,
) -> (CounterConfig, Vec<String>) {
    let paranoid = std::fs::read_to_string("/proc/sys/kernel/perf_event_paranoid")
        .ok()
        .and_then(|c| pmu::parse_paranoid(&c));
    plan(requested, hidden, paranoid, linux::probe)
}

#[cfg(not(target_os = "linux"))]
fn resolve_native(
    requested: Vec<CounterKind>,
    hidden: Vec<CounterKind>,
) -> (CounterConfig, Vec<String>) {
    let why = if cfg!(target_os = "macos") {
        "on macOS the kperf framework needs root and cannot follow child processes"
    } else {
        "no supported counter API on this platform"
    };
    (
        CounterConfig {
            requested,
            active: vec![],
            scope: Scope::All,
            availability: Availability::Unavailable(UnavailableReason::UnsupportedPlatform),
            partial_reason: None,
            hidden,
            mock: false,
        },
        vec![format!(
            "'--counters' is only supported on Linux ({why}); continuing without hardware counters."
        )],
    )
}

/// Deterministic values for `--debug-mode`, derived from the simulated time.
pub fn mock_reading(config: &CounterConfig, time_real: f64) -> HwReading {
    config
        .active
        .iter()
        .map(|&kind| {
            let per_second = match kind {
                CounterKind::Instructions => 2_000_000_000.0,
                CounterKind::Cycles => 1_000_000_000.0,
                CounterKind::CacheReferences => 10_000_000.0,
                CounterKind::CacheMisses => 1_000_000.0,
                CounterKind::Branches => 400_000_000.0,
                CounterKind::BranchMisses => 4_000_000.0,
                CounterKind::CpuMigrations => 0.0,
                CounterKind::ContextSwitches => 100.0,
                CounterKind::PageFaults => 1_000.0,
                // nanoseconds: with 1e9 cycles/s the mock clock runs at 1 GHz
                CounterKind::TaskClock => 1_000_000_000.0,
            };
            let value = (per_second * time_real) as u64;
            Some(ScaledCounter {
                value,
                raw: value,
                time_enabled: 1,
                time_running: 1,
            })
        })
        .collect()
}

/// Median per event of the readings of calibration runs.
pub fn median_overhead(samples: &[HwReading], events: usize) -> Vec<u64> {
    (0..events)
        .map(|i| {
            let mut values: Vec<u64> = samples
                .iter()
                .filter_map(|r| r.get(i).copied().flatten().map(|c| c.value))
                .collect();
            values.sort_unstable();
            values.get(values.len() / 2).copied().unwrap_or(0)
        })
        .collect()
}

/// Subtracts the calibrated overhead, clamped at 0.
pub fn subtract_overhead(reading: &mut HwReading, overhead: &[u64]) {
    for (counter, &o) in reading.iter_mut().zip(overhead) {
        if let Some(c) = counter {
            c.value = c.value.saturating_sub(o);
        }
    }
}

/// Effective frequency of a benchmark (#222).
#[derive(Debug, Clone, PartialEq)]
pub struct FrequencyExport {
    /// Hz per run, aligned with the times; `None` where not computable
    pub series: Vec<Option<f64>>,
    /// Over the runs with at least 5 ms on CPU
    pub mean: Option<f64>,
    pub stddev: Option<f64>,
    pub basis: Basis,
}

/// Per-run data of a benchmark, ready to export and to print.
#[derive(Debug, Clone, PartialEq)]
pub struct CounterExport {
    pub counters: BTreeMap<String, Vec<Option<u64>>>,
    pub summary: BTreeMap<String, crate::benchmark::benchmark_result::MetricSummary>,
    pub multiplexed_runs: usize,
    pub efficiency: Option<Efficiency>,
    pub frequency: Option<FrequencyExport>,
    pub line: Option<String>,
    pub warning: Option<String>,
    /// Scheduler jitter and frequency warnings, hidden by `--suppress-outlier-warnings`
    pub advisories: Vec<String>,
}

/// What the counters of a benchmark are compared with.
#[derive(Debug, Clone, Copy, Default)]
pub struct RunContext<'a> {
    pub real_seconds: &'a [f64],
    pub user_seconds: &'a [f64],
    pub system_seconds: &'a [f64],
    /// `--affinity` names exactly one CPU: migrations are impossible
    pub single_cpu: bool,
    /// `cpuinfo_max_freq` of the CPUs in use, if known
    pub max_frequency_hz: Option<f64>,
}

/// Minimum number of runs for the scheduler jitter warning.
const JITTER_MIN_RUNS: usize = 5;

/// Scheduler jitter warning (#220). `migrations` and `times` are aligned,
/// `None` is a run whose counter could not be read (skipped).
///
/// Fires when at least 10% of the runs had a migration, at least two runs had
/// none, and the migrated runs are slower by more than the standard deviation
/// of the quiet runs. If every run migrated there is nothing to compare, and
/// the count is reported alone.
pub fn jitter_warning(
    migrations: &[Option<u64>],
    times: &[f64],
    single_cpu: bool,
) -> Option<String> {
    if single_cpu {
        return None;
    }
    let pairs: Vec<(u64, f64)> = migrations
        .iter()
        .zip(times)
        .filter_map(|(m, &t)| m.map(|m| (m, t)))
        .collect();
    let total = pairs.len();
    if total < JITTER_MIN_RUNS {
        return None;
    }
    let moved: Vec<f64> = pairs.iter().filter(|p| p.0 > 0).map(|p| p.1).collect();
    let quiet: Vec<f64> = pairs.iter().filter(|p| p.0 == 0).map(|p| p.1).collect();
    if moved.is_empty() || (moved.len() as f64) / (total as f64) < 0.10 {
        return None;
    }
    let count: u64 = pairs.iter().map(|p| p.0).sum();
    const TIP: &str =
        "Tip: pin the command with '--affinity <CPUS>' to avoid scheduler migrations.";
    if quiet.is_empty() {
        return Some(format!(
            "{count} CPU migrations in all {total} runs.\n{TIP}"
        ));
    }
    if quiet.len() < 2 {
        return None;
    }
    let (mean_moved, mean_quiet) = (basic::mean(&moved), basic::mean(&quiet));
    let sd_quiet = basic::standard_deviation(&quiet, Some(mean_quiet));
    if mean_moved - mean_quiet <= sd_quiet || mean_quiet <= 0.0 || sd_quiet.is_nan() {
        return None;
    }
    Some(format!(
        "{count} CPU migrations in {} of {total} runs; those runs were {:.0}% slower.\n{TIP}",
        moved.len(),
        (mean_moved / mean_quiet - 1.0) * 100.0
    ))
}

/// `1.2` under 10, otherwise rounded: counts of software events per run.
fn format_event_count(mean: f64) -> String {
    if mean < 10.0 {
        format!("{mean:.1}")
    } else {
        format!("{mean:.0}")
    }
}

/// Builds the export of all runs. `runs` has one entry per run (aligned with
/// the times in `ctx`); `None` is a run whose counters could not be read.
pub fn export(
    config: &CounterConfig,
    runs: &[Option<HwReading>],
    ctx: &RunContext,
) -> Option<CounterExport> {
    if !config.is_active() || runs.is_empty() {
        return None;
    }
    let cell =
        |run: &Option<HwReading>, i: usize| run.as_ref().and_then(|r| r.get(i).copied().flatten());
    let position = |kind: CounterKind| config.active.iter().position(|&k| k == kind);
    let series_of = |kind: CounterKind| -> Vec<Option<u64>> {
        runs.iter()
            .map(|run| position(kind).and_then(|i| cell(run, i)).map(|c| c.value))
            .collect()
    };
    let mut counters = BTreeMap::new();
    let mut summary = BTreeMap::new();
    let mut means: Vec<(CounterKind, f64)> = Vec::new();
    let mut scaling: Vec<(&str, Vec<ScaledCounter>)> = Vec::new();
    for (i, kind) in config.active.iter().enumerate() {
        if config.hidden.contains(kind) {
            continue;
        }
        let cells: Vec<Option<ScaledCounter>> = runs.iter().map(|r| cell(r, i)).collect();
        let values: Vec<Option<u64>> = cells.iter().map(|c| c.map(|c| c.value)).collect();
        let floats: Vec<f64> = values.iter().flatten().map(|&v| v as f64).collect();
        if let Some(s) = crate::benchmark::benchmark_result::MetricSummary::from_values(&floats) {
            means.push((*kind, s.mean));
            summary.insert(kind.name().to_string(), s);
        }
        scaling.push((kind.name(), cells.iter().flatten().copied().collect()));
        counters.insert(kind.name().to_string(), values);
    }
    let multiplexed_runs = (0..runs.len())
        .filter(|&r| {
            (0..config.active.len()).any(|i| cell(&runs[r], i).is_some_and(|c| c.scaled()))
        })
        .count();
    let samples: Vec<CounterSample> = runs
        .iter()
        .map(|run| {
            let get =
                |kind: CounterKind| position(kind).and_then(|i| cell(run, i)).map(|c| c.value);
            CounterSample {
                instructions: get(CounterKind::Instructions),
                cycles: get(CounterKind::Cycles),
                cache_references: get(CounterKind::CacheReferences),
                cache_misses: get(CounterKind::CacheMisses),
                branches: get(CounterKind::Branches),
                branch_misses: get(CounterKind::BranchMisses),
            }
        })
        .collect();
    let efficiency = Efficiency::mean_of(&samples);
    let frequency = frequency_export(config, &series_of, runs.len(), ctx);
    let mut advisories = Vec::new();
    if let Some(f) = &frequency {
        let eligible: Vec<f64> = f.eligible.clone();
        advisories.extend(frequency::warnings(&eligible, ctx.max_frequency_hz));
    }
    if config.active.contains(&CounterKind::CpuMigrations) {
        advisories.extend(jitter_warning(
            &series_of(CounterKind::CpuMigrations),
            ctx.real_seconds,
            ctx.single_cpu,
        ));
    }
    let freq_label = frequency.as_ref().and_then(|f| {
        f.export.mean.map(|mean| {
            let mut s = format!(
                "{}{}",
                if f.export.basis == Basis::UserTime {
                    "~"
                } else {
                    ""
                },
                frequency::format_hz(mean)
            );
            if let Some(sd) = f.export.stddev.filter(|&sd| sd > 0.0) {
                s.push_str(&format!(" ± {}", frequency::format_hz(sd)));
            }
            s
        })
    });
    let line = summary_line(
        &means,
        efficiency.as_ref(),
        freq_label.as_deref(),
        multiplexed_runs,
    );
    Some(CounterExport {
        counters,
        summary,
        multiplexed_runs,
        efficiency,
        frequency: frequency.map(|f| f.export),
        line,
        warning: multiplex_warning(&scaling),
        advisories,
    })
}

struct FrequencyWork {
    export: FrequencyExport,
    /// Per-run frequencies of the runs with enough time on CPU, in run order
    eligible: Vec<f64>,
}

/// Effective frequency of every run, when `cycles` was collected (#222).
fn frequency_export(
    config: &CounterConfig,
    series_of: &dyn Fn(CounterKind) -> Vec<Option<u64>>,
    run_count: usize,
    ctx: &RunContext,
) -> Option<FrequencyWork> {
    if !config.active.contains(&CounterKind::Cycles) {
        return None;
    }
    let basis = frequency::choose_basis(
        config.scope == Scope::All,
        config.active.contains(&CounterKind::TaskClock),
    )?;
    let cycles = series_of(CounterKind::Cycles);
    let task_clock = series_of(CounterKind::TaskClock);
    let seconds = |v: &[f64], i: usize| v.get(i).copied().unwrap_or(0.0);
    let per_run: Vec<Option<frequency::RunFrequency>> = (0..run_count)
        .map(|i| {
            frequency::run_frequency(
                basis,
                cycles[i],
                task_clock[i],
                seconds(ctx.user_seconds, i),
                seconds(ctx.system_seconds, i),
            )
        })
        .collect();
    let eligible: Vec<f64> = per_run
        .iter()
        .flatten()
        .filter(|f| f.is_eligible())
        .map(|f| f.hz)
        .collect();
    let stats = crate::benchmark::benchmark_result::MetricSummary::from_values(&eligible);
    Some(FrequencyWork {
        export: FrequencyExport {
            series: per_run.iter().map(|f| f.map(|f| f.hz)).collect(),
            mean: stats.as_ref().map(|s| s.mean),
            stddev: stats.as_ref().and_then(|s| s.stddev),
            basis,
        },
        eligible,
    })
}

/// `instr 12.3M · cycles 9.8M (IPC 1.26) @3.42 GHz · cache-miss 41.2k · cpu-migr 0.4`
fn summary_line(
    means: &[(CounterKind, f64)],
    efficiency: Option<&Efficiency>,
    frequency: Option<&str>,
    multiplexed_runs: usize,
) -> Option<String> {
    if means.is_empty() {
        return None;
    }
    let both = means.iter().any(|m| m.0 == CounterKind::Instructions)
        && means.iter().any(|m| m.0 == CounterKind::Cycles);
    let parts: Vec<String> = means
        .iter()
        .map(|&(kind, mean)| {
            let mut s = match kind {
                CounterKind::TaskClock => format!(
                    "{} {}",
                    kind.short(),
                    crate::output::format::format_duration(mean / 1e9, None)
                ),
                k if k.is_software() => format!("{} {}", k.short(), format_event_count(mean)),
                k => format!("{} {}", k.short(), crate::benchmark::format_count(mean)),
            };
            if kind == CounterKind::Cycles {
                if both {
                    if let Some(ipc) = efficiency.and_then(|e| e.ipc) {
                        s.push_str(&format!(" (IPC {ipc:.2})"));
                    }
                }
                if let Some(f) = frequency {
                    s.push_str(&format!(" @{f}"));
                }
            }
            s
        })
        .collect();
    let mut line = parts.join(" · ");
    if multiplexed_runs > 0 {
        line.push_str(&format!(" [scaled in {multiplexed_runs} runs]"));
    }
    Some(line)
}

/// Opened events of one run. Dropping closes the file descriptors.
pub struct OpenCounters {
    #[cfg(target_os = "linux")]
    inner: linux::Open,
}

/// Opens the active events for the next child spawned on this thread.
/// `None` when nothing is to be measured or the platform has no support.
pub fn open(config: &CounterConfig) -> Option<OpenCounters> {
    if config.mock || !config.is_active() {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        // Software events ignore the scope: `exclude_kernel` would hide them
        let events: Vec<(u32, u64, Scope)> = config
            .active
            .iter()
            .map(|k| {
                let scope = if k.is_software() {
                    Scope::All
                } else {
                    config.scope
                };
                (k.perf_type(), k.hw_config(), scope)
            })
            .collect();
        Some(OpenCounters {
            inner: linux::open_events(&events),
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

impl OpenCounters {
    /// Reads the totals after the child was reaped.
    pub fn read(&self) -> HwReading {
        #[cfg(target_os = "linux")]
        {
            self.inner.read()
        }
        #[cfg(not(target_os = "linux"))]
        {
            vec![]
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{CounterKind, Scope};
    use crate::pmu_scaling::{RawReading, ScaledCounter, READ_FORMAT};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    const PERF_FLAG_FD_CLOEXEC: libc::c_ulong = 8;

    /// `struct perf_event_attr` as 16 words: type/size, config,
    /// sample_period, sample_type, read_format, flags.
    fn attr(type_: u32, config: u64, scope: Scope, for_run: bool) -> [u64; 16] {
        let mut attr = [0u64; 16];
        attr[0] = u64::from(type_) | (u64::from(std::mem::size_of_val(&attr) as u32) << 32);
        attr[1] = config;
        attr[4] = READ_FORMAT;
        let mut flags: u64 = 1; // disabled
        flags |= 1 << 6; // exclude_hv
        if scope == Scope::User {
            flags |= 1 << 5; // exclude_kernel
        }
        if for_run {
            flags |= 1 << 1; // inherit
            flags |= 1 << 12; // enable_on_exec
        }
        attr[5] = flags;
        attr
    }

    fn open_fd(attr: &[u64; 16]) -> Result<OwnedFd, i32> {
        // SAFETY: `attr` is a valid perf_event_attr of the size it declares;
        // the other arguments are plain integers (this thread, any CPU, no group).
        let fd = unsafe {
            libc::syscall(
                libc::SYS_perf_event_open,
                attr.as_ptr(),
                0 as libc::pid_t,
                -1 as libc::c_int,
                -1 as libc::c_int,
                PERF_FLAG_FD_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0));
        }
        // SAFETY: the kernel returned a new fd that nothing else owns.
        Ok(unsafe { OwnedFd::from_raw_fd(fd as libc::c_int) })
    }

    /// Tries to open one event and closes it again.
    pub fn probe(kind: CounterKind, scope: Scope) -> Result<(), i32> {
        open_fd(&attr(kind.perf_type(), kind.hw_config(), scope, true)).map(drop)
    }

    pub struct Open {
        fds: Vec<Option<OwnedFd>>,
    }

    /// `events` are `(perf type, config, scope)` triples.
    pub fn open_events(events: &[(u32, u64, Scope)]) -> Open {
        Open {
            fds: events
                .iter()
                .map(|&(t, c, scope)| open_fd(&attr(t, c, scope, true)).ok())
                .collect(),
        }
    }

    impl Open {
        pub fn read(&self) -> Vec<Option<ScaledCounter>> {
            self.fds
                .iter()
                .map(|fd| {
                    let fd = fd.as_ref()?;
                    let mut buf = [0u8; RawReading::SIZE];
                    // SAFETY: `buf` is valid for `buf.len()` bytes and `fd` is open.
                    let n =
                        unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
                    if n != RawReading::SIZE as isize {
                        return None;
                    }
                    RawReading::from_bytes(&buf).map(ScaledCounter::from_raw)
                })
                .collect()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::process::Command;

        const PERF_TYPE_SOFTWARE: u32 = 1;
        const PERF_COUNT_SW_TASK_CLOCK: u64 = 1;

        /// Software events work without a PMU (CI VMs), so they check the
        /// enable_on_exec + inherit mechanism that the hardware events use.
        #[test]
        fn counts_the_child_and_not_the_spawning_thread() {
            let events = [(PERF_TYPE_SOFTWARE, PERF_COUNT_SW_TASK_CLOCK, Scope::User)];
            let open = open_events(&events);
            if open.fds[0].is_none() {
                eprintln!("skipped: perf_event_open is not permitted here");
                return;
            }
            // Busy work in this thread must not be counted.
            let t = std::time::Instant::now();
            while t.elapsed().as_millis() < 30 {}
            assert_eq!(open.read()[0].unwrap().raw, 0);

            let mut child = Command::new("sh")
                .args(["-c", "i=0; while [ $i -lt 30000 ]; do i=$((i+1)); done"])
                .spawn()
                .unwrap();
            child.wait().unwrap();
            let reading = open.read()[0].unwrap();
            assert!(reading.raw > 1_000_000, "task-clock ns: {reading:?}");
            assert!(reading.time_enabled > 0);
        }

        /// Needs a PMU: `PERFRATIO_TEST_PERF=1 cargo test`.
        #[test]
        fn hardware_instructions_are_counted() {
            if std::env::var_os("PERFRATIO_TEST_PERF").is_none() {
                return;
            }
            let open = open_events(&[(0, 1, Scope::User)]);
            let mut child = Command::new("sh").args(["-c", "true"]).spawn().unwrap();
            child.wait().unwrap();
            let c = open.read()[0].expect("instructions counter");
            assert!(c.value > 10_000, "{c:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pmu_scaling::RawReading;

    fn sc(value: u64, enabled: u64, running: u64) -> Option<ScaledCounter> {
        Some(ScaledCounter::from_raw(RawReading {
            value,
            time_enabled: enabled,
            time_running: running,
        }))
    }

    #[test]
    fn parses_lists() {
        let all = parse_list(DEFAULT_COUNTERS).unwrap();
        assert_eq!(all, CounterKind::ALL.to_vec());
        assert_eq!(
            parse_list("cycles,instructions,cycles").unwrap(),
            vec![CounterKind::Cycles, CounterKind::Instructions]
        );
        let err = parse_list("instructions,bogus").unwrap_err();
        assert!(
            err.contains("'bogus'") && err.contains("branch-misses"),
            "{err}"
        );
        assert!(parse_list("Cycles").is_err());
        assert!(parse_list("").is_err());
    }

    #[test]
    fn plan_all_available() {
        let (c, w) = plan(CounterKind::ALL.to_vec(), vec![], Some(1), |_, _| Ok(()));
        assert_eq!(c.availability, Availability::Measured);
        assert_eq!(c.scope, Scope::All);
        assert_eq!(c.active.len(), 6);
        assert!(w.is_empty());
    }

    #[test]
    fn plan_falls_back_to_user_scope() {
        let (c, w) = plan(vec![CounterKind::Cycles], vec![], Some(2), |_, s| match s {
            Scope::All => Err(13),
            Scope::User => Ok(()),
        });
        assert_eq!(
            (c.scope, c.availability),
            (Scope::User, Availability::Measured)
        );
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("perf_event_paranoid=2"), "{w:?}");
    }

    #[test]
    fn plan_unavailable_reasons() {
        for (errno, reason) in [
            (13, UnavailableReason::PerfParanoid),
            (1, UnavailableReason::PerfParanoid),
            (2, UnavailableReason::NoPmu),
            (95, UnavailableReason::NoPmu),
            (22, UnavailableReason::Other),
        ] {
            let (c, w) = plan(vec![CounterKind::Instructions], vec![], Some(3), |_, _| {
                Err(errno)
            });
            assert_eq!(c.availability, Availability::Unavailable(reason), "{errno}");
            assert!(!c.is_active());
            assert_eq!(w.iter().filter(|w| w.contains("not available")).count(), 1);
        }
    }

    #[test]
    fn plan_partial() {
        let (c, w) = plan(
            vec![CounterKind::Instructions, CounterKind::CacheMisses],
            vec![],
            Some(1),
            |k, _| {
                if k == CounterKind::CacheMisses {
                    Err(2)
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(
            c.availability,
            Availability::Partial(vec![CounterKind::CacheMisses])
        );
        assert_eq!(c.active, vec![CounterKind::Instructions]);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("cache-misses"));
    }

    #[test]
    fn overhead_median_and_clamped_subtraction() {
        let cal = vec![
            vec![sc(100, 1, 1), sc(10, 1, 1)],
            vec![sc(300, 1, 1), None],
            vec![sc(200, 1, 1), sc(30, 1, 1)],
        ];
        let o = median_overhead(&cal, 2);
        assert_eq!(o, vec![200, 30]);
        let mut run = vec![sc(150, 1, 1), sc(100, 1, 1), None];
        subtract_overhead(&mut run, &[200, 30, 5]);
        assert_eq!(run[0].unwrap().value, 0);
        assert_eq!(run[1].unwrap().value, 70);
        assert!(run[2].is_none());
        assert_eq!(median_overhead(&[], 2), vec![0, 0]);
    }

    fn config(kinds: &[CounterKind]) -> CounterConfig {
        CounterConfig {
            requested: kinds.to_vec(),
            active: kinds.to_vec(),
            scope: Scope::All,
            availability: Availability::Measured,
            partial_reason: None,
            hidden: vec![],
            mock: false,
        }
    }

    #[test]
    fn export_aligns_runs_and_derives_rates() {
        let cfg = config(&[CounterKind::Instructions, CounterKind::Cycles]);
        let runs = vec![
            Some(vec![sc(2000, 10, 10), sc(1000, 10, 10)]),
            None,
            Some(vec![sc(4000, 10, 10), sc(1000, 10, 5)]),
        ];
        let e = export(&cfg, &runs, &RunContext::default()).unwrap();
        assert_eq!(
            e.counters["instructions"],
            vec![Some(2000), None, Some(4000)]
        );
        // the second run's cycles were scaled: 1000 * 10 / 5
        assert_eq!(e.counters["cycles"], vec![Some(1000), None, Some(2000)]);
        assert_eq!(e.summary["instructions"].mean, 3000.0);
        assert_eq!(e.multiplexed_runs, 1);
        assert!(e.warning.unwrap().contains("cycles"));
        let ipc = e.efficiency.unwrap().ipc.unwrap();
        assert!((ipc - 2.0).abs() < 1e-9, "{ipc}");
        let line = e.line.unwrap();
        assert!(
            line.starts_with("instr 3.0k · cycles 1.5k (IPC 2.00)"),
            "{line}"
        );
        assert!(line.ends_with("[scaled in 1 runs]"), "{line}");
    }

    #[test]
    fn export_without_ipc_inputs_has_no_ipc() {
        let cfg = config(&[CounterKind::CacheMisses]);
        let e = export(&cfg, &[Some(vec![sc(5, 1, 1)])], &RunContext::default()).unwrap();
        assert_eq!(e.line.unwrap(), "cache-miss 5");
        assert!(export(&config(&[]), &[Some(vec![])], &RunContext::default()).is_none());
    }

    #[test]
    fn parses_software_names_and_shorthands() {
        use CounterKind::*;
        assert_eq!(
            parse_list("cpu-migrations,context-switches,page-faults,task-clock").unwrap(),
            CounterKind::SOFTWARE.to_vec()
        );
        assert_eq!(
            parse_list("sw").unwrap(),
            vec![CpuMigrations, ContextSwitches]
        );
        let mut want = CounterKind::ALL.to_vec();
        want.extend([CpuMigrations, ContextSwitches]);
        assert_eq!(parse_list("default,sw").unwrap(), want);
        assert_eq!(parse_list("frequency,cycles").unwrap(), vec![Cycles]);
        let err = parse_list("migrations").unwrap_err();
        assert!(
            err.contains("cpu-migrations") && err.contains("shorthands"),
            "{err}"
        );
        assert!(CpuMigrations.is_software() && !Cycles.is_software());
        assert_eq!((Cycles.perf_type(), TaskClock.perf_type()), (0, 1));
        assert_eq!(
            [TaskClock, PageFaults, ContextSwitches, CpuMigrations].map(CounterKind::hw_config),
            [1, 2, 3, 4]
        );
    }

    #[test]
    fn cycles_pull_in_a_hidden_task_clock() {
        let (r, hidden) = with_frequency_divisor(vec![CounterKind::Cycles]);
        assert_eq!(r, vec![CounterKind::Cycles, CounterKind::TaskClock]);
        assert_eq!(hidden, vec![CounterKind::TaskClock]);
        // an explicit task-clock is a real counter
        let (r, hidden) = with_frequency_divisor(vec![CounterKind::TaskClock, CounterKind::Cycles]);
        assert_eq!(r.len(), 2);
        assert!(hidden.is_empty());
        let (r, hidden) = with_frequency_divisor(vec![CounterKind::CpuMigrations]);
        assert_eq!(r, vec![CounterKind::CpuMigrations]);
        assert!(hidden.is_empty());
    }

    /// Paranoid 2: hardware falls back to user space, scheduler events are
    /// denied and reported as such, never as zeros.
    #[test]
    fn software_events_are_denied_at_paranoid_2() {
        let probe = |_: CounterKind, s: Scope| match s {
            Scope::All => Err(13),
            Scope::User => Ok(()),
        };
        let (c, w) = plan(
            vec![CounterKind::Cycles, CounterKind::CpuMigrations],
            vec![],
            Some(2),
            probe,
        );
        assert_eq!(c.scope, Scope::User);
        assert_eq!(c.active, vec![CounterKind::Cycles]);
        assert_eq!(
            c.availability,
            Availability::Partial(vec![CounterKind::CpuMigrations])
        );
        assert_eq!(c.partial_reason, Some(UnavailableReason::KernelScopeDenied));
        assert!(w.iter().any(|m| m.contains("cpu-migrations")), "{w:?}");

        // only software events requested: unavailable with the specific reason
        let (c, w) = plan(
            vec![CounterKind::CpuMigrations, CounterKind::ContextSwitches],
            vec![],
            Some(2),
            probe,
        );
        assert_eq!(
            c.availability,
            Availability::Unavailable(UnavailableReason::KernelScopeDenied)
        );
        assert!(!c.is_active());
        assert_eq!(w.len(), 1);
        assert!(
            w[0].contains("kernel-scope") && w[0].contains("perf_event_paranoid=2"),
            "{w:?}"
        );
    }

    #[test]
    fn software_events_are_probed_in_all_scope_only() {
        let mut seen = Vec::new();
        let (c, _) = plan(
            vec![CounterKind::Cycles, CounterKind::PageFaults],
            vec![],
            Some(1),
            |k, s| {
                seen.push((k, s));
                Ok(())
            },
        );
        assert_eq!(c.availability, Availability::Measured);
        assert!(seen.contains(&(CounterKind::PageFaults, Scope::All)));
        // software only: no hardware event is probed for the scope
        let mut seen = Vec::new();
        plan(vec![CounterKind::CpuMigrations], vec![], Some(1), |k, s| {
            seen.push((k, s));
            Ok(())
        });
        assert_eq!(seen, vec![(CounterKind::CpuMigrations, Scope::All)]);
    }

    #[test]
    fn a_hidden_task_clock_failing_is_silent() {
        let (c, w) = plan(
            vec![CounterKind::Cycles, CounterKind::TaskClock],
            vec![CounterKind::TaskClock],
            Some(2),
            |k, s| match (k, s) {
                (CounterKind::TaskClock, _) => Err(13),
                (_, Scope::All) => Err(13),
                _ => Ok(()),
            },
        );
        assert_eq!(c.availability, Availability::Measured);
        assert_eq!(c.active, vec![CounterKind::Cycles]);
        assert_eq!(w.len(), 1, "{w:?}"); // only the user-scope fallback note
    }

    fn times(n: usize, quiet: f64, moved_at: &[usize], moved: f64) -> Vec<f64> {
        (0..n)
            .map(|i| {
                let jitter = if i % 2 == 0 { 0.001 } else { -0.001 };
                if moved_at.contains(&i) {
                    moved + jitter
                } else {
                    quiet + jitter
                }
            })
            .collect()
    }

    fn migrations(n: usize, moved_at: &[usize], count: u64) -> Vec<Option<u64>> {
        (0..n)
            .map(|i| Some(if moved_at.contains(&i) { count } else { 0 }))
            .collect()
    }

    #[test]
    fn jitter_warning_condition() {
        // 6 of 30 runs migrated (2 each) and are 20% slower
        let at = [3, 8, 12, 17, 21, 26];
        let w = jitter_warning(&migrations(30, &at, 2), &times(30, 1.0, &at, 1.2), false).unwrap();
        assert!(
            w.starts_with("12 CPU migrations in 6 of 30 runs; those runs were 20% slower.\nTip: "),
            "{w}"
        );
        assert!(w.contains("--affinity"));
        // same counts but the migrated runs are not slower
        assert!(
            jitter_warning(&migrations(30, &at, 2), &times(30, 1.0, &at, 1.0), false).is_none()
        );
        // single-CPU affinity suppresses it
        assert!(jitter_warning(&migrations(30, &at, 2), &times(30, 1.0, &at, 1.2), true).is_none());
        // fewer than 5 runs
        assert!(
            jitter_warning(&migrations(4, &[1], 1), &times(4, 1.0, &[1], 2.0), false).is_none()
        );
        // no migration at all
        assert!(
            jitter_warning(&migrations(30, &[], 0), &times(30, 1.0, &[], 1.0), false).is_none()
        );
    }

    #[test]
    fn jitter_warning_threshold_is_ten_percent_of_runs() {
        // 3 of 30 = 10%: warns. 2 of 30 < 10%: silent.
        let at3 = [4, 10, 20];
        assert!(
            jitter_warning(&migrations(30, &at3, 1), &times(30, 1.0, &at3, 1.5), false).is_some()
        );
        let at2 = [4, 10];
        assert!(
            jitter_warning(&migrations(30, &at2, 1), &times(30, 1.0, &at2, 1.5), false).is_none()
        );
    }

    #[test]
    fn jitter_warning_when_every_run_migrated() {
        let all: Vec<usize> = (0..6).collect();
        let w = jitter_warning(&migrations(6, &all, 1), &times(6, 1.0, &all, 1.0), false).unwrap();
        assert!(
            w.starts_with("6 CPU migrations in all 6 runs.\nTip: "),
            "{w}"
        );
        // needs two quiet runs to compare against
        let five: Vec<usize> = (0..5).collect();
        assert!(
            jitter_warning(&migrations(6, &five, 1), &times(6, 1.0, &five, 2.0), false).is_none()
        );
    }

    #[test]
    fn jitter_warning_skips_unreadable_runs() {
        let at = [3, 8, 12, 17, 21, 26];
        let mut m = migrations(30, &at, 2);
        m[0] = None;
        m[1] = None;
        m[2] = None;
        m[4] = None;
        let w = jitter_warning(&m, &times(30, 1.0, &at, 1.2), false).unwrap();
        assert!(w.contains("of 26 runs"), "{w}");
        let none: Vec<Option<u64>> = vec![None; 30];
        assert!(jitter_warning(&none, &times(30, 1.0, &at, 1.2), false).is_none());
    }

    fn kinds_config(kinds: &[CounterKind], hidden: &[CounterKind], scope: Scope) -> CounterConfig {
        CounterConfig {
            scope,
            hidden: hidden.to_vec(),
            ..config(kinds)
        }
    }

    /// 20 runs with a flat 3.4 GHz, one value per kind in `active` order.
    fn flat_runs(per_run: &[u64]) -> Vec<Option<HwReading>> {
        (0..20)
            .map(|_| Some(per_run.iter().map(|&v| sc(v, 1, 1)).collect()))
            .collect()
    }

    #[test]
    fn export_computes_frequency_from_cycles_and_task_clock() {
        let cfg = kinds_config(
            &[CounterKind::Cycles, CounterKind::TaskClock],
            &[CounterKind::TaskClock],
            Scope::All,
        );
        // 3.4e9 cycles over 1 s of task-clock
        let runs = flat_runs(&[3_400_000_000, 1_000_000_000]);
        let secs = vec![1.0; 20];
        let ctx = RunContext {
            real_seconds: &secs,
            user_seconds: &secs,
            system_seconds: &secs,
            ..Default::default()
        };
        let e = export(&cfg, &runs, &ctx).unwrap();
        let f = e.frequency.unwrap();
        assert_eq!(f.basis, Basis::TaskClock);
        assert_eq!(f.series.len(), 20);
        assert_eq!(f.series[0], Some(3.4e9));
        assert_eq!(f.mean, Some(3.4e9));
        // the hidden divisor is neither exported nor printed
        assert_eq!(e.counters.keys().collect::<Vec<_>>(), ["cycles"]);
        assert_eq!(e.line.unwrap(), "cycles 3400.0M @3.40 GHz");
        assert!(e.advisories.is_empty());
    }

    #[test]
    fn user_scope_uses_user_time_never_task_clock() {
        let cfg = kinds_config(
            &[CounterKind::Cycles, CounterKind::TaskClock],
            &[CounterKind::TaskClock],
            Scope::User,
        );
        // task-clock says 2 s (includes kernel), user time is 1 s
        let runs = flat_runs(&[2_000_000_000, 2_000_000_000]);
        let user = vec![1.0; 20];
        let sys = vec![1.0; 20];
        let ctx = RunContext {
            real_seconds: &user,
            user_seconds: &user,
            system_seconds: &sys,
            ..Default::default()
        };
        let e = export(&cfg, &runs, &ctx).unwrap();
        let f = e.frequency.unwrap();
        assert_eq!(f.basis, Basis::UserTime);
        assert_eq!(f.mean, Some(2.0e9)); // not 1 GHz
        assert_eq!(e.line.unwrap(), "cycles 2000.0M @~2.00 GHz");
    }

    #[test]
    fn all_scope_without_task_clock_has_no_frequency() {
        let cfg = kinds_config(&[CounterKind::Cycles], &[], Scope::All);
        let e = export(&cfg, &flat_runs(&[1_000_000]), &RunContext::default()).unwrap();
        assert!(e.frequency.is_none());
        assert_eq!(e.line.unwrap(), "cycles 1.0M");
    }

    #[test]
    fn short_runs_stay_in_the_series_but_not_in_the_mean() {
        let cfg = kinds_config(
            &[CounterKind::Cycles, CounterKind::TaskClock],
            &[CounterKind::TaskClock],
            Scope::All,
        );
        let runs = vec![
            Some(vec![sc(1_000_000, 1, 1), sc(1_000_000, 1, 1)]), // 1 ms: 1 GHz, too short
            Some(vec![sc(40_000_000, 1, 1), sc(10_000_000, 1, 1)]), // 10 ms: 4 GHz
            None,
        ];
        let secs = vec![0.0; 3];
        let ctx = RunContext {
            real_seconds: &secs,
            user_seconds: &secs,
            system_seconds: &secs,
            ..Default::default()
        };
        let f = export(&cfg, &runs, &ctx).unwrap().frequency.unwrap();
        assert_eq!(f.series, vec![Some(1e9), Some(4e9), None]);
        assert_eq!(f.mean, Some(4e9));
        assert_eq!(f.stddev, None);
    }

    #[test]
    fn export_adds_software_counters_to_the_line() {
        let cfg = config(&[CounterKind::CpuMigrations, CounterKind::ContextSwitches]);
        let runs = vec![
            Some(vec![sc(0, 1, 1), sc(120, 1, 1)]),
            Some(vec![sc(1, 1, 1), sc(124, 1, 1)]),
        ];
        let e = export(&cfg, &runs, &RunContext::default()).unwrap();
        assert_eq!(e.counters["cpu-migrations"], vec![Some(0), Some(1)]);
        assert_eq!(e.line.unwrap(), "cpu-migr 0.5 · ctx-sw 122");
        assert!(e.frequency.is_none());
        assert_eq!(e.multiplexed_runs, 0);
    }

    #[test]
    fn mock_values_follow_the_time() {
        let mut cfg = config(&[CounterKind::Instructions, CounterKind::TaskClock]);
        cfg.mock = true;
        let r = mock_reading(&cfg, 0.5);
        assert_eq!(r[0].unwrap().value, 1_000_000_000);
        assert_eq!(r[1].unwrap().value, 500_000_000);
        assert!(open(&cfg).is_none());
    }
}
