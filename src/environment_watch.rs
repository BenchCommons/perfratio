//! Observes the environment while a benchmark runs: cgroup CPU throttling and
//! CPU temperature. The result is `interference` in the JSON and feeds the
//! `environment_stability` quality check. See `docs/specs/cgroup-thermal.md`.
//!
//! Diagnostics only: nothing here changes a statistic. A source that cannot be
//! read is left out, never reported as "no throttling".

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::cgroup::{Cgroup, Snapshot};
use crate::thermal::{ThermalMonitor, ThermalTrace};

/// `interference` of a result
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Interference {
    /// Time the cgroup was throttled by its CPU quota during the runs,
    /// microseconds (the cgroup's counter, so it includes other processes in it)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cgroup_throttled_usec: Option<u64>,
    /// Runs during which the throttled time grew
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cgroup_throttled_runs: Option<usize>,
    /// Processes the out-of-memory killer killed in the cgroup during the runs
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cgroup_oom_kills: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thermal: Option<ThermalTrace>,
}

/// State of one benchmark's observation
#[derive(Debug, Default)]
pub struct Watch {
    cgroup: Option<Cgroup>,
    throttled_usec: u64,
    throttled_runs: usize,
    oom_kills: Option<u64>,
    thermal: Option<BackgroundThermal>,
}

impl Watch {
    /// Observe nothing (the mock executor of `--debug-mode`)
    pub fn disabled() -> Self {
        Watch::default()
    }

    /// Observe the host below `root`
    pub fn new(root: &Path) -> Self {
        let cgroup = if std::env::var_os("PFDIAG_NO_CGROUP").is_some() { None } else { crate::cgroup::locate(root).filter(|c| c.snapshot().is_some()) };
        Watch {
            cgroup,
            throttled_usec: 0,
            throttled_runs: 0,
            oom_kills: None,
            thermal: if std::env::var_os("PFDIAG_NO_THERMAL").is_some() { None } else { Some(BackgroundThermal::start(root)) },
        }
    }

    /// Before a run, outside its timed window
    pub fn begin(&self) -> Option<Snapshot> {
        self.cgroup.as_ref()?.snapshot()
    }

    /// After a run, with what [`begin`](Self::begin) returned
    pub fn end(&mut self, before: Option<Snapshot>) {
        let (Some(before), Some(cgroup)) = (before, self.cgroup.as_ref()) else {
            return;
        };
        let Some(after) = cgroup.snapshot() else {
            return;
        };
        let grown = after.since(&before);
        self.throttled_usec += grown.throttled_usec;
        if grown.throttled_usec > 0 || grown.nr_throttled > 0 {
            self.throttled_runs += 1;
        }
        if let Some(kills) = grown.oom_kills {
            *self.oom_kills.get_or_insert(0) += kills;
        }
    }

    /// The observations; `None` when nothing could be observed
    pub fn finish(self) -> Option<Interference> {
        let observed_cgroup = self.cgroup.is_some();
        let interference = Interference {
            cgroup_throttled_usec: observed_cgroup.then_some(self.throttled_usec),
            cgroup_throttled_runs: observed_cgroup.then_some(self.throttled_runs),
            cgroup_oom_kills: self.oom_kills,
            thermal: self.thermal.and_then(BackgroundThermal::finish),
        };
        (interference != Interference::default()).then_some(interference)
    }
}

/// How often the background thread reads the temperature sensors
const BACKGROUND_INTERVAL: Duration = Duration::from_secs(1);

/// Longest wait in `finish` for a first reading that is still in progress
const FIRST_READ_WAIT: Duration = Duration::from_millis(200);

/// Reads the temperature on its own thread. Some hosts (cloud VMs) take about
/// a second per read of `/sys/class/thermal` and `/sys/class/hwmon`; reading
/// them on the benchmark thread delayed the first run by that much.
#[derive(Debug)]
struct BackgroundThermal {
    stop: Arc<AtomicBool>,
    latest: Arc<Mutex<Option<ThermalTrace>>>,
    /// Wait in `finish` for a first reading: only for a fixture root (tests),
    /// whose reads are instant; a real host never waits for its sensors.
    wait_first_read: bool,
}

impl BackgroundThermal {
    fn start(root: &Path) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let latest = Arc::new(Mutex::new(None));
        let (thread_stop, thread_latest) = (Arc::clone(&stop), Arc::clone(&latest));
        let root_is_fixture = root != Path::new("/");
        let root = root.to_path_buf();
        let spawned = std::thread::Builder::new()
            .name("perfratio-thermal".into())
            .spawn(move || {
                let Some(mut monitor) = ThermalMonitor::start(&root) else {
                    return;
                };
                loop {
                    if let Ok(mut slot) = thread_latest.lock() {
                        *slot = Some(monitor.trace());
                    }
                    // sleep in short steps so the thread ends soon after `finish`
                    let mut slept = Duration::ZERO;
                    while slept < BACKGROUND_INTERVAL {
                        if thread_stop.load(Ordering::Relaxed) {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(20));
                        slept += Duration::from_millis(20);
                    }
                    monitor.sample();
                }
            });
        if spawned.is_err() {
            stop.store(true, Ordering::Relaxed);
        }
        let wait_first_read = root_is_fixture;
        BackgroundThermal {
            stop,
            latest,
            wait_first_read,
        }
    }

    /// The last trace the thread recorded; never waits for a sensor read.
    /// `None` when no sensor could be read (yet).
    fn finish(self) -> Option<ThermalTrace> {
        // A benchmark shorter than the first read (e.g. `--debug-mode`) waits
        // briefly for it; this is after the last run, outside every timing.
        let wait = if self.wait_first_read {
            FIRST_READ_WAIT
        } else {
            Duration::ZERO
        };
        let deadline = std::time::Instant::now() + wait;
        let trace = loop {
            let trace = self.latest.lock().ok().and_then(|slot| *slot);
            if trace.is_some() || std::time::Instant::now() >= deadline {
                break trace;
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        self.stop.store(true, Ordering::Relaxed);
        trace
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(root: &Path, path: &str, content: &str) {
        let full = root.join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, content).unwrap();
    }

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(r, "proc/self/cgroup", "0::/job\n");
        write(r, "sys/fs/cgroup/cgroup.controllers", "cpu\n");
        write(r, "sys/fs/cgroup/job/cpu.max", "20000 100000\n");
        stat(r, 0, 0);
        write(r, "sys/class/thermal/thermal_zone0/temp", "50000\n");
        dir
    }

    fn stat(root: &Path, nr: u64, usec: u64) {
        write(
            root,
            "sys/fs/cgroup/job/cpu.stat",
            &format!("nr_throttled {nr}\nthrottled_usec {usec}\n"),
        );
    }

    #[test]
    fn counts_the_throttled_time_and_runs() {
        let dir = fixture();
        let r = dir.path();
        let mut watch = Watch::new(r);
        for (nr, usec) in [(0, 0), (3, 4000), (3, 4000), (5, 9000)] {
            let before = watch.begin();
            stat(r, nr, usec);
            watch.end(before);
        }
        let i = watch.finish().unwrap();
        // run 1: 0 -> 0, run 2: 0 -> 4000, run 3: no change, run 4: 4000 -> 9000
        assert_eq!(i.cgroup_throttled_usec, Some(9000));
        assert_eq!(i.cgroup_throttled_runs, Some(2));
        assert_eq!(i.cgroup_oom_kills, None);
        assert_eq!(i.thermal.unwrap().start_c, 50.0);
    }

    #[test]
    fn a_quiet_cgroup_reports_zero_because_it_was_read() {
        let dir = fixture();
        let mut watch = Watch::new(dir.path());
        let before = watch.begin();
        watch.end(before);
        let i = watch.finish().unwrap();
        assert_eq!(i.cgroup_throttled_usec, Some(0));
        assert_eq!(i.cgroup_throttled_runs, Some(0));
    }

    #[test]
    fn an_unreadable_cgroup_is_omitted_not_zero() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(r, "proc/self/cgroup", "0::/\n");
        write(r, "sys/fs/cgroup/cgroup.controllers", "memory\n");
        // no throttle counters at all
        write(r, "sys/fs/cgroup/cpu.stat", "usage_usec 1\n");
        let mut watch = Watch::new(r);
        let before = watch.begin();
        watch.end(before);
        assert_eq!(watch.finish(), None);
    }

    #[test]
    fn a_disabled_watch_and_an_empty_host_observe_nothing() {
        let mut watch = Watch::disabled();
        let before = watch.begin();
        watch.end(before);
        assert_eq!(watch.finish(), None);
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Watch::new(dir.path()).finish(), None);
    }

    #[test]
    fn serializes_only_what_was_observed() {
        let i = Interference {
            cgroup_throttled_usec: Some(5),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_string(&i).unwrap(),
            r#"{"cgroup_throttled_usec":5}"#
        );
    }
}
