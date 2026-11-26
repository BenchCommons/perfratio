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
    /// kernel context, so they cannot be scoped: they are always opened with
    /// user + kernel (`Scope::All`).
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

/// Which privilege levels one event counts (`perf_event_attr` exclude bits).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    /// User and kernel space. Needs `perf_event_paranoid <= 1` or `CAP_PERFMON`.
    All,
    /// User space only (`exclude_kernel`)
    User,
    /// Kernel space only (`exclude_user`). Needs `perf_event_paranoid <= 1`
    /// or `CAP_PERFMON`.
    Kernel,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::All => "all",
            Scope::User => "user",
            Scope::Kernel => "kernel",
        }
    }

    /// `perf stat` modifier suffix of a scoped key (`all` has none).
    fn suffix(self) -> &'static str {
        match self {
            Scope::All => "",
            Scope::User => ":u",
            Scope::Kernel => ":k",
        }
    }
}

/// The `--scope` option. The default is `all` (like `perf stat`), decision D1
/// in `counter-scope-modifiers.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScopeMode {
    User,
    Kernel,
    #[default]
    All,
    /// Every unmodified event is counted twice, as `E:u` and `E:k`
    Split,
}

impl ScopeMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "user" => Some(ScopeMode::User),
            "kernel" => Some(ScopeMode::Kernel),
            "all" => Some(ScopeMode::All),
            "split" => Some(ScopeMode::Split),
            _ => None,
        }
    }

    /// The scope that unsuffixed keys stand for (`counters_scope`).
    pub fn default_scope(self) -> Scope {
        match self {
            ScopeMode::User => Scope::User,
            ScopeMode::Kernel => Scope::Kernel,
            ScopeMode::All | ScopeMode::Split => Scope::All,
        }
    }
}

/// One counted event: what and at which privilege level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CounterSpec {
    pub kind: CounterKind,
    pub scope: Scope,
}

impl CounterSpec {
    /// JSON key: the event name, with `:u` / `:k` unless the scope is the
    /// effective default scope (`counters_scope`).
    pub fn key(self, default: Scope) -> String {
        if self.scope == default {
            self.kind.name().to_string()
        } else {
            format!("{}{}", self.kind.name(), self.scope.suffix())
        }
    }
}

/// Parses a comma-separated `--counters` list with optional `:u` / `:k`
/// modifiers (`:uk` and no modifier both mean the `--scope` default). A
/// modifier overrides `--scope` for its event; `--scope split` expands an
/// unmodified event to `E:u` and `E:k`. Duplicates after expansion are
/// dropped, the order is kept. Names are lowercase.
///
/// Shorthands: `default` (the hardware list), `sw` (`cpu-migrations`,
/// `context-switches`) and `frequency` (`cycles`; `task-clock` is added
/// automatically). The software events cannot be scoped: they are counted
/// from kernel context and are always user + kernel.
pub fn parse_list(list: &str, mode: ScopeMode) -> Result<Vec<CounterSpec>, String> {
    let mut specs: Vec<CounterSpec> = Vec::new();
    let mut push = |spec: CounterSpec| {
        if !specs.contains(&spec) {
            specs.push(spec);
        }
    };
    for item in list.split(',') {
        let item = item.trim();
        let (name, modifier) = match item.split_once(':') {
            Some((n, m)) => (n, Some(m)),
            None => (item, None),
        };
        let expanded: Vec<CounterKind> = match name {
            "default" | "sw" | "frequency" if modifier.is_some() => {
                return Err(format!(
                    "the shorthand '{name}' cannot take a modifier (in '{item}')"
                ));
            }
            "default" => CounterKind::ALL.to_vec(),
            "sw" => vec![CounterKind::CpuMigrations, CounterKind::ContextSwitches],
            "frequency" => vec![CounterKind::Cycles],
            _ => vec![CounterKind::ALL
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
                })?],
        };
        for kind in expanded {
            if kind.is_software() {
                if modifier.is_some() {
                    return Err(format!(
                        "software events cannot be scoped (in '{item}'): '{}' is always counted \
                         in user and kernel space",
                        kind.name()
                    ));
                }
                push(CounterSpec {
                    kind,
                    scope: Scope::All,
                });
                continue;
            }
            let scope = match modifier {
                None => None,
                Some(m) => parse_modifier(m, item)?,
            };
            match (scope, mode) {
                (Some(scope), _) => push(CounterSpec { kind, scope }),
                (None, ScopeMode::Split) => {
                    push(CounterSpec {
                        kind,
                        scope: Scope::User,
                    });
                    push(CounterSpec {
                        kind,
                        scope: Scope::Kernel,
                    });
                }
                (None, mode) => push(CounterSpec {
                    kind,
                    scope: mode.default_scope(),
                }),
            }
        }
    }
    Ok(specs)
}

/// `u`, `k` or `uk`/`ku`. `None` means "the `--scope` default".
fn parse_modifier(modifier: &str, item: &str) -> Result<Option<Scope>, String> {
    match modifier {
        "u" => Ok(Some(Scope::User)),
        "k" => Ok(Some(Scope::Kernel)),
        "uk" | "ku" => Ok(None),
        m if m.contains('h') => Err(format!(
            "modifier ':h' is not supported (in '{item}'); use ':u', ':k' or ':uk'"
        )),
        m => Err(format!(
            "unknown modifier ':{m}' in '{item}' for '--counters' (valid: ':u', ':k', ':uk')"
        )),
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
    Partial(Vec<CounterSpec>),
    Unavailable(UnavailableReason),
}

/// The resolved `--counters` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CounterConfig {
    pub requested: Vec<CounterSpec>,
    /// Events that are really collected, a subset of `requested` (after the
    /// scope fallback)
    pub active: Vec<CounterSpec>,
    /// Effective default scope (`counters_scope`): what unsuffixed keys count
    pub scope: Scope,
    /// Default scope that was asked for; differs from `scope` after a fallback
    pub requested_scope: Scope,
    pub availability: Availability,
    /// Why events are missing when `availability` is `Partial`
    pub partial_reason: Option<UnavailableReason>,
    /// Events that are collected but neither exported nor printed as counters:
    /// the `task-clock` that only divides `cycles` for the effective
    /// frequency (#222)
    pub hidden: Vec<CounterKind>,
    /// `--debug-mode`: synthetic values, nothing is opened
    pub mock: bool,
}

impl CounterConfig {
    pub fn is_active(&self) -> bool {
        !self.active.is_empty()
    }

    /// JSON key of the event at `index` of `active`.
    pub fn key(&self, index: usize) -> String {
        self.active[index].key(self.scope)
    }
}

/// Raw counts of one run, one entry per active event (`None`: not readable).
pub type HwReading = Vec<Option<ScaledCounter>>;

const EPERM: i32 = 1;
const EACCES: i32 = 13;

fn kernel_denied_message(paranoid: Option<i32>) -> String {
    format!(
        "kernel counters need kernel.perf_event_paranoid <= 1 or CAP_PERFMON (it is {}). \
         Run `sudo sysctl kernel.perf_event_paranoid=1` or grant CAP_PERFMON",
        paranoid.map_or("unknown".to_string(), |p| p.to_string())
    )
}

/// Why one event could not be opened.
fn reason_for(kind: CounterKind, errno: i32, paranoid: Option<i32>) -> UnavailableReason {
    if kind.is_software() && matches!(errno, EPERM | EACCES) {
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
fn with_frequency_divisor(mut requested: Vec<CounterSpec>) -> (Vec<CounterSpec>, Vec<CounterKind>) {
    let mut hidden = Vec::new();
    let has =
        |kind: CounterKind, requested: &[CounterSpec]| requested.iter().any(|s| s.kind == kind);
    if has(CounterKind::Cycles, &requested) && !has(CounterKind::TaskClock, &requested) {
        requested.push(CounterSpec {
            kind: CounterKind::TaskClock,
            scope: Scope::All,
        });
        hidden.push(CounterKind::TaskClock);
    }
    (requested, hidden)
}

/// Pure planning step: opens nothing itself, `probe` tries one event.
/// `requested_scope` is the scope of unsuffixed keys. `hidden` events are
/// collected, but failing to open them is silent (the frequency is then
/// unavailable). Software events are never scoped: they are probed and
/// opened in `Scope::All`, because the kernel does not count them with
/// `exclude_kernel`, and they do not take part in the scope fallback.
/// Returns the config and the warnings to print once, or an error when kernel
/// counting was asked for explicitly (`--scope kernel|split`, a `:k` modifier)
/// and is denied: a silent downgrade would invalidate the request.
pub fn plan(
    mut requested: Vec<CounterSpec>,
    hidden: Vec<CounterKind>,
    requested_scope: Scope,
    paranoid: Option<i32>,
    mut probe: impl FnMut(CounterKind, Scope) -> Result<(), i32>,
) -> Result<(CounterConfig, Vec<String>), String> {
    let mut warnings = Vec::new();
    let mut scope = requested_scope;
    // Kernel-side counting is denied at the default paranoid level 2. Probe
    // the real scope (a user-only probe says nothing about it), once per
    // distinct scope in use.
    let mut denied = |wanted: Scope, requested: &[CounterSpec]| {
        requested
            .iter()
            .find(|s| s.scope == wanted && !s.kind.is_software())
            .is_some_and(|s| matches!(probe(s.kind, wanted), Err(EPERM | EACCES)))
    };
    if denied(Scope::Kernel, &requested) {
        return Err(format!(
            "{}, or do not ask for kernel counters ('--scope kernel|split', ':k')",
            kernel_denied_message(paranoid)
        ));
    }
    if denied(Scope::All, &requested) {
        warnings.push(format!(
            "{}; counting user space only",
            kernel_denied_message(paranoid)
        ));
        for spec in &mut requested {
            if spec.scope == Scope::All && !spec.kind.is_software() {
                spec.scope = Scope::User;
            }
        }
        // `instructions` and `instructions:u` are one event now
        let mut seen = Vec::new();
        requested.retain(|s| {
            let new = !seen.contains(s);
            seen.push(*s);
            new
        });
        scope = if scope == Scope::All {
            Scope::User
        } else {
            scope
        };
    }
    let mut active = Vec::new();
    let mut failed: Vec<(CounterSpec, i32)> = Vec::new();
    for &spec in &requested {
        match probe(spec.kind, spec.scope) {
            Ok(()) => active.push(spec),
            Err(errno) => {
                if !hidden.contains(&spec.kind) {
                    failed.push((spec, errno));
                }
            }
        }
    }
    let mut partial_reason = None;
    let availability = if failed.is_empty() {
        Availability::Measured
    } else {
        let (first, first_errno) = failed[0];
        let reason = reason_for(first.kind, first_errno, paranoid);
        let names: Vec<String> = failed.iter().map(|f| f.0.key(scope)).collect();
        if active.iter().all(|s| hidden.contains(&s.kind)) {
            let scheduler_only = failed.iter().all(|f| f.0.kind.is_software());
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
    Ok((
        CounterConfig {
            requested,
            active,
            scope,
            requested_scope,
            availability,
            partial_reason,
            hidden,
            mock: false,
        },
        warnings,
    ))
}

/// Resolves a `--counters` request against the machine. Returns the config and
/// the warnings to print (once, before the first benchmark).
pub fn resolve(
    requested: Vec<CounterSpec>,
    requested_scope: Scope,
    debug_mode: bool,
) -> Result<(CounterConfig, Vec<String>), String> {
    let (requested, hidden) = with_frequency_divisor(requested);
    if debug_mode {
        return Ok((
            CounterConfig {
                active: requested.clone(),
                requested,
                scope: requested_scope,
                requested_scope,
                availability: Availability::Measured,
                partial_reason: None,
                hidden,
                mock: true,
            },
            vec![],
        ));
    }
    resolve_native(requested, hidden, requested_scope)
}

#[cfg(target_os = "linux")]
fn resolve_native(
    requested: Vec<CounterSpec>,
    hidden: Vec<CounterKind>,
    requested_scope: Scope,
) -> Result<(CounterConfig, Vec<String>), String> {
    let paranoid = std::fs::read_to_string("/proc/sys/kernel/perf_event_paranoid")
        .ok()
        .and_then(|c| pmu::parse_paranoid(&c));
    plan(requested, hidden, requested_scope, paranoid, linux::probe)
}

#[cfg(not(target_os = "linux"))]
fn resolve_native(
    requested: Vec<CounterSpec>,
    hidden: Vec<CounterKind>,
    requested_scope: Scope,
) -> Result<(CounterConfig, Vec<String>), String> {
    let why = if cfg!(target_os = "macos") {
        "on macOS the kperf framework needs root and cannot follow child processes"
    } else {
        "no supported counter API on this platform"
    };
    Ok((
        CounterConfig {
            requested,
            active: vec![],
            scope: requested_scope,
            requested_scope,
            availability: Availability::Unavailable(UnavailableReason::UnsupportedPlatform),
            partial_reason: None,
            hidden,
            mock: false,
        },
        vec![format!(
            "'--counters' is only supported on Linux ({why}); continuing without hardware counters."
        )],
    ))
}

/// Deterministic values for `--debug-mode`, derived from the simulated time.
pub fn mock_reading(config: &CounterConfig, time_real: f64) -> HwReading {
    config
        .active
        .iter()
        .map(|&spec| {
            let per_second = match spec.kind {
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
            let all = (per_second * time_real) as u64;
            // user is a quarter of everything, so user + kernel = all
            let value = match spec.scope {
                Scope::All => all,
                Scope::User => all / 4,
                Scope::Kernel => all - all / 4,
            };
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
    /// Lowest `time_running / time_enabled` over the runs, only for counters that
    /// were scaled or never ran (`counters_coverage` in the JSON export).
    pub coverage: BTreeMap<String, f64>,
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

/// The scope whose events feed the derived rates (IPC, miss rates): the
/// default scope if it was counted, else the only scope in use. Rates never
/// mix scopes.
fn rate_scope(config: &CounterConfig) -> Option<Scope> {
    let mut hardware = config.active.iter().filter(|s| !s.kind.is_software());
    if hardware.clone().any(|s| s.scope == config.scope) {
        return Some(config.scope);
    }
    let first = hardware.next()?.scope;
    hardware.all(|s| s.scope == first).then_some(first)
}

/// Builds the export of all runs. `runs` has one entry per run (aligned with
/// `times`); `None` is a run whose counters could not be read.
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
    let mut counters = BTreeMap::new();
    let mut summary = BTreeMap::new();
    let mut means: Vec<(CounterSpec, f64)> = Vec::new();
    let mut scaling: Vec<(String, Vec<ScaledCounter>)> = Vec::new();
    for (i, spec) in config.active.iter().enumerate() {
        if config.hidden.contains(&spec.kind) {
            continue;
        }
        let key = config.key(i);
        let cells: Vec<Option<ScaledCounter>> = runs.iter().map(|r| cell(r, i)).collect();
        let values: Vec<Option<u64>> = cells.iter().map(|c| c.map(|c| c.value)).collect();
        let floats: Vec<f64> = values.iter().flatten().map(|&v| v as f64).collect();
        if let Some(s) = crate::benchmark::benchmark_result::MetricSummary::from_values(&floats) {
            means.push((*spec, s.mean));
            summary.insert(key.clone(), s);
        }
        scaling.push((key.clone(), cells.iter().flatten().copied().collect()));
        counters.insert(key, values);
    }
    let multiplexed_runs = (0..runs.len())
        .filter(|&r| {
            (0..config.active.len()).any(|i| cell(&runs[r], i).is_some_and(|c| c.scaled()))
        })
        .count();
    let efficiency = rate_scope(config).and_then(|scope| {
        let samples: Vec<CounterSample> = runs
            .iter()
            .map(|run| {
                let get = |kind: CounterKind| {
                    config
                        .active
                        .iter()
                        .position(|&s| s.kind == kind && s.scope == scope)
                        .and_then(|i| cell(run, i))
                        .map(|c| c.value)
                };
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
        Efficiency::mean_of(&samples)
    });
    let frequency = frequency_export(config, runs, ctx);
    let mut advisories = Vec::new();
    if let Some(f) = &frequency {
        advisories.extend(frequency::warnings(&f.eligible, ctx.max_frequency_hz));
    }
    if let Some(i) = config
        .active
        .iter()
        .position(|s| s.kind == CounterKind::CpuMigrations)
    {
        let migrations: Vec<Option<u64>> = runs
            .iter()
            .map(|run| cell(run, i).map(|c| c.value))
            .collect();
        advisories.extend(jitter_warning(
            &migrations,
            ctx.real_seconds,
            ctx.single_cpu,
        ));
    }
    let freq_label = frequency.as_ref().and_then(|f| {
        f.export.mean.map(|mean| {
            let mut label = format!(
                "{}{}",
                if f.export.basis == Basis::UserTime {
                    "~"
                } else {
                    ""
                },
                frequency::format_hz(mean)
            );
            if let Some(sd) = f.export.stddev.filter(|&sd| sd > 0.0) {
                label.push_str(&format!(" ± {}", frequency::format_hz(sd)));
            }
            (f.scope, label)
        })
    });
    let line = summary_line(
        &means,
        config.scope,
        rate_scope(config),
        efficiency.as_ref(),
        freq_label
            .as_ref()
            .map(|(scope, label)| (*scope, label.as_str())),
        multiplexed_runs,
    );
    let scaling: Vec<(&str, Vec<ScaledCounter>)> = scaling
        .iter()
        .map(|(k, v)| (k.as_str(), v.clone()))
        .collect();
    let coverage: BTreeMap<String, f64> = scaling
        .iter()
        .filter_map(|(name, cells)| {
            let worst = cells
                .iter()
                .filter(|c| c.scaled() || c.never_ran())
                .map(|c| c.coverage())
                .reduce(f64::min)?;
            Some((name.to_string(), worst))
        })
        .collect();
    Some(CounterExport {
        counters,
        summary,
        multiplexed_runs,
        coverage,
        efficiency,
        frequency: frequency.map(|f| f.export),
        line,
        warning: multiplex_warning(&scaling),
        advisories,
    })
}

struct FrequencyWork {
    export: FrequencyExport,
    /// The scope of the `cycles` that were divided (`All` also for a `:u` + `:k` sum)
    scope: Scope,
    /// Per-run frequencies of the runs with enough time on CPU, in run order
    eligible: Vec<f64>,
}

/// Effective frequency of every run, when `cycles` was collected (#222).
/// `cycles` over user + kernel (or the sum of `cycles:u` and `cycles:k`) is
/// divided by `task-clock`; user-only `cycles` by the user CPU time and
/// kernel-only by the system CPU time, never by `task-clock`.
fn frequency_export(
    config: &CounterConfig,
    runs: &[Option<HwReading>],
    ctx: &RunContext,
) -> Option<FrequencyWork> {
    let value = |run: &Option<HwReading>, kind: CounterKind, scope: Scope| -> Option<u64> {
        let i = config
            .active
            .iter()
            .position(|s| s.kind == kind && s.scope == scope)?;
        run.as_ref()?.get(i).copied().flatten().map(|c| c.value)
    };
    let has = |scope: Scope| {
        config
            .active
            .iter()
            .any(|s| s.kind == CounterKind::Cycles && s.scope == scope)
    };
    let scope = if has(Scope::All) || (has(Scope::User) && has(Scope::Kernel)) {
        Scope::All
    } else if has(Scope::User) {
        Scope::User
    } else if has(Scope::Kernel) {
        Scope::Kernel
    } else {
        return None;
    };
    let basis = frequency::choose_basis(
        scope,
        config
            .active
            .iter()
            .any(|s| s.kind == CounterKind::TaskClock),
    )?;
    let seconds = |v: &[f64], i: usize| v.get(i).copied().unwrap_or(0.0);
    let per_run: Vec<Option<frequency::RunFrequency>> = runs
        .iter()
        .enumerate()
        .map(|(i, run)| {
            let cycles = if scope == Scope::All && !has(Scope::All) {
                value(run, CounterKind::Cycles, Scope::User)
                    .zip(value(run, CounterKind::Cycles, Scope::Kernel))
                    .map(|(u, k)| u + k)
            } else {
                value(run, CounterKind::Cycles, scope)
            };
            frequency::run_frequency(
                basis,
                cycles,
                value(run, CounterKind::TaskClock, Scope::All),
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
        scope,
        eligible,
    })
}

/// One printed value: a single scope, or a user + kernel pair shown as a
/// total with its breakdown.
#[derive(Clone, Copy)]
enum Slot {
    Single(Scope),
    Split,
}

/// `instr 12.3M · cycles 9.8M (IPC 1.26) · cache-miss 41.2k · branch-miss 18.0k`
///
/// Events counted in both scopes read `instr 1.70M [user 421k, kernel 1.28M]`;
/// events at another scope than the default carry the `:u` / `:k` suffix.
/// IPC is shown only where `instructions` and `cycles` share the slot.
fn summary_line(
    means: &[(CounterSpec, f64)],
    default: Scope,
    rate_scope: Option<Scope>,
    efficiency: Option<&Efficiency>,
    frequency: Option<(Scope, &str)>,
    multiplexed_runs: usize,
) -> Option<String> {
    if means.is_empty() {
        return None;
    }
    let mean = |kind: CounterKind, scope: Scope| {
        means
            .iter()
            .find(|(s, _)| s.kind == kind && s.scope == scope)
            .map(|m| m.1)
    };
    // `All` is the total. Without it a user + kernel pair makes one.
    let slot_value = |kind: CounterKind, slot: Slot| match slot {
        Slot::Single(scope) => mean(kind, scope),
        Slot::Split => Some(mean(kind, Scope::User)? + mean(kind, Scope::Kernel)?),
    };
    let mut slots: Vec<(CounterKind, Slot)> = Vec::new();
    for (spec, _) in means {
        let paired = spec.scope != Scope::All
            && mean(spec.kind, Scope::All).is_none()
            && mean(spec.kind, Scope::User).is_some()
            && mean(spec.kind, Scope::Kernel).is_some();
        let slot = if paired {
            Slot::Split
        } else {
            Slot::Single(spec.scope)
        };
        let seen = slots
            .iter()
            .any(|(k, s)| *k == spec.kind && matches!((s, slot), (Slot::Split, Slot::Split)));
        if !seen {
            slots.push((spec.kind, slot));
        }
    }
    let parts: Vec<String> = slots
        .iter()
        .map(|&(kind, slot)| {
            let value = slot_value(kind, slot).unwrap_or_default();
            let mut s = match slot {
                Slot::Single(scope) => {
                    let suffix = if scope == default { "" } else { scope.suffix() };
                    let shown = match kind {
                        CounterKind::TaskClock => {
                            crate::output::format::format_duration(value / 1e9, None)
                        }
                        k if k.is_software() => format_event_count(value),
                        _ => crate::benchmark::format_count(value),
                    };
                    format!("{}{suffix} {shown}", kind.short())
                }
                Slot::Split => format!(
                    "{} {} [user {}, kernel {}]",
                    kind.short(),
                    crate::benchmark::format_count(value),
                    crate::benchmark::format_count(mean(kind, Scope::User).unwrap_or_default()),
                    crate::benchmark::format_count(mean(kind, Scope::Kernel).unwrap_or_default()),
                ),
            };
            // miss rate next to the raw misses (#213): mean of the per-run
            // ratios where it exists, else the ratio of the means
            let miss_rate = |denominator: CounterKind, mean_rate: Option<f64>| {
                let total = slot_value(denominator, slot)?;
                match slot {
                    Slot::Single(scope) if Some(scope) == rate_scope && mean_rate.is_some() => {
                        mean_rate
                    }
                    _ => (total > 0.0).then(|| value / total),
                }
            };
            let rate = match kind {
                CounterKind::CacheMisses => miss_rate(
                    CounterKind::CacheReferences,
                    efficiency.and_then(|e| e.cache_miss_rate),
                ),
                CounterKind::BranchMisses => miss_rate(
                    CounterKind::Branches,
                    efficiency.and_then(|e| e.branch_miss_rate),
                ),
                _ => None,
            };
            if let Some(rate) = rate {
                s.push_str(&format!(" (miss rate {:.1}%)", rate * 100.0));
            }
            if kind == CounterKind::Cycles {
                if let Some(instr) = slot_value(CounterKind::Instructions, slot) {
                    let ipc = match slot {
                        // keep the mean of the per-run ratios where it exists
                        Slot::Single(scope) if Some(scope) == rate_scope => {
                            efficiency.and_then(|e| e.ipc)
                        }
                        _ => (value > 0.0).then(|| instr / value),
                    };
                    if let Some(ipc) = ipc {
                        s.push_str(&format!(" (IPC {ipc:.2})"));
                    }
                }
                if let Some((_, label)) = frequency.filter(|(fscope, _)| match slot {
                    Slot::Single(scope) => scope == *fscope,
                    Slot::Split => true,
                }) {
                    // after the IPC
                    s.push_str(&format!(" @{label}"));
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

/// `perf_event_attr` bit-field flags of a scope (see `perf_event_open(2)`):
/// `exclude_user` / `exclude_kernel` select the scope, `exclude_hv` and
/// `exclude_guest` are always set so that hypervisor and KVM guest counts
/// never leak in. `exclude_idle` is never set (the arm64 PMU driver rejects
/// it). Runs additionally `inherit` and `enable_on_exec`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn attr_flags(scope: Scope, for_run: bool) -> u64 {
    const DISABLED: u64 = 1 << 0;
    const INHERIT: u64 = 1 << 1;
    const EXCLUDE_USER: u64 = 1 << 4;
    const EXCLUDE_KERNEL: u64 = 1 << 5;
    const EXCLUDE_HV: u64 = 1 << 6;
    const ENABLE_ON_EXEC: u64 = 1 << 12;
    const EXCLUDE_GUEST: u64 = 1 << 20;
    let mut flags = DISABLED | EXCLUDE_HV | EXCLUDE_GUEST;
    match scope {
        Scope::All => {}
        Scope::User => flags |= EXCLUDE_KERNEL,
        Scope::Kernel => flags |= EXCLUDE_USER,
    }
    if for_run {
        flags |= INHERIT | ENABLE_ON_EXEC;
    }
    flags
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
        let events: Vec<(u32, u64, Scope)> = config
            .active
            .iter()
            .map(|s| (s.kind.perf_type(), s.kind.hw_config(), s.scope))
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

/// Tries to open the `--topdown` group once and closes it again. The error is
/// the `errno` of `perf_event_open`.
pub fn probe_group(events: &[(u32, u64)], scope: Scope) -> Result<(), i32> {
    #[cfg(target_os = "linux")]
    {
        linux::open_group(events, scope).map(drop)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (events, scope);
        Err(95) // EOPNOTSUPP
    }
}

/// The counters of one run: the `--counters` events and the `--topdown`
/// group. Their readings are joined into one [`HwReading`], counters first.
pub struct RunCounters {
    counters: Option<OpenCounters>,
    topdown: Option<OpenCounters>,
    n_counters: usize,
    n_topdown: usize,
}

/// Opens everything that is active for the next child spawned on this thread.
/// `None` when nothing is to be measured.
pub fn open_run(
    counters: Option<&CounterConfig>,
    topdown: Option<&crate::topdown::TopdownConfig>,
) -> Option<RunCounters> {
    let counters = counters.filter(|c| c.is_active() && !c.mock);
    let topdown = topdown.filter(|t| t.is_active() && !t.mock);
    if counters.is_none() && topdown.is_none() {
        return None;
    }
    #[cfg(target_os = "linux")]
    let open_topdown = |t: &crate::topdown::TopdownConfig| {
        let plan = t.plan.as_ref()?;
        let events: Vec<(u32, u64)> = plan.events.iter().map(|e| (e.pmu_type, e.config)).collect();
        // A group that cannot be opened leaves the reading empty (null runs)
        linux::open_group(&events, t.scope)
            .ok()
            .map(|inner| OpenCounters { inner })
    };
    #[cfg(not(target_os = "linux"))]
    let open_topdown = |_: &crate::topdown::TopdownConfig| None;
    Some(RunCounters {
        n_counters: counters.map_or(0, |c| c.active.len()),
        n_topdown: topdown.map_or(0, |t| t.event_count()),
        counters: counters.and_then(open),
        topdown: topdown.and_then(open_topdown),
    })
}

impl RunCounters {
    /// Reads the totals after the child was reaped. An event group that could
    /// not be opened gives `None` entries.
    pub fn read(&self) -> HwReading {
        let mut reading = self
            .counters
            .as_ref()
            .map_or_else(|| vec![None; self.n_counters], OpenCounters::read);
        reading.extend(
            self.topdown
                .as_ref()
                .map_or_else(|| vec![None; self.n_topdown], OpenCounters::read),
        );
        reading
    }
}

/// Whether calibration runs are needed: some real (non-mock) events are open.
pub fn needs_calibration(
    counters: Option<&CounterConfig>,
    topdown: Option<&crate::topdown::TopdownConfig>,
) -> bool {
    counters.is_some_and(|c| c.is_active() && !c.mock)
        || topdown.is_some_and(|t| t.is_active() && !t.mock)
}

/// Median overhead of the calibration runs per event of a combined reading.
/// The Intel metric events are fractions of a register, a fixed overhead has
/// no meaning for them: their entries stay 0.
pub fn calibrated_overhead(
    samples: &[HwReading],
    counters: Option<&CounterConfig>,
    topdown: Option<&crate::topdown::TopdownConfig>,
) -> Vec<u64> {
    let n_counters = counters.map_or(0, |c| c.active.len());
    let mut overhead = median_overhead(samples, crate::topdown::reading_len(counters, topdown));
    if topdown
        .and_then(|t| t.plan.as_ref())
        .is_some_and(|p| p.source == crate::topdown::Source::IntelPerfMetrics)
    {
        for o in overhead.iter_mut().skip(n_counters) {
            *o = 0;
        }
    }
    overhead
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
    fn attr(type_: u32, config: u64, scope: Scope, for_run: bool, member: bool) -> [u64; 16] {
        let mut attr = [0u64; 16];
        attr[0] = u64::from(type_) | (u64::from(std::mem::size_of_val(&attr) as u32) << 32);
        attr[1] = config;
        attr[4] = READ_FORMAT;
        let mut flags = super::attr_flags(scope, for_run);
        if member {
            // A group member follows its leader: only the leader is disabled
            // and switched on at exec
            flags &= !((1 << 0) | (1 << 12));
        }
        attr[5] = flags;
        attr
    }

    fn open_fd(attr: &[u64; 16], group_fd: libc::c_int) -> Result<OwnedFd, i32> {
        // SAFETY: `attr` is a valid perf_event_attr of the size it declares;
        // the other arguments are plain integers (this thread, any CPU, and
        // the leader's fd or -1).
        let fd = unsafe {
            libc::syscall(
                libc::SYS_perf_event_open,
                attr.as_ptr(),
                0 as libc::pid_t,
                -1 as libc::c_int,
                group_fd,
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
        open_fd(
            &attr(kind.perf_type(), kind.hw_config(), scope, true, false),
            -1,
        )
        .map(drop)
    }

    pub struct Open {
        fds: Vec<Option<OwnedFd>>,
    }

    /// `events` are `(perf type, config, scope)` triples.
    pub fn open_events(events: &[(u32, u64, Scope)]) -> Open {
        Open {
            fds: events
                .iter()
                .map(|&(t, c, scope)| open_fd(&attr(t, c, scope, true, false), -1).ok())
                .collect(),
        }
    }

    /// Opens `events` as one group, the first being the leader. Members only
    /// open if the leader did, so a failure leaves every entry `None`.
    pub fn open_group(events: &[(u32, u64)], scope: Scope) -> Result<Open, i32> {
        let mut fds: Vec<Option<OwnedFd>> = Vec::with_capacity(events.len());
        for (i, &(t, c)) in events.iter().enumerate() {
            let leader = fds
                .first()
                .and_then(|f| f.as_ref())
                .map_or(-1, |f| f.as_raw_fd());
            fds.push(Some(open_fd(&attr(t, c, scope, true, i > 0), leader)?));
        }
        Ok(Open { fds })
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

    fn spec(kind: CounterKind, scope: Scope) -> CounterSpec {
        CounterSpec { kind, scope }
    }

    fn all(kinds: &[CounterKind]) -> Vec<CounterSpec> {
        kinds.iter().map(|&k| spec(k, Scope::All)).collect()
    }

    #[test]
    fn parses_lists() {
        let specs = parse_list(DEFAULT_COUNTERS, ScopeMode::All).unwrap();
        assert_eq!(specs, all(&CounterKind::ALL));
        assert_eq!(
            parse_list("cycles,instructions,cycles", ScopeMode::All).unwrap(),
            all(&[CounterKind::Cycles, CounterKind::Instructions])
        );
        let err = parse_list("instructions,bogus", ScopeMode::All).unwrap_err();
        assert!(
            err.contains("'bogus'") && err.contains("branch-misses"),
            "{err}"
        );
        assert!(parse_list("Cycles", ScopeMode::All).is_err());
        assert!(parse_list("", ScopeMode::All).is_err());
    }

    #[test]
    fn parses_modifiers() {
        use CounterKind::*;
        let p = |l, m| parse_list(l, m).unwrap();
        assert_eq!(
            p("instructions:u,instructions:k,cycles:uk", ScopeMode::All),
            vec![
                spec(Instructions, Scope::User),
                spec(Instructions, Scope::Kernel),
                spec(Cycles, Scope::All)
            ]
        );
        // a modifier wins over --scope
        assert_eq!(
            p("cycles:k,instructions", ScopeMode::User),
            vec![spec(Cycles, Scope::Kernel), spec(Instructions, Scope::User)]
        );
        assert_eq!(
            p("cycles:u", ScopeMode::Kernel),
            vec![spec(Cycles, Scope::User)]
        );
        // `:uk` is the default scope
        assert_eq!(
            p("cycles:uk", ScopeMode::Kernel),
            vec![spec(Cycles, Scope::Kernel)]
        );
        // duplicates after expansion
        assert_eq!(
            p("instructions,instructions:uk", ScopeMode::All),
            all(&[Instructions])
        );
        assert_eq!(
            p("cycles:u,cycles", ScopeMode::User),
            vec![spec(Cycles, Scope::User)]
        );
    }

    #[test]
    fn split_expands_unmodified_events() {
        use CounterKind::*;
        assert_eq!(
            parse_list("instructions,cycles:k,cycles", ScopeMode::Split).unwrap(),
            vec![
                spec(Instructions, Scope::User),
                spec(Instructions, Scope::Kernel),
                spec(Cycles, Scope::Kernel),
                spec(Cycles, Scope::User),
            ]
        );
    }

    #[test]
    fn rejects_bad_modifiers() {
        let h = parse_list("cycles:h", ScopeMode::All).unwrap_err();
        assert!(h.contains("modifier ':h' is not supported"), "{h}");
        assert!(parse_list("cycles:uh", ScopeMode::All).is_err());
        let x = parse_list("cycles:x", ScopeMode::All).unwrap_err();
        assert!(x.contains("unknown modifier ':x'"), "{x}");
        assert!(parse_list("cycles:", ScopeMode::All).is_err());
        assert!(parse_list("bogus:u", ScopeMode::All).is_err());
    }

    #[test]
    fn scope_modes() {
        assert_eq!(ScopeMode::parse("split"), Some(ScopeMode::Split));
        assert_eq!(ScopeMode::parse("both"), None);
        assert_eq!(ScopeMode::default(), ScopeMode::All);
        assert_eq!(ScopeMode::Split.default_scope(), Scope::All);
        assert_eq!(ScopeMode::Kernel.default_scope(), Scope::Kernel);
    }

    #[test]
    fn attr_flags_follow_the_spec_table() {
        const EXCLUDE_USER: u64 = 1 << 4;
        const EXCLUDE_KERNEL: u64 = 1 << 5;
        const EXCLUDE_HV: u64 = 1 << 6;
        const EXCLUDE_IDLE: u64 = 1 << 7;
        const EXCLUDE_GUEST: u64 = 1 << 20;
        for (scope, user, kernel) in [
            (Scope::User, false, true),
            (Scope::Kernel, true, false),
            (Scope::All, false, false),
        ] {
            let f = attr_flags(scope, false);
            assert_eq!(f & EXCLUDE_USER != 0, user, "{scope:?}");
            assert_eq!(f & EXCLUDE_KERNEL != 0, kernel, "{scope:?}");
            assert!(f & EXCLUDE_HV != 0 && f & EXCLUDE_GUEST != 0);
            assert_eq!(f & EXCLUDE_IDLE, 0);
            assert_eq!(f & 1, 1, "disabled");
            assert_eq!(f & ((1 << 1) | (1 << 12)), 0, "no inherit outside runs");
            assert_eq!(
                attr_flags(scope, true) & ((1 << 1) | (1 << 12)),
                (1 << 1) | (1 << 12)
            );
        }
    }

    #[test]
    fn key_naming() {
        let u = spec(CounterKind::Instructions, Scope::User);
        assert_eq!(u.key(Scope::All), "instructions:u");
        assert_eq!(u.key(Scope::User), "instructions");
        assert_eq!(
            spec(CounterKind::Cycles, Scope::Kernel).key(Scope::All),
            "cycles:k"
        );
        assert_eq!(
            spec(CounterKind::Cycles, Scope::All).key(Scope::All),
            "cycles"
        );
    }

    fn plan_all(
        kinds: &[CounterKind],
        paranoid: Option<i32>,
        probe: impl FnMut(CounterKind, Scope) -> Result<(), i32>,
    ) -> (CounterConfig, Vec<String>) {
        plan(all(kinds), vec![], Scope::All, paranoid, probe).unwrap()
    }

    #[test]
    fn plan_all_available() {
        let (c, w) = plan_all(&CounterKind::ALL, Some(1), |_, _| Ok(()));
        assert_eq!(c.availability, Availability::Measured);
        assert_eq!((c.scope, c.requested_scope), (Scope::All, Scope::All));
        assert_eq!(c.active.len(), 6);
        assert!(w.is_empty());
    }

    #[test]
    fn plan_falls_back_to_user_scope() {
        let (c, w) = plan_all(&[CounterKind::Cycles], Some(2), |_, s| match s {
            Scope::User => Ok(()),
            _ => Err(13),
        });
        assert_eq!(
            (c.scope, c.requested_scope, c.availability),
            (Scope::User, Scope::All, Availability::Measured)
        );
        assert_eq!(c.active, vec![spec(CounterKind::Cycles, Scope::User)]);
        assert_eq!(w.len(), 1);
        assert!(
            w[0].contains("kernel counters need kernel.perf_event_paranoid <= 1 or CAP_PERFMON")
                && w[0].contains("counting user space only")
                && w[0].contains("sysctl kernel.perf_event_paranoid=1")
                && w[0].contains("it is 2"),
            "{w:?}"
        );
    }

    #[test]
    fn fallback_merges_the_events_that_become_equal() {
        use CounterKind::*;
        let requested = vec![
            spec(Instructions, Scope::All),
            spec(Instructions, Scope::User),
            spec(Cycles, Scope::User),
        ];
        let (c, _) = plan(requested, vec![], Scope::All, Some(2), |_, s| match s {
            Scope::User => Ok(()),
            _ => Err(1),
        })
        .unwrap();
        assert_eq!(
            c.active,
            vec![spec(Instructions, Scope::User), spec(Cycles, Scope::User)]
        );
        assert_eq!(c.key(0), "instructions");
    }

    #[test]
    fn explicit_kernel_scope_denied_is_an_error() {
        use CounterKind::*;
        let denied = |_: CounterKind, s: Scope| match s {
            Scope::User => Ok(()),
            _ => Err(13),
        };
        // `--scope kernel`, `--scope split` and an explicit `:k`
        for (specs, scope) in [
            (vec![spec(Instructions, Scope::Kernel)], Scope::Kernel),
            (
                vec![
                    spec(Instructions, Scope::User),
                    spec(Instructions, Scope::Kernel),
                ],
                Scope::All,
            ),
            (
                vec![spec(Cycles, Scope::All), spec(Instructions, Scope::Kernel)],
                Scope::All,
            ),
        ] {
            let err = plan(specs, vec![], scope, Some(2), denied).unwrap_err();
            assert!(
                err.contains("perf_event_paranoid <= 1 or CAP_PERFMON")
                    && err.contains("CAP_PERFMON"),
                "{err}"
            );
        }
        // user-only is never an error
        assert!(plan(
            vec![spec(Cycles, Scope::User)],
            vec![],
            Scope::User,
            Some(2),
            denied
        )
        .is_ok());
    }

    #[test]
    fn plan_probes_each_scope_in_use_once() {
        use CounterKind::*;
        let mut probed = Vec::new();
        let specs = vec![
            spec(Instructions, Scope::User),
            spec(Cycles, Scope::Kernel),
            spec(Branches, Scope::Kernel),
        ];
        let (c, w) = plan(specs, vec![], Scope::All, Some(1), |k, s| {
            probed.push((k, s));
            Ok(())
        })
        .unwrap();
        assert!(w.is_empty() && c.availability == Availability::Measured);
        // one preflight probe for the kernel scope, then one per event
        assert_eq!(
            probed.iter().filter(|p| p.1 == Scope::Kernel).count(),
            1 + 2
        );
    }

    #[test]
    fn denied_kernel_counting_is_unavailable_not_zero() {
        // paranoid 3 denies everything, user scope included
        let (c, w) = plan_all(&[CounterKind::Instructions], Some(3), |_, _| Err(13));
        assert_eq!(
            c.availability,
            Availability::Unavailable(UnavailableReason::PerfParanoid)
        );
        assert!(!c.is_active());
        assert!(export(&c, &[Some(vec![])], &RunContext::default()).is_none());
        assert!(w.iter().any(|w| w.contains("not available")));
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
            let (c, w) = plan_all(&[CounterKind::Instructions], Some(3), |_, _| Err(errno));
            assert_eq!(c.availability, Availability::Unavailable(reason), "{errno}");
            assert!(!c.is_active());
            assert_eq!(w.iter().filter(|w| w.contains("not available")).count(), 1);
        }
    }

    #[test]
    fn plan_partial() {
        let (c, w) = plan_all(
            &[CounterKind::Instructions, CounterKind::CacheMisses],
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
            Availability::Partial(vec![spec(CounterKind::CacheMisses, Scope::All)])
        );
        assert_eq!(c.active, all(&[CounterKind::Instructions]));
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

    fn config(specs: &[CounterSpec]) -> CounterConfig {
        CounterConfig {
            requested: specs.to_vec(),
            active: specs.to_vec(),
            scope: Scope::All,
            requested_scope: Scope::All,
            availability: Availability::Measured,
            partial_reason: None,
            hidden: vec![],
            mock: false,
        }
    }

    #[test]
    fn terminal_line_shows_miss_rates_next_to_the_raw_counts() {
        let cfg = config(&all(&[
            CounterKind::CacheReferences,
            CounterKind::CacheMisses,
            CounterKind::Branches,
            CounterKind::BranchMisses,
        ]));
        let run = vec![
            sc(1000, 10, 10),
            sc(182, 10, 10),
            sc(2000, 10, 10),
            sc(62, 10, 10),
        ];
        let line = export(&cfg, &[Some(run)], &RunContext::default())
            .unwrap()
            .line
            .unwrap();
        assert!(line.contains("cache-miss 182 (miss rate 18.2%)"), "{line}");
        assert!(line.contains("branch-miss 62 (miss rate 3.1%)"), "{line}");
        // no denominator, no rate
        let cfg = config(&all(&[CounterKind::CacheMisses]));
        let line = export(&cfg, &[Some(vec![sc(5, 1, 1)])], &RunContext::default())
            .unwrap()
            .line
            .unwrap();
        assert!(!line.contains("miss rate"), "{line}");
    }

    #[test]
    fn export_aligns_runs_and_derives_rates() {
        let cfg = config(&all(&[CounterKind::Instructions, CounterKind::Cycles]));
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
        assert_eq!(e.coverage.len(), 1);
        assert!((e.coverage["cycles"] - 0.5).abs() < 1e-12);
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
        let cfg = config(&all(&[CounterKind::CacheMisses]));
        let e = export(&cfg, &[Some(vec![sc(5, 1, 1)])], &RunContext::default()).unwrap();
        assert_eq!(e.line.unwrap(), "cache-miss 5");
        assert!(export(&config(&[]), &[Some(vec![])], &RunContext::default()).is_none());
    }

    #[test]
    fn export_suffixes_keys_and_filters_aligned_arrays() {
        use CounterKind::*;
        // user, kernel and the default scope side by side; run 2 unreadable
        let cfg = config(&[
            spec(Instructions, Scope::User),
            spec(Instructions, Scope::Kernel),
            spec(Cycles, Scope::All),
        ]);
        let runs = vec![
            Some(vec![sc(400, 1, 1), sc(1200, 1, 1), sc(1600, 1, 1)]),
            None,
            Some(vec![sc(440, 1, 1), sc(1300, 1, 1), sc(1800, 1, 1)]),
        ];
        let e = export(&cfg, &runs, &RunContext::default()).unwrap();
        assert_eq!(
            e.counters.keys().collect::<Vec<_>>(),
            ["cycles", "instructions:k", "instructions:u"]
        );
        assert_eq!(
            e.counters["instructions:u"],
            vec![Some(400), None, Some(440)]
        );
        assert_eq!(e.summary["instructions:k"].mean, 1250.0);
        // `instructions` exists only in other scopes, so there is no IPC
        assert!(e.efficiency.is_none());
        assert_eq!(
            e.line.unwrap(),
            "instr 1.7k [user 420, kernel 1.2k] · cycles 1.7k"
        );
        // a lone scoped event keeps its suffix
        let cfg = config(&[spec(Instructions, Scope::Kernel), spec(Cycles, Scope::All)]);
        let e = export(
            &cfg,
            &[Some(vec![sc(1200, 1, 1), sc(1600, 1, 1)])],
            &RunContext::default(),
        )
        .unwrap();
        assert_eq!(e.line.unwrap(), "instr:k 1.2k · cycles 1.6k");
    }

    #[test]
    fn split_line_shows_total_with_breakdown_and_per_slot_ipc() {
        use CounterKind::*;
        let mut cfg = config(&[
            spec(Instructions, Scope::User),
            spec(Instructions, Scope::Kernel),
            spec(Cycles, Scope::User),
            spec(Cycles, Scope::Kernel),
        ]);
        cfg.requested_scope = Scope::All;
        let runs = vec![Some(vec![
            sc(1000, 1, 1),
            sc(3000, 1, 1),
            sc(500, 1, 1),
            sc(1500, 1, 1),
        ])];
        let e = export(&cfg, &runs, &RunContext::default()).unwrap();
        assert_eq!(
            e.line.unwrap(),
            "instr 4.0k [user 1.0k, kernel 3.0k] · cycles 2.0k [user 500, kernel 1.5k] (IPC 2.00)"
        );
        // no unsuffixed scope in use and two scopes: no single IPC
        assert!(e.efficiency.is_none());
        assert_eq!(
            e.counters.keys().cloned().collect::<Vec<_>>(),
            ["cycles:k", "cycles:u", "instructions:k", "instructions:u"]
        );
    }

    #[test]
    fn user_only_run_uses_the_user_rates_and_plain_keys() {
        use CounterKind::*;
        let mut cfg = config(&[spec(Instructions, Scope::User), spec(Cycles, Scope::User)]);
        cfg.scope = Scope::User; // after the fallback
        let e = export(
            &cfg,
            &[Some(vec![sc(300, 1, 1), sc(100, 1, 1)])],
            &RunContext::default(),
        )
        .unwrap();
        assert!(e.counters.contains_key("instructions"));
        assert!((e.efficiency.unwrap().ipc.unwrap() - 3.0).abs() < 1e-9);
        assert!(e.line.unwrap().contains("(IPC 3.00)"));
    }

    #[test]
    fn parses_software_names_and_shorthands() {
        use CounterKind::*;
        let mode = ScopeMode::All;
        assert_eq!(
            parse_list(
                "cpu-migrations,context-switches,page-faults,task-clock",
                mode
            )
            .unwrap(),
            all(&CounterKind::SOFTWARE)
        );
        assert_eq!(
            parse_list("sw", mode).unwrap(),
            all(&[CpuMigrations, ContextSwitches])
        );
        let mut want = CounterKind::ALL.to_vec();
        want.extend([CpuMigrations, ContextSwitches]);
        assert_eq!(parse_list("default,sw", mode).unwrap(), all(&want));
        assert_eq!(
            parse_list("frequency,cycles", mode).unwrap(),
            all(&[Cycles])
        );
        let err = parse_list("migrations", mode).unwrap_err();
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
    fn software_events_cannot_be_scoped() {
        use CounterKind::*;
        for list in [
            "cpu-migrations:u",
            "context-switches:k",
            "page-faults:uk",
            "sw:u",
        ] {
            let err = parse_list(list, ScopeMode::All).unwrap_err();
            assert!(
                err.contains("cannot be scoped") || err.contains("cannot take a modifier"),
                "{list}: {err}"
            );
        }
        // `--scope` does not touch them either: one user + kernel event
        for mode in [ScopeMode::User, ScopeMode::Kernel, ScopeMode::Split] {
            let specs = parse_list("cpu-migrations,cycles", mode).unwrap();
            assert_eq!(specs[0], spec(CpuMigrations, Scope::All), "{mode:?}");
            assert_eq!(specs[0].key(mode.default_scope()), "cpu-migrations");
        }
        // hardware keeps the scope: `default:u` is refused, plain `default` expands
        assert!(parse_list("default:u", ScopeMode::All).is_err());
    }

    #[test]
    fn cycles_pull_in_a_hidden_task_clock() {
        use CounterKind::*;
        let (r, hidden) = with_frequency_divisor(all(&[Cycles]));
        assert_eq!(r, all(&[Cycles, TaskClock]));
        assert_eq!(hidden, vec![TaskClock]);
        // scoped or split cycles need only one divisor
        let (r, _) =
            with_frequency_divisor(vec![spec(Cycles, Scope::User), spec(Cycles, Scope::Kernel)]);
        assert_eq!(r.iter().filter(|s| s.kind == TaskClock).count(), 1);
        // an explicit task-clock is a real counter
        let (r, hidden) = with_frequency_divisor(all(&[TaskClock, Cycles]));
        assert_eq!(r.len(), 2);
        assert!(hidden.is_empty());
        let (r, hidden) = with_frequency_divisor(all(&[CpuMigrations]));
        assert_eq!(r, all(&[CpuMigrations]));
        assert!(hidden.is_empty());
    }

    /// Paranoid 2: hardware falls back to user space, scheduler events are
    /// denied and reported as such, never as zeros.
    #[test]
    fn software_events_are_denied_at_paranoid_2() {
        use CounterKind::*;
        let probe = |_: CounterKind, s: Scope| match s {
            Scope::User => Ok(()),
            _ => Err(13),
        };
        let (c, w) = plan(
            all(&[Cycles, CpuMigrations]),
            vec![],
            Scope::All,
            Some(2),
            probe,
        )
        .unwrap();
        assert_eq!((c.scope, c.requested_scope), (Scope::User, Scope::All));
        assert_eq!(c.active, vec![spec(Cycles, Scope::User)]);
        assert_eq!(
            c.availability,
            Availability::Partial(vec![spec(CpuMigrations, Scope::All)])
        );
        assert_eq!(c.partial_reason, Some(UnavailableReason::KernelScopeDenied));
        assert!(w.iter().any(|m| m.contains("cpu-migrations")), "{w:?}");

        // only software events requested: unavailable with the specific reason
        let (c, w) = plan(
            all(&[CpuMigrations, ContextSwitches]),
            vec![],
            Scope::All,
            Some(2),
            probe,
        )
        .unwrap();
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
    fn software_events_do_not_trigger_the_scope_fallback() {
        use CounterKind::*;
        // software events fail in `All`, but no hardware event was asked for
        let mut seen = Vec::new();
        let (c, _) = plan(
            all(&[CpuMigrations]),
            vec![],
            Scope::All,
            Some(1),
            |k, s| {
                seen.push((k, s));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(seen, vec![(CpuMigrations, Scope::All)]);
        assert_eq!(
            (c.scope, c.availability),
            (Scope::All, Availability::Measured)
        );
        // with `--scope user` they still count user + kernel
        let (c, _) = plan(
            vec![spec(Cycles, Scope::User), spec(PageFaults, Scope::All)],
            vec![],
            Scope::User,
            Some(1),
            |_, _| Ok(()),
        )
        .unwrap();
        assert_eq!(c.active.len(), 2);
        assert_eq!(c.active[1].scope, Scope::All);
    }

    #[test]
    fn a_hidden_task_clock_failing_is_silent() {
        use CounterKind::*;
        let (c, w) = plan(
            all(&[Cycles, TaskClock]),
            vec![TaskClock],
            Scope::All,
            Some(2),
            |k, s| match (k, s) {
                (TaskClock, _) => Err(13),
                (_, Scope::User) => Ok(()),
                _ => Err(13),
            },
        )
        .unwrap();
        assert_eq!(c.availability, Availability::Measured);
        assert_eq!(c.active, vec![spec(Cycles, Scope::User)]);
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
        let all_runs: Vec<usize> = (0..6).collect();
        let w = jitter_warning(
            &migrations(6, &all_runs, 1),
            &times(6, 1.0, &all_runs, 1.0),
            false,
        )
        .unwrap();
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
        for i in [0, 1, 2, 4] {
            m[i] = None;
        }
        let w = jitter_warning(&m, &times(30, 1.0, &at, 1.2), false).unwrap();
        assert!(w.contains("of 26 runs"), "{w}");
        let none: Vec<Option<u64>> = vec![None; 30];
        assert!(jitter_warning(&none, &times(30, 1.0, &at, 1.2), false).is_none());
    }

    /// 20 identical runs, one value per spec of `cfg.active`.
    fn flat_runs(per_run: &[u64]) -> Vec<Option<HwReading>> {
        (0..20)
            .map(|_| Some(per_run.iter().map(|&v| sc(v, 1, 1)).collect()))
            .collect()
    }

    fn hidden_task_clock(specs: Vec<CounterSpec>) -> CounterConfig {
        CounterConfig {
            hidden: vec![CounterKind::TaskClock],
            ..config(&specs)
        }
    }

    fn secs_ctx<'a>(real: &'a [f64], user: &'a [f64], system: &'a [f64]) -> RunContext<'a> {
        RunContext {
            real_seconds: real,
            user_seconds: user,
            system_seconds: system,
            ..Default::default()
        }
    }

    #[test]
    fn export_computes_frequency_from_cycles_and_task_clock() {
        use CounterKind::*;
        let cfg = hidden_task_clock(all(&[Cycles, TaskClock]));
        // 3.4e9 cycles over 1 s of task-clock
        let runs = flat_runs(&[3_400_000_000, 1_000_000_000]);
        let secs = vec![1.0; 20];
        let e = export(&cfg, &runs, &secs_ctx(&secs, &secs, &secs)).unwrap();
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
        use CounterKind::*;
        let cfg = CounterConfig {
            scope: Scope::User,
            ..hidden_task_clock(vec![spec(Cycles, Scope::User), spec(TaskClock, Scope::All)])
        };
        // task-clock says 2 s (includes kernel), user time is 1 s
        let runs = flat_runs(&[2_000_000_000, 2_000_000_000]);
        let one = vec![1.0; 20];
        let e = export(&cfg, &runs, &secs_ctx(&one, &one, &one)).unwrap();
        let f = e.frequency.unwrap();
        assert_eq!(f.basis, Basis::UserTime);
        assert_eq!(f.mean, Some(2.0e9)); // not 1 GHz
        assert_eq!(e.line.unwrap(), "cycles 2000.0M @~2.00 GHz");
    }

    #[test]
    fn kernel_scope_uses_system_time() {
        use CounterKind::*;
        let cfg = CounterConfig {
            scope: Scope::Kernel,
            ..hidden_task_clock(vec![
                spec(Cycles, Scope::Kernel),
                spec(TaskClock, Scope::All),
            ])
        };
        let runs = flat_runs(&[1_000_000_000, 9_000_000_000]);
        let user = vec![9.0; 20];
        let sys = vec![0.5; 20];
        let f = export(&cfg, &runs, &secs_ctx(&user, &user, &sys))
            .unwrap()
            .frequency
            .unwrap();
        assert_eq!((f.basis, f.mean), (Basis::SystemTime, Some(2.0e9)));
    }

    #[test]
    fn split_cycles_are_summed_and_use_task_clock() {
        use CounterKind::*;
        let cfg = hidden_task_clock(vec![
            spec(Cycles, Scope::User),
            spec(Cycles, Scope::Kernel),
            spec(TaskClock, Scope::All),
        ]);
        let runs = flat_runs(&[2_000_000_000, 1_400_000_000, 1_000_000_000]);
        let secs = vec![1.0; 20];
        let e = export(&cfg, &runs, &secs_ctx(&secs, &secs, &secs)).unwrap();
        let f = e.frequency.unwrap();
        assert_eq!((f.basis, f.mean), (Basis::TaskClock, Some(3.4e9)));
        assert!(e.line.unwrap().contains("@3.40 GHz"));
    }

    #[test]
    fn all_scope_without_task_clock_has_no_frequency() {
        let cfg = config(&all(&[CounterKind::Cycles]));
        let e = export(&cfg, &flat_runs(&[1_000_000]), &RunContext::default()).unwrap();
        assert!(e.frequency.is_none());
        assert_eq!(e.line.unwrap(), "cycles 1.0M");
    }

    #[test]
    fn short_runs_stay_in_the_series_but_not_in_the_mean() {
        use CounterKind::*;
        let cfg = hidden_task_clock(all(&[Cycles, TaskClock]));
        let runs = vec![
            Some(vec![sc(1_000_000, 1, 1), sc(1_000_000, 1, 1)]), // 1 ms: 1 GHz, too short
            Some(vec![sc(40_000_000, 1, 1), sc(10_000_000, 1, 1)]), // 10 ms: 4 GHz
            None,
        ];
        let secs = vec![0.0; 3];
        let f = export(&cfg, &runs, &secs_ctx(&secs, &secs, &secs))
            .unwrap()
            .frequency
            .unwrap();
        assert_eq!(f.series, vec![Some(1e9), Some(4e9), None]);
        assert_eq!(f.mean, Some(4e9));
        assert_eq!(f.stddev, None);
    }

    #[test]
    fn export_adds_software_counters_to_the_line() {
        use CounterKind::*;
        let cfg = config(&all(&[CpuMigrations, ContextSwitches, TaskClock]));
        let runs = vec![
            Some(vec![sc(0, 1, 1), sc(120, 1, 1), sc(2_000_000, 1, 1)]),
            Some(vec![sc(1, 1, 1), sc(124, 1, 1), sc(4_000_000, 1, 1)]),
        ];
        let e = export(&cfg, &runs, &RunContext::default()).unwrap();
        assert_eq!(e.counters["cpu-migrations"], vec![Some(0), Some(1)]);
        assert_eq!(
            e.counters["task-clock"],
            vec![Some(2_000_000), Some(4_000_000)]
        );
        assert_eq!(
            e.line.unwrap(),
            "cpu-migr 0.5 · ctx-sw 122 · task-clock 3.0 ms"
        );
        assert!(e.frequency.is_none());
        assert_eq!(e.multiplexed_runs, 0);
    }

    #[test]
    fn software_keys_carry_no_scope_suffix() {
        use CounterKind::*;
        // `--scope user`: hardware is unsuffixed, so is the always-All software event
        let cfg = CounterConfig {
            scope: Scope::User,
            ..config(&[spec(Cycles, Scope::User), spec(PageFaults, Scope::All)])
        };
        let e = export(&cfg, &flat_runs(&[5, 7]), &RunContext::default()).unwrap();
        assert_eq!(
            e.counters.keys().collect::<Vec<_>>(),
            ["cycles", "page-faults"]
        );
    }

    #[test]
    fn mock_values_follow_the_time_and_scope() {
        use CounterKind::*;
        let mut cfg = config(&[
            spec(Instructions, Scope::All),
            spec(Instructions, Scope::User),
            spec(Instructions, Scope::Kernel),
            spec(TaskClock, Scope::All),
        ]);
        cfg.mock = true;
        let r = mock_reading(&cfg, 0.5);
        let v: Vec<u64> = r.iter().map(|c| c.unwrap().value).collect();
        assert_eq!(v[..3], [1_000_000_000, 250_000_000, 750_000_000]);
        assert_eq!(v[1] + v[2], v[0]);
        assert_eq!(v[3], 500_000_000); // task-clock in ns
        assert!(open(&cfg).is_none());
    }
}
