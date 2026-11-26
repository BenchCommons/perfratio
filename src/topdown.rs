//! `--topdown`: Top-down Microarchitecture Analysis, level 1 (#221).
//!
//! Splits all issue slots of a run into four classes (retiring, bad
//! speculation, frontend bound, backend bound). The result is a hypothesis
//! about where the pipeline loses slots, not a diagnosis: at level 1 the
//! backend class mixes memory stalls and execution-unit contention.
//!
//! Two sources, see `docs/specs/topdown-tma.md`:
//! - Intel Ice Lake / Sunny Cove and later: the `perf_metrics` register,
//!   read as the `slots` event plus four `topdown-*` metric events. Their
//!   encodings come from `/sys/bus/event_source/devices/<pmu>/events`.
//! - AMD Zen 4 / Zen 5: six events combined with the formulas of perf's
//!   `PipelineL1` metrics (`amdzen4/pipeline.json`, `amdzen5/pipeline.json`).
//!
//! Everything here is pure (sysfs root and CPU description are injected) and
//! runs on all platforms; only opening the events (in [`crate::perf_counters`])
//! needs Linux. Nothing was run on a real PMU while writing this module.
//!
//! The readings travel with the `--counters` readings: [`HwReading`] holds
//! the active counters first and the topdown events after them, so overhead
//! calibration, concurrency sums and failed-run alignment work unchanged.

use crate::benchmark::benchmark_result::MetricSummary;
use crate::perf_counters::{HwReading, Scope};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Where the kernel lists PMUs and their events
pub const SYSFS_PMU_ROOT: &str = "/sys/bus/event_source/devices";

/// Below this many slots a run is dominated by process startup
pub const SHORT_RUN_SLOTS: f64 = 1_000_000.0;

/// perf warns that topdown accuracy may decrease over long periods
pub const LONG_RUN_SECONDS: f64 = 5.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    IntelPerfMetrics,
    AmdPipelineUtil,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::IntelPerfMetrics => "intel_perf_metrics",
            Source::AmdPipelineUtil => "amd_pipeline_util",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Source::IntelPerfMetrics => "Intel perf_metrics",
            Source::AmdPipelineUtil => "AMD pipeline utilisation",
        }
    }
}

/// Why `--topdown` cannot be used (open enum in the JSON export).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnavailableReason {
    UnsupportedPlatform,
    UnsupportedCpu,
    UnsupportedKernel,
    PerfParanoid,
    NoPmu,
    HybridCpuUnpinned,
    Other,
}

impl UnavailableReason {
    pub fn as_str(self) -> &'static str {
        match self {
            UnavailableReason::UnsupportedPlatform => "unsupported_platform",
            UnavailableReason::UnsupportedCpu => "unsupported_cpu",
            UnavailableReason::UnsupportedKernel => "unsupported_kernel",
            UnavailableReason::PerfParanoid => "perf_paranoid",
            UnavailableReason::NoPmu => "no_pmu",
            UnavailableReason::HybridCpuUnpinned => "hybrid_cpu_unpinned",
            UnavailableReason::Other => "other",
        }
    }

    /// One sentence for the warning printed once before the benchmarks.
    pub fn explanation(self) -> &'static str {
        match self {
            UnavailableReason::UnsupportedPlatform => "it needs Linux on x86_64",
            UnavailableReason::UnsupportedCpu => {
                "this CPU has no supported topdown events (needs Intel Ice Lake or later, or AMD Zen 4 or later)"
            }
            UnavailableReason::UnsupportedKernel => {
                "the kernel does not expose the perf_metrics events (`slots`, `topdown-*`); a newer kernel is needed"
            }
            UnavailableReason::PerfParanoid => {
                "perf_event_open is denied; lower kernel.perf_event_paranoid or grant CAP_PERFMON"
            }
            UnavailableReason::NoPmu => "no usable PMU (virtual machine without vPMU?)",
            UnavailableReason::HybridCpuUnpinned => {
                "this is a hybrid CPU; pin the command to P-cores with --affinity so the events count on one core type"
            }
            UnavailableReason::Other => {
                "the events could not be scheduled (if the NMI watchdog is on, try \
                 `echo 0 | sudo tee /proc/sys/kernel/nmi_watchdog`)"
            }
        }
    }
}

/// One event to open: the perf PMU `type` and the `config` word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventSpec {
    pub pmu_type: u32,
    pub config: u64,
}

/// The events to collect for a source. The first event is the group leader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub source: Source,
    /// Intel: slots, retiring, bad-spec, fe-bound, be-bound.
    /// AMD: cycles, no-ops-from-frontend, backend-stalls, smt-contention,
    /// ops dispatched, ops retired.
    pub events: Vec<EventSpec>,
    /// Dispatch width of the AMD core (6 on Zen 4, 8 on Zen 5); 0 on Intel
    pub width: u64,
}

/// The resolved `--topdown` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopdownConfig {
    /// `None` when unavailable
    pub plan: Option<Plan>,
    pub unavailable: Option<UnavailableReason>,
    pub scope: Scope,
    /// `--debug-mode`: synthetic values, nothing is opened
    pub mock: bool,
}

impl TopdownConfig {
    pub fn is_active(&self) -> bool {
        self.plan.is_some()
    }

    /// Number of events this config adds to a [`HwReading`]
    pub fn event_count(&self) -> usize {
        self.plan.as_ref().map_or(0, |p| p.events.len())
    }

    pub fn unavailable(reason: UnavailableReason) -> Self {
        TopdownConfig {
            plan: None,
            unavailable: Some(reason),
            scope: Scope::All,
            mock: false,
        }
    }
}

// ---------------------------------------------------------------------------
// CPU identification and event discovery

/// What `/proc/cpuinfo` says about the first CPU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostCpu {
    pub vendor: String,
    pub family: u32,
    pub model: u32,
    pub hypervisor: bool,
}

/// Parses the text of `/proc/cpuinfo` (x86).
pub fn parse_cpuinfo(text: &str) -> Option<HostCpu> {
    let field = |key: &str| {
        text.lines().find_map(|line| {
            let (k, v) = line.split_once(':')?;
            (k.trim() == key).then(|| v.trim().to_string())
        })
    };
    let flags = field("flags").unwrap_or_default();
    Some(HostCpu {
        vendor: field("vendor_id")?,
        family: field("cpu family")?.parse().ok()?,
        model: field("model")?.parse().ok()?,
        hypervisor: flags.split_whitespace().any(|f| f == "hypervisor"),
    })
}

/// Intel family 6 models whose cores have the `perf_metrics` register
/// (perf's `mapfile.csv` and `arch/x86/events/intel/core.c`). Only used to
/// tell "kernel too old" from "CPU too old" when sysfs lacks the events; the
/// sysfs files themselves decide whether `--topdown` works.
const INTEL_PERF_METRICS_MODELS: &[u32] = &[
    0x6A, 0x6C, 0x7D, 0x7E, 0x9D, // Ice Lake
    0x8C, 0x8D, // Tiger Lake
    0xA7, // Rocket Lake
    0x97, 0x9A, 0xB7, 0xBA, 0xBF, // Alder Lake, Raptor Lake
    0x8F, 0xCF, // Sapphire Rapids, Emerald Rapids
    0xAA, 0xAC, 0xBD, 0xC5, 0xC6, // Meteor Lake, Lunar Lake, Arrow Lake
    0xAD, 0xAE, // Granite Rapids
];

/// AMD core generation for the `PipelineL1` events, following perf's
/// `mapfile.csv` (family 0x19: Zen 3 for models `[245]x` and `0x`, otherwise
/// Zen 4; family 0x1a: Zen 5 for models `[12467]x` and `0x`). Model 0x30
/// to 0x3f (Zen 3 "Trento") is excluded although the map file sends it to Zen 4.
fn amd_width(family: u32, model: u32) -> Option<u64> {
    let high = model >> 4;
    match family {
        0x19 if matches!(high, 0x1 | 0x6 | 0x7 | 0x8..=0xF) => Some(6),
        0x1A if matches!(high, 0x0..=0x2 | 0x4 | 0x6 | 0x7) => Some(8),
        _ => None,
    }
}

/// Parses a sysfs event file such as `event=0x00,umask=0x80` into the raw
/// `config` word (`event | umask << 8`). Terms other than `event` and
/// `umask` are not supported.
pub fn parse_event_config(text: &str) -> Option<u64> {
    let (mut event, mut umask) = (0u64, 0u64);
    for term in text.trim().split(',') {
        let (key, value) = term.split_once('=')?;
        let value = value.trim();
        let number = match value.strip_prefix("0x") {
            Some(hex) => u64::from_str_radix(hex, 16).ok()?,
            None => value.parse().ok()?,
        };
        match key.trim() {
            "event" => event = number,
            "umask" => umask = number,
            _ => return None,
        }
    }
    (event <= 0xff && umask <= 0xff).then_some(event | umask << 8)
}

/// Raw config word of an AMD event code (12 bits) and unit mask.
fn amd_config(event: u64, umask: u64) -> u64 {
    (event & 0xff) | (umask & 0xff) << 8 | (event >> 8 & 0xf) << 32
}

fn read(root: &Path, relative: &str) -> Option<String> {
    std::fs::read_to_string(root.join(relative)).ok()
}

fn pmu_type(root: &Path, pmu: &str) -> Option<u32> {
    read(root, &format!("{pmu}/type"))?.trim().parse().ok()
}

fn intel_plan(host: &HostCpu, root: &Path, pinned: bool) -> Result<Plan, UnavailableReason> {
    use UnavailableReason::*;
    let hybrid = root.join("cpu_core").is_dir();
    let pmu = if hybrid {
        "cpu_core"
    } else if root.join("cpu").is_dir() {
        "cpu"
    } else if root.join("cpu_atom").is_dir() {
        return Err(UnsupportedCpu);
    } else {
        return Err(NoPmu);
    };
    if hybrid && !pinned {
        return Err(HybridCpuUnpinned);
    }
    let event = |name: &str| {
        read(root, &format!("{pmu}/events/{name}")).and_then(|t| parse_event_config(&t))
    };
    let names = [
        "slots",
        "topdown-retiring",
        "topdown-bad-spec",
        "topdown-fe-bound",
        "topdown-be-bound",
    ];
    let configs: Vec<Option<u64>> = names.iter().map(|n| event(n)).collect();
    let Some(configs) = configs.into_iter().collect::<Option<Vec<u64>>>() else {
        return Err(if host.hypervisor {
            NoPmu
        } else if INTEL_PERF_METRICS_MODELS.contains(&host.model) {
            UnsupportedKernel
        } else {
            UnsupportedCpu
        });
    };
    let Some(pmu_type) = pmu_type(root, pmu) else {
        return Err(UnsupportedKernel);
    };
    Ok(Plan {
        source: Source::IntelPerfMetrics,
        events: configs
            .into_iter()
            .map(|config| EventSpec { pmu_type, config })
            .collect(),
        width: 0,
    })
}

fn amd_plan(host: &HostCpu, root: &Path) -> Result<Plan, UnavailableReason> {
    use UnavailableReason::*;
    let width = amd_width(host.family, host.model).ok_or(UnsupportedCpu)?;
    let pmu_type = pmu_type(root, "cpu").ok_or(NoPmu)?;
    // Encodings of perf's amdzen4 and amdzen5 JSON (identical on both)
    let codes = [
        (0x76, 0x00),  // ls_not_halted_cyc
        (0x1a0, 0x01), // de_no_dispatch_per_slot.no_ops_from_frontend
        (0x1a0, 0x1e), // de_no_dispatch_per_slot.backend_stalls
        (0x1a0, 0x60), // de_no_dispatch_per_slot.smt_contention
        (0xaa, 0x07),  // de_src_op_disp.all
        (0xc1, 0x00),  // ex_ret_ops
    ];
    Ok(Plan {
        source: Source::AmdPipelineUtil,
        events: codes
            .iter()
            .map(|&(event, umask)| EventSpec {
                pmu_type,
                config: amd_config(event, umask),
            })
            .collect(),
        width,
    })
}

/// Chooses the source for this CPU. `root` is the sysfs PMU directory,
/// `pinned` says whether `--affinity` restricts the command to one core type.
pub fn plan(host: &HostCpu, root: &Path, pinned: bool) -> Result<Plan, UnavailableReason> {
    match host.vendor.as_str() {
        "GenuineIntel" if host.family == 6 => intel_plan(host, root, pinned),
        "AuthenticAMD" => amd_plan(host, root),
        _ => Err(UnavailableReason::UnsupportedCpu),
    }
}

// ---------------------------------------------------------------------------
// Formulas

/// The four level-1 classes as fractions of all slots.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fractions {
    pub retiring: f64,
    pub bad_speculation: f64,
    pub frontend_bound: f64,
    pub backend_bound: f64,
    /// AMD only: slots lost because the sibling thread was dispatching
    pub smt_contention: Option<f64>,
    /// All slots of the run (the weight of this run in a slot-weighted view)
    pub slots: f64,
}

/// Intel: `x = metric_x / (retiring + bad_spec + fe_bound + be_bound)`, as
/// perf's `tma_*` expressions do. `values` are `slots` followed by the four
/// metric counts. `None` for a run without slots.
pub fn intel_fractions(values: &[u64]) -> Option<Fractions> {
    let [slots, retiring, bad, fe, be] = <[u64; 5]>::try_from(values).ok()?;
    let sum = retiring as f64 + bad as f64 + fe as f64 + be as f64;
    if slots == 0 || sum == 0.0 {
        return None;
    }
    Some(Fractions {
        retiring: retiring as f64 / sum,
        bad_speculation: bad as f64 / sum,
        frontend_bound: fe as f64 / sum,
        backend_bound: be as f64 / sum,
        smt_contention: None,
        slots: slots as f64,
    })
}

/// AMD `PipelineL1` (perf `amdzen4/pipeline.json`): `width` dispatch slots per
/// not-halted cycle. `values` are cycles, frontend, backend, smt contention,
/// ops dispatched and ops retired. Ops dispatched that did not retire are bad
/// speculation (0 if a multiplexing artefact makes retired exceed dispatched).
pub fn amd_fractions(width: u64, values: &[u64]) -> Option<Fractions> {
    let [cycles, fe, be, smt, dispatched, retired] = <[u64; 6]>::try_from(values).ok()?;
    let slots = width as f64 * cycles as f64;
    if slots == 0.0 {
        return None;
    }
    Some(Fractions {
        retiring: retired as f64 / slots,
        bad_speculation: dispatched.saturating_sub(retired) as f64 / slots,
        frontend_bound: fe as f64 / slots,
        backend_bound: be as f64 / slots,
        smt_contention: Some(smt as f64 / slots),
        slots,
    })
}

/// Fractions of one run from its raw topdown readings (the topdown part of
/// a [`HwReading`]). `None` if any event could not be read or never ran.
pub fn run_fractions(
    plan: &Plan,
    readings: &[Option<crate::pmu_scaling::ScaledCounter>],
) -> Option<Fractions> {
    if readings.len() != plan.events.len() {
        return None;
    }
    let values: Option<Vec<u64>> = readings
        .iter()
        .map(|r| r.filter(|c| !c.never_ran()).map(|c| c.value))
        .collect();
    let values = values?;
    match plan.source {
        Source::IntelPerfMetrics => intel_fractions(&values),
        Source::AmdPipelineUtil => amd_fractions(plan.width, &values),
    }
}

/// The class marked as the primary bottleneck: the largest of bad
/// speculation, frontend bound and backend bound (and `smt_contention` on
/// AMD). Retiring is never marked. Ties go to backend, frontend, bad
/// speculation, SMT contention, in that order.
pub fn bottleneck(retiring_excluded: &[(&'static str, f64)]) -> Option<&'static str> {
    let mut best: Option<(&'static str, f64)> = None;
    for &(name, value) in retiring_excluded {
        if best.is_none_or(|(_, b)| value > b) {
            best = Some((name, value));
        }
    }
    best.map(|b| b.0)
}

// ---------------------------------------------------------------------------
// Export

/// `topdown` object of a result: per-run fractions aligned with `times`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TopdownSeries {
    pub level: u8,
    /// `intel_perf_metrics` or `amd_pipeline_util`
    pub source: String,
    pub retiring: Vec<Option<f64>>,
    pub bad_speculation: Vec<Option<f64>>,
    pub frontend_bound: Vec<Option<f64>>,
    pub backend_bound: Vec<Option<f64>>,
    /// AMD only
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smt_contention: Option<Vec<Option<f64>>>,
}

/// `topdown_summary` object of a result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TopdownSummary {
    pub retiring: MetricSummary,
    pub bad_speculation: MetricSummary,
    pub frontend_bound: MetricSummary,
    pub backend_bound: MetricSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smt_contention: Option<MetricSummary>,
    /// Largest non-retiring class (open enum)
    pub bottleneck: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TopdownExport {
    pub series: TopdownSeries,
    pub summary: TopdownSummary,
    /// The terminal block, one line each, without indentation
    pub lines: Vec<String>,
}

/// Builds the export of all runs. `runs` has one entry per run (aligned with
/// `times`) holding the topdown part of its reading; `mean_time` is the mean
/// wall time, used for the accuracy note. `None` if no run could be read.
pub fn export(
    plan: &Plan,
    runs: &[Option<Vec<Option<crate::pmu_scaling::ScaledCounter>>>],
    mean_time: f64,
) -> Option<TopdownExport> {
    let fractions: Vec<Option<Fractions>> = runs
        .iter()
        .map(|r| r.as_ref().and_then(|r| run_fractions(plan, r)))
        .collect();
    if fractions.iter().all(Option::is_none) {
        return None;
    }
    let column = |f: fn(&Fractions) -> f64| -> Vec<Option<f64>> {
        fractions.iter().map(|x| x.as_ref().map(f)).collect()
    };
    let smt = (plan.source == Source::AmdPipelineUtil)
        .then(|| column(|f| f.smt_contention.unwrap_or(0.0)));
    let series = TopdownSeries {
        level: 1,
        source: plan.source.as_str().to_string(),
        retiring: column(|f| f.retiring),
        bad_speculation: column(|f| f.bad_speculation),
        frontend_bound: column(|f| f.frontend_bound),
        backend_bound: column(|f| f.backend_bound),
        smt_contention: smt,
    };
    let summarize = |values: &[Option<f64>]| {
        let present: Vec<f64> = values.iter().flatten().copied().collect();
        MetricSummary::from_values(&present).unwrap_or_default()
    };
    let retiring = summarize(&series.retiring);
    let bad = summarize(&series.bad_speculation);
    let fe = summarize(&series.frontend_bound);
    let be = summarize(&series.backend_bound);
    let smt_summary = series.smt_contention.as_deref().map(summarize);
    let mut candidates = vec![
        ("backend_bound", be.mean),
        ("frontend_bound", fe.mean),
        ("bad_speculation", bad.mean),
    ];
    if let Some(s) = &smt_summary {
        candidates.push(("smt_contention", s.mean));
    }
    let primary = bottleneck(&candidates).unwrap_or("backend_bound");

    let present: Vec<&Fractions> = fractions.iter().flatten().collect();
    let mean_slots = present.iter().map(|f| f.slots).sum::<f64>() / present.len() as f64;
    let mut lines = Vec::new();
    let mut header = format!("Topdown (level 1, {}):", plan.source.label());
    if mean_slots < SHORT_RUN_SLOTS {
        header.push_str("  [short run, low confidence]");
    }
    lines.push(header);
    let row = |label: &str, key: &str, mean: f64| {
        let mut line = format!("  {:<17}{:>5.1}%", format!("{label}:"), mean * 100.0);
        if key == primary {
            line.push_str("   <- primary bottleneck (hypothesis)");
        }
        line
    };
    lines.push(row("Retiring", "retiring", retiring.mean));
    lines.push(row("Bad Speculation", "bad_speculation", bad.mean));
    lines.push(row("Frontend Bound", "frontend_bound", fe.mean));
    lines.push(row("Backend Bound", "backend_bound", be.mean));
    if let Some(s) = smt_summary.as_ref().filter(|s| s.mean > 0.001) {
        lines.push(row("SMT Contention", "smt_contention", s.mean));
    }
    if mean_time > LONG_RUN_SECONDS {
        lines.push("  note: topdown accuracy may decrease when measuring long periods".to_string());
    }
    Some(TopdownExport {
        series,
        summary: TopdownSummary {
            retiring,
            bad_speculation: bad,
            frontend_bound: fe,
            backend_bound: be,
            smt_contention: smt_summary,
            bottleneck: primary.to_string(),
        },
        lines,
    })
}

// ---------------------------------------------------------------------------
// Resolving against the machine

/// Number of events the active counters and topdown config add to a reading.
pub fn reading_len(
    counters: Option<&crate::perf_counters::CounterConfig>,
    topdown: Option<&TopdownConfig>,
) -> usize {
    counters.map_or(0, |c| c.active.len()) + topdown.map_or(0, TopdownConfig::event_count)
}

/// Splits a combined reading into its `--counters` part and its topdown part.
pub fn split_reading(
    reading: &HwReading,
    counters: usize,
) -> (
    &[Option<crate::pmu_scaling::ScaledCounter>],
    &[Option<crate::pmu_scaling::ScaledCounter>],
) {
    reading.split_at(counters.min(reading.len()))
}

/// Deterministic values for `--debug-mode`: 25 / 12.5 / 12.5 / 50 percent
/// (Intel) or the same split with an idle SMT sibling (AMD).
pub fn mock_reading(plan: &Plan, time_real: f64) -> HwReading {
    let cycles = (1_000_000_000.0 * time_real).round();
    let counter = |value: f64| {
        let value = value as u64;
        Some(crate::pmu_scaling::ScaledCounter {
            value,
            raw: value,
            time_enabled: 1,
            time_running: 1,
        })
    };
    match plan.source {
        Source::IntelPerfMetrics => {
            let slots = cycles * 4.0;
            vec![
                counter(slots),
                counter(slots * 0.25),
                counter(slots * 0.125),
                counter(slots * 0.125),
                counter(slots * 0.5),
            ]
        }
        Source::AmdPipelineUtil => {
            let slots = cycles * plan.width as f64;
            vec![
                counter(cycles),
                counter(slots * 0.125),
                counter(slots * 0.5),
                counter(0.0),
                counter(slots * 0.375),
                counter(slots * 0.25),
            ]
        }
    }
}

/// The plan used with `--debug-mode`.
pub fn mock_config() -> TopdownConfig {
    TopdownConfig {
        plan: Some(Plan {
            source: Source::IntelPerfMetrics,
            events: vec![
                EventSpec {
                    pmu_type: 0,
                    config: 0
                };
                5
            ],
            width: 0,
        }),
        unavailable: None,
        scope: Scope::All,
        mock: true,
    }
}

/// Resolves `--topdown` against the machine. Returns the config and the
/// warnings to print once, before the first benchmark. Never fails: an
/// unusable CPU or kernel gives an `unavailable` config with a reason.
pub fn resolve(debug_mode: bool, pinned: bool) -> (TopdownConfig, Vec<String>) {
    if debug_mode {
        return (mock_config(), vec![]);
    }
    resolve_native(pinned)
}

fn unavailable_with_warning(reason: UnavailableReason) -> (TopdownConfig, Vec<String>) {
    (
        TopdownConfig::unavailable(reason),
        vec![format!(
            "'--topdown' is not available ({}): {}; continuing without it.",
            reason.as_str(),
            reason.explanation()
        )],
    )
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn resolve_native(pinned: bool) -> (TopdownConfig, Vec<String>) {
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let Some(host) = parse_cpuinfo(&cpuinfo) else {
        return unavailable_with_warning(UnavailableReason::UnsupportedCpu);
    };
    let plan = match plan(&host, Path::new(SYSFS_PMU_ROOT), pinned) {
        Ok(plan) => plan,
        Err(reason) => return unavailable_with_warning(reason),
    };
    let events: Vec<(u32, u64)> = plan.events.iter().map(|e| (e.pmu_type, e.config)).collect();
    let mut warnings = Vec::new();
    let mut scope = Scope::All;
    let mut probe = crate::perf_counters::probe_group(&events, scope);
    // Kernel-side counting is denied at the default paranoid level 2
    if matches!(probe, Err(1 | 13)) {
        scope = Scope::User;
        warnings.push(
            "'--topdown' counts user-space slots only (kernel counting is denied by \
             perf_event_paranoid)."
                .to_string(),
        );
        probe = crate::perf_counters::probe_group(&events, scope);
    }
    match probe {
        Ok(()) => (
            TopdownConfig {
                plan: Some(plan),
                unavailable: None,
                scope,
                mock: false,
            },
            warnings,
        ),
        Err(errno) => {
            let reason = match errno {
                1 | 13 => UnavailableReason::PerfParanoid,
                2 | 19 | 95 => UnavailableReason::NoPmu,
                _ => UnavailableReason::Other,
            };
            unavailable_with_warning(reason)
        }
    }
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn resolve_native(_pinned: bool) -> (TopdownConfig, Vec<String>) {
    unavailable_with_warning(UnavailableReason::UnsupportedPlatform)
}

/// Whether the real (non-mock) `--topdown` can work on this platform at all.
pub const fn platform_supported() -> bool {
    cfg!(all(target_os = "linux", target_arch = "x86_64"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pmu_scaling::ScaledCounter;
    use std::fs;

    fn counter(value: u64) -> Option<ScaledCounter> {
        Some(ScaledCounter {
            value,
            raw: value,
            time_enabled: 10,
            time_running: 10,
        })
    }

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    // The values of the first two tests are the example of the issue
    // (retiring 24.5%, bad speculation 12.1%, frontend 14.2%, backend 49.2%)
    // scaled to counts; the Intel expression normalizes by the metric sum.
    #[test]
    fn intel_fractions_match_the_issue_example() {
        let f = intel_fractions(&[1000, 245, 121, 142, 492]).unwrap();
        close(f.retiring, 0.245);
        close(f.bad_speculation, 0.121);
        close(f.frontend_bound, 0.142);
        close(f.backend_bound, 0.492);
        assert_eq!(f.smt_contention, None);
        close(f.slots, 1000.0);
    }

    #[test]
    fn intel_fractions_are_normalized_to_one() {
        // metrics that sum to 90% of the slots (rounding in the 8-bit
        // metric register) are scaled up so the classes sum to 1
        let f = intel_fractions(&[1000, 225, 90, 135, 450]).unwrap();
        close(
            f.retiring + f.bad_speculation + f.frontend_bound + f.backend_bound,
            1.0,
        );
        close(f.backend_bound, 0.5);
    }

    #[test]
    fn intel_zero_slot_runs_are_null() {
        assert!(intel_fractions(&[0, 1, 1, 1, 1]).is_none());
        assert!(intel_fractions(&[100, 0, 0, 0, 0]).is_none());
        assert!(intel_fractions(&[100, 1, 1]).is_none());
    }

    // perf's PipelineL1 expressions with W = 6: total_dispatch_slots = 6 * cycles
    #[test]
    fn amd_fractions_follow_the_pipeline_metrics() {
        let f = amd_fractions(6, &[1000, 600, 2400, 60, 2940, 2400]).unwrap();
        close(f.frontend_bound, 0.1);
        close(f.backend_bound, 0.4);
        close(f.smt_contention.unwrap(), 0.01);
        close(f.retiring, 0.4);
        close(f.bad_speculation, 0.09); // (2940 - 2400) / 6000
                                        // the four classes sum to 1 minus the SMT contention
        close(
            f.retiring + f.bad_speculation + f.frontend_bound + f.backend_bound,
            0.99,
        );
        close(f.slots, 6000.0);
    }

    #[test]
    fn amd_width_is_8_on_zen5() {
        let f = amd_fractions(8, &[1000, 0, 0, 0, 0, 4000]).unwrap();
        close(f.retiring, 0.5);
    }

    #[test]
    fn amd_bad_speculation_is_never_negative() {
        // retired above dispatched: possible with scaled, multiplexed counts
        let f = amd_fractions(6, &[100, 0, 0, 0, 500, 520]).unwrap();
        assert_eq!(f.bad_speculation, 0.0);
        assert!(amd_fractions(6, &[0, 1, 1, 1, 1, 1]).is_none());
    }

    #[test]
    fn primary_bottleneck_is_the_largest_non_retiring_class() {
        let c = [
            ("backend_bound", 0.49),
            ("frontend_bound", 0.14),
            ("bad_speculation", 0.12),
        ];
        assert_eq!(bottleneck(&c), Some("backend_bound"));
        // ties go to the first (backend before frontend before bad speculation)
        let tie = [
            ("backend_bound", 0.2),
            ("frontend_bound", 0.2),
            ("bad_speculation", 0.2),
        ];
        assert_eq!(bottleneck(&tie), Some("backend_bound"));
        let smt = [("backend_bound", 0.1), ("smt_contention", 0.3)];
        assert_eq!(bottleneck(&smt), Some("smt_contention"));
        assert_eq!(bottleneck(&[]), None);
    }

    #[test]
    fn parses_sysfs_event_files() {
        assert_eq!(parse_event_config("event=0x00,umask=0x80\n"), Some(0x8000));
        assert_eq!(parse_event_config("event=0x00,umask=0x4"), Some(0x0400));
        assert_eq!(parse_event_config("event=0x3c,umask=0x0"), Some(0x3c));
        assert_eq!(parse_event_config("event=0x3c,umask=0x0,any=1"), None);
        assert_eq!(parse_event_config("cpu/event=1/"), None);
        assert_eq!(parse_event_config(""), None);
    }

    #[test]
    fn amd_event_words_carry_the_high_nibble() {
        assert_eq!(amd_config(0x1a0, 0x01), 0x1_0000_01a0);
        assert_eq!(amd_config(0x1a0, 0x1e), 0x1_0000_1ea0);
        assert_eq!(amd_config(0x76, 0), 0x76);
        assert_eq!(amd_config(0xaa, 0x07), 0x07aa);
    }

    fn cpuinfo(vendor: &str, family: u32, model: u32, flags: &str) -> String {
        format!(
            "processor\t: 0\nvendor_id\t: {vendor}\ncpu family\t: {family}\nmodel\t\t: {model}\n\
             model name\t: Some CPU\nflags\t\t: fpu {flags}\n\nprocessor\t: 1\nmodel\t\t: 1\n"
        )
    }

    #[test]
    fn parses_cpuinfo() {
        let host = parse_cpuinfo(&cpuinfo("GenuineIntel", 6, 143, "sse hypervisor")).unwrap();
        assert_eq!(
            host,
            HostCpu {
                vendor: "GenuineIntel".into(),
                family: 6,
                model: 143,
                hypervisor: true
            }
        );
        assert!(
            !parse_cpuinfo(&cpuinfo("AuthenticAMD", 25, 17, "sse"))
                .unwrap()
                .hypervisor
        );
        assert!(parse_cpuinfo("processor : 0\n").is_none());
    }

    struct Sysfs(std::path::PathBuf);

    impl Sysfs {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("perfratio-topdown-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Sysfs(dir)
        }

        fn file(&self, relative: &str, content: &str) {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }

        fn intel(&self, pmu: &str, ty: u32) {
            self.file(&format!("{pmu}/type"), &format!("{ty}\n"));
            for (name, text) in [
                ("slots", "event=0x00,umask=0x4"),
                ("topdown-retiring", "event=0x00,umask=0x80"),
                ("topdown-bad-spec", "event=0x00,umask=0x81"),
                ("topdown-fe-bound", "event=0x00,umask=0x82"),
                ("topdown-be-bound", "event=0x00,umask=0x83"),
            ] {
                self.file(&format!("{pmu}/events/{name}"), text);
            }
        }
    }

    impl Drop for Sysfs {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn host(vendor: &str, family: u32, model: u32, hypervisor: bool) -> HostCpu {
        HostCpu {
            vendor: vendor.into(),
            family,
            model,
            hypervisor,
        }
    }

    #[test]
    fn intel_events_come_from_sysfs() {
        let sys = Sysfs::new("intel");
        sys.intel("cpu", 4);
        let p = plan(&host("GenuineIntel", 6, 0x8F, false), &sys.0, false).unwrap();
        assert_eq!(p.source, Source::IntelPerfMetrics);
        let configs: Vec<u64> = p.events.iter().map(|e| e.config).collect();
        assert_eq!(configs, [0x0400, 0x8000, 0x8100, 0x8200, 0x8300]);
        assert!(p.events.iter().all(|e| e.pmu_type == 4));
    }

    #[test]
    fn intel_without_slots_distinguishes_kernel_cpu_and_vm() {
        let sys = Sysfs::new("noslots");
        sys.file("cpu/type", "4\n");
        let reason =
            |model, vm| plan(&host("GenuineIntel", 6, model, vm), &sys.0, false).unwrap_err();
        // Sapphire Rapids has perf_metrics, so the kernel lacks it
        assert_eq!(reason(0x8F, false), UnavailableReason::UnsupportedKernel);
        // Skylake has no perf_metrics register
        assert_eq!(reason(0x55, false), UnavailableReason::UnsupportedCpu);
        // a guest without `slots` is reported as no PMU
        assert_eq!(reason(0x8F, true), UnavailableReason::NoPmu);
    }

    #[test]
    fn intel_without_any_pmu_directory() {
        let sys = Sysfs::new("nopmu");
        assert_eq!(
            plan(&host("GenuineIntel", 6, 0x8F, true), &sys.0, false).unwrap_err(),
            UnavailableReason::NoPmu
        );
        sys.file("cpu_atom/type", "10\n");
        assert_eq!(
            plan(&host("GenuineIntel", 6, 0xBE, false), &sys.0, true).unwrap_err(),
            UnavailableReason::UnsupportedCpu
        );
    }

    #[test]
    fn hybrid_intel_needs_pinning_and_uses_cpu_core() {
        let sys = Sysfs::new("hybrid");
        sys.intel("cpu_core", 10);
        sys.file("cpu_atom/type", "11\n");
        let h = host("GenuineIntel", 6, 0x97, false);
        assert_eq!(
            plan(&h, &sys.0, false).unwrap_err(),
            UnavailableReason::HybridCpuUnpinned
        );
        let p = plan(&h, &sys.0, true).unwrap();
        assert!(p.events.iter().all(|e| e.pmu_type == 10));
    }

    #[test]
    fn amd_zen4_and_zen5_are_selected_by_model() {
        let sys = Sysfs::new("amd");
        sys.file("cpu/type", "4\n");
        let zen4 = plan(&host("AuthenticAMD", 0x19, 0x11, false), &sys.0, false).unwrap();
        assert_eq!((zen4.source, zen4.width), (Source::AmdPipelineUtil, 6));
        assert_eq!(zen4.events.len(), 6);
        assert_eq!(zen4.events[1].config, 0x1_0000_01a0);
        assert_eq!(zen4.events[5].config, 0xc1);
        let zen5 = plan(&host("AuthenticAMD", 0x1A, 0x44, false), &sys.0, false).unwrap();
        assert_eq!(zen5.width, 8);
        // Zen 1 to Zen 3 and unknown vendors have no equivalent events
        for (family, model) in [
            (0x17, 0x31),
            (0x19, 0x21),
            (0x19, 0x01),
            (0x19, 0x30),
            (0x1A, 0xA0),
        ] {
            assert_eq!(
                plan(&host("AuthenticAMD", family, model, false), &sys.0, false).unwrap_err(),
                UnavailableReason::UnsupportedCpu,
                "{family:#x} {model:#x}"
            );
        }
        assert_eq!(
            plan(&host("HygonGenuine", 24, 0, false), &sys.0, false).unwrap_err(),
            UnavailableReason::UnsupportedCpu
        );
        let empty = Sysfs::new("amd-empty");
        assert_eq!(
            plan(&host("AuthenticAMD", 0x19, 0x11, true), &empty.0, false).unwrap_err(),
            UnavailableReason::NoPmu
        );
    }

    fn intel_plan_fixture() -> Plan {
        Plan {
            source: Source::IntelPerfMetrics,
            events: vec![
                EventSpec {
                    pmu_type: 4,
                    config: 0
                };
                5
            ],
            width: 0,
        }
    }

    #[test]
    fn export_aligns_runs_and_marks_the_bottleneck() {
        let plan = intel_plan_fixture();
        let run = |slots: u64, m: [u64; 4]| {
            Some(vec![
                counter(slots),
                counter(m[0]),
                counter(m[1]),
                counter(m[2]),
                counter(m[3]),
            ])
        };
        let runs = vec![
            run(2_000_000, [245, 121, 142, 492]),
            None,
            run(2_000_000, [255, 121, 132, 492]),
        ];
        let e = export(&plan, &runs, 0.01).unwrap();
        assert_eq!(e.series.level, 1);
        assert_eq!(e.series.source, "intel_perf_metrics");
        assert_eq!(e.series.retiring.len(), 3);
        assert!(e.series.retiring[1].is_none());
        assert!(e.series.smt_contention.is_none());
        close(e.summary.retiring.mean, 0.25);
        assert_eq!(e.summary.bottleneck, "backend_bound");
        assert_eq!(e.lines[0], "Topdown (level 1, Intel perf_metrics):");
        assert!(
            e.lines[1].contains("Retiring:") && e.lines[1].contains("25.0%"),
            "{:?}",
            e.lines
        );
        assert!(!e.lines[1].contains("<-"));
        assert!(
            e.lines[4].contains("Backend Bound:")
                && e.lines[4].contains("<- primary bottleneck (hypothesis)")
        );
        assert_eq!(e.lines.len(), 5);
        // all runs unreadable: no export
        assert!(export(&plan, &[None, None], 0.01).is_none());
        assert!(export(&plan, &[], 0.01).is_none());
    }

    #[test]
    fn short_and_long_runs_are_annotated() {
        let plan = intel_plan_fixture();
        let run = |slots| {
            Some(vec![
                counter(slots),
                counter(1),
                counter(1),
                counter(1),
                counter(1),
            ])
        };
        let short = export(&plan, &[run(1000)], 6.0).unwrap();
        assert!(
            short.lines[0].ends_with("[short run, low confidence]"),
            "{:?}",
            short.lines
        );
        assert!(short.lines.last().unwrap().contains("long periods"));
        let ok = export(&plan, &[run(5_000_000)], 0.1).unwrap();
        assert!(!ok.lines[0].contains("short run"));
        assert!(!ok.lines.iter().any(|l| l.contains("long periods")));
    }

    #[test]
    fn amd_export_has_smt_contention() {
        let plan = Plan {
            source: Source::AmdPipelineUtil,
            events: vec![
                EventSpec {
                    pmu_type: 4,
                    config: 0
                };
                6
            ],
            width: 6,
        };
        let reading = vec![
            counter(1_000_000),
            counter(600_000),
            counter(2_400_000),
            counter(600_000),
            counter(3_000_000),
            counter(2_400_000),
        ];
        let e = export(&plan, &[Some(reading)], 0.1).unwrap();
        assert_eq!(e.series.source, "amd_pipeline_util");
        close(e.series.smt_contention.as_ref().unwrap()[0].unwrap(), 0.1);
        assert!(e.lines.iter().any(|l| l.contains("SMT Contention")));
        assert_eq!(e.summary.bottleneck, "backend_bound");
    }

    #[test]
    fn a_counter_that_never_ran_makes_the_run_null() {
        let plan = intel_plan_fixture();
        let never = Some(ScaledCounter {
            value: 0,
            raw: 0,
            time_enabled: 10,
            time_running: 0,
        });
        let reading = vec![counter(100), never, counter(1), counter(1), counter(1)];
        assert!(run_fractions(&plan, &reading).is_none());
    }

    #[test]
    fn json_round_trips_and_omits_absent_keys() {
        let plan = intel_plan_fixture();
        let reading = Some(vec![
            counter(2_000_000),
            counter(1),
            counter(1),
            counter(1),
            counter(1),
        ]);
        let e = export(&plan, &[reading], 0.1).unwrap();
        let json = serde_json::to_value(&e.series).unwrap();
        assert!(json.get("smt_contention").is_none());
        assert_eq!(json["level"], 1);
        let back: TopdownSeries = serde_json::from_value(json).unwrap();
        assert_eq!(back, e.series);
        let summary = serde_json::to_value(&e.summary).unwrap();
        assert_eq!(summary["bottleneck"], "backend_bound");
        assert_eq!(
            serde_json::from_value::<TopdownSummary>(summary).unwrap(),
            e.summary
        );
    }

    #[test]
    fn mock_values_give_fixed_fractions() {
        let plan = mock_config().plan.unwrap();
        let f = run_fractions(&plan, &mock_reading(&plan, 0.5)).unwrap();
        close(f.backend_bound, 0.5);
        close(f.retiring, 0.25);
        let amd = Plan {
            source: Source::AmdPipelineUtil,
            events: vec![
                EventSpec {
                    pmu_type: 4,
                    config: 0
                };
                6
            ],
            width: 6,
        };
        let f = run_fractions(&amd, &mock_reading(&amd, 0.5)).unwrap();
        close(f.backend_bound, 0.5);
        close(f.bad_speculation, 0.125);
    }

    #[test]
    fn reading_is_split_after_the_counters() {
        let reading: HwReading = vec![counter(1), counter(2), counter(3)];
        let (a, b) = split_reading(&reading, 2);
        assert_eq!((a.len(), b.len()), (2, 1));
        let (a, b) = split_reading(&reading, 9);
        assert_eq!((a.len(), b.len()), (3, 0));
    }

    #[test]
    fn unavailable_reasons_have_stable_names() {
        use UnavailableReason::*;
        let names: Vec<&str> = [
            UnsupportedPlatform,
            UnsupportedCpu,
            UnsupportedKernel,
            PerfParanoid,
            NoPmu,
            HybridCpuUnpinned,
            Other,
        ]
        .iter()
        .map(|r| r.as_str())
        .collect();
        assert_eq!(
            names,
            [
                "unsupported_platform",
                "unsupported_cpu",
                "unsupported_kernel",
                "perf_paranoid",
                "no_pmu",
                "hybrid_cpu_unpinned",
                "other"
            ]
        );
    }

    #[test]
    fn unavailable_config_warns_once_with_the_reason() {
        let (config, warnings) = unavailable_with_warning(UnavailableReason::HybridCpuUnpinned);
        assert!(!config.is_active());
        assert_eq!(config.event_count(), 0);
        assert_eq!(
            config.unavailable,
            Some(UnavailableReason::HybridCpuUnpinned)
        );
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("hybrid_cpu_unpinned") && warnings[0].contains("--affinity"));
        assert!(warnings[0].contains("continuing without it"));
    }
}
