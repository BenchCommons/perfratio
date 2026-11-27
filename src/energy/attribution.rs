//! Estimated energy attribution (`--energy-attribution`, `docs/specs/energy-attribution-and-power-meter.md`, #381).
//!
//! RAPL and the other sensors measure a package or a machine, not a process. When other work
//! shares the host, part of the measured energy is not the benchmark's. The models split the
//! measured energy of a run window by a CPU-time share, as Scaphandre and Kepler do:
//!
//! `estimated = measured window energy x share`
//!
//! - `cpu-time`: `share = CPU time of the command (user + system) / busy CPU time of the host`
//! - `cgroup`: `share = CPU time of the own cgroup / busy CPU time of the host`
//!
//! The result is an estimate and is exported with `kind: "estimated"`. The measured energy
//! stays the primary value. Idle power is part of the measured energy and so of the share.

use serde::{Deserialize, Serialize};

/// Host CPU windows shorter than this are too coarse: `/proc/stat` counts 10 ms ticks.
pub const COARSE_HOST_SECONDS: f64 = 0.05;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Model {
    /// CPU time of the command over the busy CPU time of the host
    CpuTime,
    /// CPU time of the own cgroup (container) over the busy CPU time of the host
    Cgroup,
}

impl Model {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "cpu-time" => Some(Self::CpuTime),
            "cgroup" => Some(Self::Cgroup),
            _ => None,
        }
    }

    /// `energy_attribution.model` in the JSON export (open enum)
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CpuTime => "cpu-time-share",
            Self::Cgroup => "cgroup-cpu-share",
        }
    }

    /// `energy_attribution.scope` in the JSON export (open enum)
    pub fn scope(self) -> &'static str {
        match self {
            Self::CpuTime => "process",
            Self::Cgroup => "cgroup",
        }
    }
}

/// Cumulative CPU time counters, in seconds. A field is `None` when it cannot be read.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct HostSnapshot {
    /// Busy CPU time of all CPUs of the host (user, nice, system, irq, softirq, steal)
    pub host_busy: Option<f64>,
    /// CPU time of the cgroup of this process, descendants included
    pub cgroup_cpu: Option<f64>,
}

pub trait HostCpu: Send + Sync {
    fn snapshot(&self) -> HostSnapshot;
}

/// Busy seconds from the `cpu` line of `/proc/stat` (`user nice system idle iowait irq
/// softirq steal guest guest_nice`, in ticks). `guest` is already part of `user`.
pub fn parse_proc_stat(text: &str, ticks_per_second: f64) -> Option<f64> {
    let line = text.lines().find(|l| l.starts_with("cpu "))?;
    let f: Vec<f64> = line
        .split_whitespace()
        .skip(1)
        .map(|v| v.parse::<f64>())
        .collect::<Result<_, _>>()
        .ok()?;
    if f.len() < 3 || ticks_per_second <= 0.0 {
        return None;
    }
    let get = |i: usize| f.get(i).copied().unwrap_or(0.0);
    let ticks = get(0) + get(1) + get(2) + get(5) + get(6) + get(7);
    Some(ticks / ticks_per_second)
}

/// Reads `/proc/stat` and the cgroup files below `root` (`/` in production, a fixture in tests).
pub struct LinuxHostCpu {
    root: std::path::PathBuf,
    cgroup: Option<crate::cgroup::Cgroup>,
    ticks: f64,
}

impl LinuxHostCpu {
    pub fn new() -> Self {
        Self::at("/", user_hz())
    }

    pub fn at(root: impl Into<std::path::PathBuf>, ticks: f64) -> Self {
        let root = root.into();
        let cgroup = crate::cgroup::locate(&root);
        Self {
            root,
            cgroup,
            ticks,
        }
    }
}

impl Default for LinuxHostCpu {
    fn default() -> Self {
        Self::new()
    }
}

impl HostCpu for LinuxHostCpu {
    fn snapshot(&self) -> HostSnapshot {
        HostSnapshot {
            host_busy: std::fs::read_to_string(self.root.join("proc/stat"))
                .ok()
                .and_then(|t| parse_proc_stat(&t, self.ticks)),
            cgroup_cpu: self.cgroup.as_ref().and_then(|c| c.cpu_usage_seconds()),
        }
    }
}

#[cfg(target_os = "linux")]
fn user_hz() -> f64 {
    // SAFETY: sysconf has no preconditions
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if hz > 0 {
        hz as f64
    } else {
        100.0
    }
}

#[cfg(not(target_os = "linux"))]
fn user_hz() -> f64 {
    100.0
}

/// Host source used by `--debug-mode`: every snapshot adds 1 s of host busy time and
/// 0.5 s of cgroup time, so the shares are exact in tests.
#[derive(Default)]
pub struct MockHostCpu {
    reads: std::sync::atomic::AtomicU32,
}

impl HostCpu for MockHostCpu {
    fn snapshot(&self) -> HostSnapshot {
        let n = f64::from(
            self.reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                + 1,
        );
        HostSnapshot {
            host_busy: Some(n),
            cgroup_cpu: Some(n * 0.5),
        }
    }
}

/// The share of the host energy attributed to the subject, and whether it was clamped to 1.
/// `None` when the host did no measurable work or an input is missing or negative.
pub fn share(
    model: Model,
    process_cpu: f64,
    host_busy_delta: Option<f64>,
    cgroup_delta: Option<f64>,
) -> Option<(f64, bool)> {
    let host = host_busy_delta.filter(|h| h.is_finite() && *h > 0.0)?;
    let numerator = match model {
        Model::CpuTime => Some(process_cpu),
        Model::Cgroup => cgroup_delta,
    }
    .filter(|n| n.is_finite() && *n >= 0.0)?;
    let raw = numerator / host;
    // Tick quantization and other CPUs' clocks can make the numerator exceed the host time
    Some((raw.min(1.0), raw > 1.0))
}

/// Estimated energy of one run
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Estimate {
    pub joules: f64,
    pub share: f64,
    pub clamped: bool,
    /// The host CPU window was shorter than [`COARSE_HOST_SECONDS`]
    pub coarse: bool,
}

/// Per-run attribution state of one benchmark
pub struct Tracker {
    model: Model,
    source: Box<dyn HostCpu>,
    start: Option<HostSnapshot>,
}

impl Tracker {
    pub fn new(model: Model, source: Box<dyn HostCpu>) -> Self {
        Self {
            model,
            source,
            start: None,
        }
    }

    pub fn model(&self) -> Model {
        self.model
    }

    /// Whether the counters the model needs can be read at all
    pub fn is_supported(&self) -> bool {
        let s = self.source.snapshot();
        s.host_busy.is_some() && (self.model == Model::CpuTime || s.cgroup_cpu.is_some())
    }

    /// Call right where the energy window opens
    pub fn begin(&mut self) {
        self.start = Some(self.source.snapshot());
    }

    /// Call right after the energy window closes. `window_energy` is the energy of the whole
    /// window as the sampler returned it, `process_cpu` the user + system time of the command.
    pub fn end(&mut self, window_energy: Option<f64>, process_cpu: f64) -> Option<Estimate> {
        let before = self.start.take()?;
        let after = self.source.snapshot();
        let energy = window_energy?;
        let delta = |a: Option<f64>, b: Option<f64>| Some((a? - b?).max(0.0));
        let host = delta(after.host_busy, before.host_busy);
        let (share, clamped) = share(
            self.model,
            process_cpu,
            host,
            delta(after.cgroup_cpu, before.cgroup_cpu),
        )?;
        Some(Estimate {
            joules: energy * share,
            share,
            clamped,
            coarse: host.is_some_and(|h| h < COARSE_HOST_SECONDS),
        })
    }
}

/// `results[].energy_attribution` of the JSON export (new in 1.37)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnergyAttribution {
    /// Always `estimated`: the values are a model output, not a measurement
    pub kind: String,
    pub model: String,
    pub scope: String,
    /// Runs with an estimate
    pub samples: usize,
    /// Runs whose share was clamped to 1
    pub clamped_runs: usize,
    pub mean_share: f64,
    pub mean_joules: f64,
    /// One value per run; only when every run has an estimate
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub joules: Option<Vec<f64>>,
}

impl EnergyAttribution {
    /// Summary of the per-run estimates, `None` when no run has one
    pub fn from_runs(model: Model, runs: &[Option<Estimate>]) -> Option<Self> {
        let have: Vec<&Estimate> = runs.iter().flatten().collect();
        if have.is_empty() {
            return None;
        }
        let n = have.len() as f64;
        Some(Self {
            kind: "estimated".into(),
            model: model.as_str().into(),
            scope: model.scope().into(),
            samples: have.len(),
            clamped_runs: have.iter().filter(|e| e.clamped).count(),
            mean_share: have.iter().map(|e| e.share).sum::<f64>() / n,
            mean_joules: have.iter().map(|e| e.joules).sum::<f64>() / n,
            joules: (have.len() == runs.len()).then(|| have.iter().map(|e| e.joules).collect()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROC_STAT: &str = include_str!("../../tests/fixtures/proc_stat_busy.txt");

    #[test]
    fn proc_stat_busy_time_excludes_idle_iowait_and_guest() {
        // user 4000 nice 100 system 900 idle 90000 iowait 500 irq 20 softirq 80 steal 400
        // guest 300 guest_nice 0 -> busy = 4000+100+900+20+80+400 = 5500 ticks
        assert_eq!(parse_proc_stat(PROC_STAT, 100.0), Some(55.0));
        assert_eq!(parse_proc_stat(PROC_STAT, 250.0), Some(22.0));
    }

    #[test]
    fn proc_stat_accepts_old_kernels_and_rejects_garbage() {
        assert_eq!(parse_proc_stat("cpu 100 0 50 1000\n", 100.0), Some(1.5));
        assert_eq!(parse_proc_stat("cpu0 1 2 3 4\n", 100.0), None);
        assert_eq!(parse_proc_stat("cpu 1 x 3 4\n", 100.0), None);
        assert_eq!(parse_proc_stat("cpu 1 2\n", 100.0), None);
        assert_eq!(parse_proc_stat("", 100.0), None);
        assert_eq!(parse_proc_stat(PROC_STAT, 0.0), None);
    }

    #[test]
    fn cpu_time_share_is_process_over_host_busy_time() {
        assert_eq!(
            share(Model::CpuTime, 0.25, Some(1.0), None),
            Some((0.25, false))
        );
        // clamped to 1 when the process time exceeds the host window
        assert_eq!(
            share(Model::CpuTime, 1.5, Some(1.0), None),
            Some((1.0, true))
        );
        assert_eq!(
            share(Model::CpuTime, 0.0, Some(1.0), None),
            Some((0.0, false))
        );
    }

    #[test]
    fn cgroup_share_uses_the_cgroup_time_not_the_process_time() {
        assert_eq!(
            share(Model::Cgroup, 0.25, Some(2.0), Some(0.5)),
            Some((0.25, false))
        );
        assert_eq!(share(Model::Cgroup, 0.25, Some(2.0), None), None);
    }

    #[test]
    fn shares_need_a_busy_host_and_sane_inputs() {
        assert_eq!(share(Model::CpuTime, 0.1, Some(0.0), None), None);
        assert_eq!(share(Model::CpuTime, 0.1, None, None), None);
        assert_eq!(share(Model::CpuTime, -0.1, Some(1.0), None), None);
        assert_eq!(share(Model::CpuTime, f64::NAN, Some(1.0), None), None);
    }

    struct Scripted(std::sync::Mutex<Vec<HostSnapshot>>);
    impl HostCpu for Scripted {
        fn snapshot(&self) -> HostSnapshot {
            self.0.lock().unwrap().remove(0)
        }
    }

    fn snap(host: f64, cgroup: f64) -> HostSnapshot {
        HostSnapshot {
            host_busy: Some(host),
            cgroup_cpu: Some(cgroup),
        }
    }

    #[test]
    fn the_tracker_scales_the_window_energy_by_the_share() {
        let source = Scripted(std::sync::Mutex::new(vec![
            snap(10.0, 3.0),
            snap(14.0, 4.0),
        ]));
        let mut tracker = Tracker::new(Model::CpuTime, Box::new(source));
        tracker.begin();
        let e = tracker.end(Some(8.0), 1.0).unwrap();
        // host busy grew by 4 s, the command used 1 s: share 0.25
        assert_eq!(e.share, 0.25);
        assert_eq!(e.joules, 2.0);
        assert!(!e.clamped && !e.coarse);
    }

    #[test]
    fn the_container_model_and_coarse_windows() {
        let source = Scripted(std::sync::Mutex::new(vec![
            snap(10.0, 3.0),
            snap(10.04, 3.02),
        ]));
        let mut tracker = Tracker::new(Model::Cgroup, Box::new(source));
        tracker.begin();
        let e = tracker.end(Some(1.0), 0.0).unwrap();
        assert!((e.share - 0.5).abs() < 1e-9);
        assert!(e.coarse);
    }

    #[test]
    fn the_tracker_gives_nothing_without_energy_or_a_start() {
        let source = Scripted(std::sync::Mutex::new(vec![snap(1.0, 1.0), snap(2.0, 2.0)]));
        let mut tracker = Tracker::new(Model::CpuTime, Box::new(source));
        // end() without begin()
        assert_eq!(tracker.end(Some(1.0), 0.1), None);
        tracker.begin();
        assert_eq!(tracker.end(None, 0.1), None);
    }

    #[test]
    fn the_summary_is_labelled_estimated() {
        let run = |j, s, clamped| {
            Some(Estimate {
                joules: j,
                share: s,
                clamped,
                coarse: false,
            })
        };
        let a = EnergyAttribution::from_runs(
            Model::CpuTime,
            &[run(1.0, 0.5, false), run(3.0, 1.0, true)],
        )
        .unwrap();
        assert_eq!(a.kind, "estimated");
        assert_eq!(
            (a.model.as_str(), a.scope.as_str()),
            ("cpu-time-share", "process")
        );
        assert_eq!((a.samples, a.clamped_runs), (2, 1));
        assert_eq!((a.mean_joules, a.mean_share), (2.0, 0.75));
        assert_eq!(a.joules, Some(vec![1.0, 3.0]));

        let partial =
            EnergyAttribution::from_runs(Model::Cgroup, &[run(1.0, 0.5, false), None]).unwrap();
        assert_eq!(partial.joules, None);
        assert_eq!(partial.samples, 1);
        assert_eq!(partial.scope, "cgroup");
        assert_eq!(EnergyAttribution::from_runs(Model::Cgroup, &[None]), None);
    }

    #[test]
    fn the_linux_source_reads_a_fixture_tree() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        let write = |p: &str, c: &str| {
            let f = r.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, c).unwrap();
        };
        write("proc/stat", PROC_STAT);
        write("proc/self/cgroup", "0::/ctr\n");
        write("sys/fs/cgroup/cgroup.controllers", "cpu\n");
        write("sys/fs/cgroup/ctr/cpu.stat", "usage_usec 2500000\n");
        let source = LinuxHostCpu::at(r, 100.0);
        assert_eq!(
            source.snapshot(),
            HostSnapshot {
                host_busy: Some(55.0),
                cgroup_cpu: Some(2.5)
            }
        );
        let tracker = Tracker::new(Model::Cgroup, Box::new(source));
        assert!(tracker.is_supported());

        // no /proc/stat, no cgroup: unsupported, never a zero
        let empty = tempfile::tempdir().unwrap();
        let tracker = Tracker::new(
            Model::CpuTime,
            Box::new(LinuxHostCpu::at(empty.path(), 100.0)),
        );
        assert!(!tracker.is_supported());
    }
}
