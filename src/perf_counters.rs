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
use crate::pmu::{self, PmuStatus};
use crate::pmu_scaling::{multiplex_warning, ScaledCounter};
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
}

impl CounterKind {
    pub const ALL: [CounterKind; 6] = [
        CounterKind::Instructions,
        CounterKind::Cycles,
        CounterKind::CacheReferences,
        CounterKind::CacheMisses,
        CounterKind::Branches,
        CounterKind::BranchMisses,
    ];

    pub fn name(self) -> &'static str {
        match self {
            CounterKind::Instructions => "instructions",
            CounterKind::Cycles => "cycles",
            CounterKind::CacheReferences => "cache-references",
            CounterKind::CacheMisses => "cache-misses",
            CounterKind::Branches => "branches",
            CounterKind::BranchMisses => "branch-misses",
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
        }
    }

    /// `PERF_COUNT_HW_*` value
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn hw_config(self) -> u64 {
        match self {
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
        let kind = CounterKind::ALL
            .into_iter()
            .find(|k| k.name() == name)
            .ok_or_else(|| {
                let valid: Vec<&str> = CounterKind::ALL.iter().map(|k| k.name()).collect();
                format!(
                    "unknown counter '{name}' for '--counters' (valid: {})",
                    valid.join(", ")
                )
            })?;
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
    Other,
}

impl UnavailableReason {
    pub fn as_str(self) -> &'static str {
        match self {
            UnavailableReason::UnsupportedPlatform => "unsupported_platform",
            UnavailableReason::PerfParanoid => "perf_paranoid",
            UnavailableReason::NoPmu => "no_pmu",
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

/// Pure planning step: opens nothing itself, `probe` tries one event.
/// `requested_scope` is the scope of unsuffixed keys. Returns the config and
/// the warnings to print once, or an error when kernel counting was asked for
/// explicitly (`--scope kernel|split`, a `:k` modifier) and is denied: a
/// silent downgrade would invalidate the request.
pub fn plan(
    mut requested: Vec<CounterSpec>,
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
            .find(|s| s.scope == wanted)
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
            if spec.scope == Scope::All {
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
            Err(errno) => failed.push((spec, errno)),
        }
    }
    let availability = if failed.is_empty() {
        Availability::Measured
    } else {
        let status = pmu::decide(
            paranoid,
            Some(pmu::Probe::Failed(failed.first().map_or(0, |f| f.1))),
        );
        let names: Vec<String> = failed.iter().map(|f| f.0.key(scope)).collect();
        if active.is_empty() {
            warnings.push(format!(
                "hardware counters are not available, continuing without them: {}",
                pmu::diagnostic(&status, "perfratio")
                    .unwrap_or_else(|| pmu::summary(&status))
                    .trim_end()
            ));
            Availability::Unavailable(match status {
                PmuStatus::Denied { .. } => UnavailableReason::PerfParanoid,
                PmuStatus::Unsupported => UnavailableReason::NoPmu,
                _ => UnavailableReason::Other,
            })
        } else {
            warnings.push(format!(
                "these hardware counters are not available and are skipped: {}",
                names.join(", ")
            ));
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
    if debug_mode {
        return Ok((
            CounterConfig {
                active: requested.clone(),
                requested,
                scope: requested_scope,
                requested_scope,
                availability: Availability::Measured,
                mock: true,
            },
            vec![],
        ));
    }
    resolve_native(requested, requested_scope)
}

#[cfg(target_os = "linux")]
fn resolve_native(
    requested: Vec<CounterSpec>,
    requested_scope: Scope,
) -> Result<(CounterConfig, Vec<String>), String> {
    let paranoid = std::fs::read_to_string("/proc/sys/kernel/perf_event_paranoid")
        .ok()
        .and_then(|c| pmu::parse_paranoid(&c));
    plan(requested, requested_scope, paranoid, linux::probe)
}

#[cfg(not(target_os = "linux"))]
fn resolve_native(
    requested: Vec<CounterSpec>,
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
    pub line: Option<String>,
    pub warning: Option<String>,
}

/// The scope whose events feed the derived rates (IPC, miss rates): the
/// default scope if it was counted, else the only scope in use. Rates never
/// mix scopes.
fn rate_scope(config: &CounterConfig) -> Option<Scope> {
    if config.active.iter().any(|s| s.scope == config.scope) {
        return Some(config.scope);
    }
    let first = config.active.first()?.scope;
    config
        .active
        .iter()
        .all(|s| s.scope == first)
        .then_some(first)
}

/// Builds the export of all runs. `runs` has one entry per run (aligned with
/// `times`); `None` is a run whose counters could not be read.
pub fn export(config: &CounterConfig, runs: &[Option<HwReading>]) -> Option<CounterExport> {
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
    let line = summary_line(
        &means,
        config.scope,
        rate_scope(config),
        efficiency.as_ref(),
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
        line,
        warning: multiplex_warning(&scaling),
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
                    format!(
                        "{}{suffix} {}",
                        kind.short(),
                        crate::benchmark::format_count(value)
                    )
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
            .map(|s| (0, s.kind.hw_config(), s.scope))
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
        attr[5] = super::attr_flags(scope, for_run);
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
        open_fd(&attr(0, kind.hw_config(), scope, true)).map(drop)
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
        plan(all(kinds), Scope::All, paranoid, probe).unwrap()
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
        let (c, _) = plan(requested, Scope::All, Some(2), |_, s| match s {
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
            let err = plan(specs, scope, Some(2), denied).unwrap_err();
            assert!(
                err.contains("perf_event_paranoid <= 1 or CAP_PERFMON")
                    && err.contains("CAP_PERFMON"),
                "{err}"
            );
        }
        // user-only is never an error
        assert!(plan(
            vec![spec(Cycles, Scope::User)],
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
        let (c, w) = plan(specs, Scope::All, Some(1), |k, s| {
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
        assert!(export(&c, &[Some(vec![])]).is_none());
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
        let line = export(&cfg, &[Some(run)]).unwrap().line.unwrap();
        assert!(line.contains("cache-miss 182 (miss rate 18.2%)"), "{line}");
        assert!(line.contains("branch-miss 62 (miss rate 3.1%)"), "{line}");
        // no denominator, no rate
        let cfg = config(&all(&[CounterKind::CacheMisses]));
        let line = export(&cfg, &[Some(vec![sc(5, 1, 1)])])
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
        let e = export(&cfg, &runs).unwrap();
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
        let e = export(&cfg, &[Some(vec![sc(5, 1, 1)])]).unwrap();
        assert_eq!(e.line.unwrap(), "cache-miss 5");
        assert!(export(&config(&[]), &[Some(vec![])]).is_none());
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
        let e = export(&cfg, &runs).unwrap();
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
        let e = export(&cfg, &[Some(vec![sc(1200, 1, 1), sc(1600, 1, 1)])]).unwrap();
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
        let e = export(&cfg, &runs).unwrap();
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
        let e = export(&cfg, &[Some(vec![sc(300, 1, 1), sc(100, 1, 1)])]).unwrap();
        assert!(e.counters.contains_key("instructions"));
        assert!((e.efficiency.unwrap().ipc.unwrap() - 3.0).abs() < 1e-9);
        assert!(e.line.unwrap().contains("(IPC 3.00)"));
    }

    #[test]
    fn mock_values_follow_the_time_and_scope() {
        use CounterKind::*;
        let mut cfg = config(&[
            spec(Instructions, Scope::All),
            spec(Instructions, Scope::User),
            spec(Instructions, Scope::Kernel),
        ]);
        cfg.mock = true;
        let r = mock_reading(&cfg, 0.5);
        let v: Vec<u64> = r.iter().map(|c| c.unwrap().value).collect();
        assert_eq!(v, vec![1_000_000_000, 250_000_000, 750_000_000]);
        assert_eq!(v[1] + v[2], v[0]);
        assert!(open(&cfg).is_none());
    }
}
