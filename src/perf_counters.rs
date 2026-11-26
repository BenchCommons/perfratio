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

/// Parses a comma-separated `--counters` list. Duplicates are dropped, the
/// order is kept. Names are lowercase.
pub fn parse_list(list: &str) -> Result<Vec<CounterKind>, String> {
    let mut kinds = Vec::new();
    for name in list.split(',') {
        let kind = CounterKind::ALL
            .into_iter()
            .find(|k| k.name() == name.trim())
            .ok_or_else(|| {
                let valid: Vec<&str> = CounterKind::ALL.iter().map(|k| k.name()).collect();
                format!(
                    "unknown counter '{}' for '--counters' (valid: {})",
                    name.trim(),
                    valid.join(", ")
                )
            })?;
        if !kinds.contains(&kind) {
            kinds.push(kind);
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

/// Pure planning step: opens nothing itself, `probe` tries one event.
/// Returns the config and the warnings to print once.
pub fn plan(
    requested: Vec<CounterKind>,
    paranoid: Option<i32>,
    mut probe: impl FnMut(CounterKind, Scope) -> Result<(), i32>,
) -> (CounterConfig, Vec<String>) {
    const EPERM: i32 = 1;
    const EACCES: i32 = 13;
    let mut warnings = Vec::new();
    let mut scope = Scope::All;
    // Kernel-side counting is denied at the default paranoid level 2:
    // fall back to user space and say so (never report user-only as `all`).
    if let Some(&first) = requested.first() {
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
        match probe(kind, scope) {
            Ok(()) => active.push(kind),
            Err(errno) => failed.push((kind, errno)),
        }
    }
    let availability = if failed.is_empty() {
        Availability::Measured
    } else {
        let status = pmu::decide(
            paranoid,
            Some(pmu::Probe::Failed(failed.first().map_or(0, |f| f.1))),
        );
        let names: Vec<&str> = failed.iter().map(|f| f.0.name()).collect();
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
    (
        CounterConfig {
            requested,
            active,
            scope,
            availability,
            mock: false,
        },
        warnings,
    )
}

/// Resolves a `--counters` request against the machine. Returns the config and
/// the warnings to print (once, before the first benchmark).
pub fn resolve(requested: Vec<CounterKind>, debug_mode: bool) -> (CounterConfig, Vec<String>) {
    if debug_mode {
        return (
            CounterConfig {
                active: requested.clone(),
                requested,
                scope: Scope::All,
                availability: Availability::Measured,
                mock: true,
            },
            vec![],
        );
    }
    resolve_native(requested)
}

#[cfg(target_os = "linux")]
fn resolve_native(requested: Vec<CounterKind>) -> (CounterConfig, Vec<String>) {
    let paranoid = std::fs::read_to_string("/proc/sys/kernel/perf_event_paranoid")
        .ok()
        .and_then(|c| pmu::parse_paranoid(&c));
    plan(requested, paranoid, linux::probe)
}

#[cfg(not(target_os = "linux"))]
fn resolve_native(requested: Vec<CounterKind>) -> (CounterConfig, Vec<String>) {
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
    let mut means: Vec<(CounterKind, f64)> = Vec::new();
    let mut scaling: Vec<(&str, Vec<ScaledCounter>)> = Vec::new();
    for (i, kind) in config.active.iter().enumerate() {
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
            let get = |kind: CounterKind| {
                config
                    .active
                    .iter()
                    .position(|&k| k == kind)
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
    let efficiency = Efficiency::mean_of(&samples);
    let line = summary_line(&means, efficiency.as_ref(), multiplexed_runs);
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

/// `instr 12.3M · cycles 9.8M (IPC 1.26) · cache-miss 41.2k · branch-miss 18.0k`
fn summary_line(
    means: &[(CounterKind, f64)],
    efficiency: Option<&Efficiency>,
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
            let mut s = format!("{} {}", kind.short(), crate::benchmark::format_count(mean));
            if kind == CounterKind::Cycles && both {
                if let Some(ipc) = efficiency.and_then(|e| e.ipc) {
                    s.push_str(&format!(" (IPC {ipc:.2})"));
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
        let events: Vec<(u32, u64)> = config.active.iter().map(|k| (0, k.hw_config())).collect();
        Some(OpenCounters {
            inner: linux::open_events(&events, config.scope),
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
        open_fd(&attr(0, kind.hw_config(), scope, true)).map(drop)
    }

    pub struct Open {
        fds: Vec<Option<OwnedFd>>,
    }

    /// `events` are `(perf type, config)` pairs.
    pub fn open_events(events: &[(u32, u64)], scope: Scope) -> Open {
        Open {
            fds: events
                .iter()
                .map(|&(t, c)| open_fd(&attr(t, c, scope, true)).ok())
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
            let events = [(PERF_TYPE_SOFTWARE, PERF_COUNT_SW_TASK_CLOCK)];
            let open = open_events(&events, Scope::User);
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
            let open = open_events(&[(0, 1)], Scope::User);
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
        let (c, w) = plan(CounterKind::ALL.to_vec(), Some(1), |_, _| Ok(()));
        assert_eq!(c.availability, Availability::Measured);
        assert_eq!(c.scope, Scope::All);
        assert_eq!(c.active.len(), 6);
        assert!(w.is_empty());
    }

    #[test]
    fn plan_falls_back_to_user_scope() {
        let (c, w) = plan(vec![CounterKind::Cycles], Some(2), |_, s| match s {
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
            let (c, w) = plan(vec![CounterKind::Instructions], Some(3), |_, _| Err(errno));
            assert_eq!(c.availability, Availability::Unavailable(reason), "{errno}");
            assert!(!c.is_active());
            assert_eq!(w.iter().filter(|w| w.contains("not available")).count(), 1);
        }
    }

    #[test]
    fn plan_partial() {
        let (c, w) = plan(
            vec![CounterKind::Instructions, CounterKind::CacheMisses],
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
        let cfg = config(&[CounterKind::CacheMisses]);
        let e = export(&cfg, &[Some(vec![sc(5, 1, 1)])]).unwrap();
        assert_eq!(e.line.unwrap(), "cache-miss 5");
        assert!(export(&config(&[]), &[Some(vec![])]).is_none());
    }

    #[test]
    fn mock_values_follow_the_time() {
        let mut cfg = config(&[CounterKind::Instructions]);
        cfg.mock = true;
        let r = mock_reading(&cfg, 0.5);
        assert_eq!(r[0].unwrap().value, 1_000_000_000);
        assert!(open(&cfg).is_none());
    }
}
