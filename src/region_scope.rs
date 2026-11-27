//! Region-scoped CPU time, energy and hardware counters (`--region-scope`),
//! see `docs/specs/region-scope.md`.
//!
//! With `PERFRATIO_REGION_LIVE=1` the region SDKs write every marker to the event
//! file at once. A watcher thread of perfratio polls that file and, for every
//! `start`/`stop` line it finds, takes a [`Sample`] of the child: CPU time
//! (`/proc/<pid>/stat`, `proc_pidinfo`, `GetProcessTimes`), the energy of an
//! [`EnergySampler`] and the counters opened on the child. The value of a region
//! is the sum of `sample(stop) - sample(start)` over its pairs.
//!
//! The sample is taken after the SDK wrote the line, so it lags the marker by the
//! write-to-poll delay `D` (see `docs/specs/region-scope.md` for its bound). The
//! pure parts (line scan, pairing, unit conversions) take injected readings.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Set to `1` by perfratio when the SDK must write every marker immediately.
pub const ENV_LIVE: &str = "PERFRATIO_REGION_LIVE";
/// Value of the `scope` field of the exported metrics.
pub const SCOPE: &str = "region";
/// Key prefix of the per-run series inside the runner: `region_scope:<name>:<metric>`.
pub const KEY_PREFIX: &str = "region_scope:";
/// Interval at which the watcher looks for new markers. The effective interval is
/// the timer granularity of the OS (about 60 µs on Linux, 1 ms or more on Windows).
pub const POLL: Duration = Duration::from_micros(25);

/// Metric names of the scoped values, in export order.
pub const CPU_USER: &str = "cpu_user";
pub const CPU_SYSTEM: &str = "cpu_system";
pub const ENERGY: &str = "energy";

/// Key of the series of `metric` in region `name`.
pub fn key(name: &str, metric: &str) -> String {
    format!("{KEY_PREFIX}{name}:{metric}")
}

/// Unit of a scoped metric: seconds, joules, or a plain count.
pub fn unit(metric: &str) -> &'static str {
    match metric {
        CPU_USER | CPU_SYSTEM => "s",
        ENERGY => "J",
        _ => "count",
    }
}

/// What was read from the child at one instant. Counters and energy are totals
/// since the start of the watch, CPU times totals since the process start.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Sample {
    pub cpu_user: f64,
    pub cpu_system: f64,
    /// Joules since the watcher started, `None` without a working sampler.
    pub energy: Option<f64>,
    /// One entry per counter, `None` where it could not be read.
    pub counters: Vec<Option<u64>>,
}

/// Where samples come from. Real: [`LiveSource`]. Tests inject their own.
pub trait Source: Send {
    /// `None` when the child can no longer be read (it exited).
    fn read(&mut self) -> Option<Sample>;
}

/// One marker seen in the event file, with the sample taken for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Marker {
    pub start: bool,
    pub name: String,
    pub sample: Option<Sample>,
}

/// Value of one region in one run (sum over its start/stop pairs).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Delta {
    pub cpu_user: f64,
    pub cpu_system: f64,
    pub energy: Option<f64>,
    pub counters: Vec<Option<u64>>,
}

/// `start <name> <t>` or `stop <name> <t>` -> `(is_start, name)`. Other lines
/// (`hello`, comments, malformed) give `None`; perfratio's regular parser reports
/// the errors later.
pub fn marker_of_line(line: &str) -> Option<(bool, &str)> {
    let mut parts = line.split_whitespace();
    let start = match parts.next()? {
        "start" => true,
        "stop" => false,
        _ => return None,
    };
    let name = parts.next()?;
    crate::region::is_valid_name(name).then_some((start, name))
}

/// Sum the intervals of every region. A region whose marker has no sample in
/// some pair is `None` for the run (reported as `null`, never as 0).
pub fn totals(markers: &[Marker]) -> BTreeMap<String, Option<Delta>> {
    let mut open: BTreeMap<&str, Option<&Sample>> = BTreeMap::new();
    let mut sums: BTreeMap<String, Option<Delta>> = BTreeMap::new();
    let mut pairs: BTreeMap<&str, u32> = BTreeMap::new();
    for m in markers {
        if m.start {
            open.insert(&m.name, m.sample.as_ref());
            continue;
        }
        let Some(from) = open.remove(m.name.as_str()) else {
            continue;
        };
        let first = pairs.entry(&m.name).or_insert(0);
        let entry = sums
            .entry(m.name.clone())
            .or_insert_with(|| Some(Delta::default()));
        let (Some(from), Some(to)) = (from, m.sample.as_ref()) else {
            *entry = None;
            continue;
        };
        if let Some(sum) = entry {
            add_interval(sum, *first == 0, from, to);
        }
        *first += 1;
    }
    sums
}

/// Add `to - from` to `sum`. A value that is unreadable in one pair stays
/// unreadable for the whole run. Counters and energy that go backwards count as 0.
fn add_interval(sum: &mut Delta, first: bool, from: &Sample, to: &Sample) {
    sum.cpu_user += (to.cpu_user - from.cpu_user).max(0.0);
    sum.cpu_system += (to.cpu_system - from.cpu_system).max(0.0);
    let energy = to.energy.zip(from.energy).map(|(b, a)| (b - a).max(0.0));
    sum.energy = if first {
        energy
    } else {
        sum.energy.zip(energy).map(|(acc, e)| acc + e)
    };
    let diffs: Vec<Option<u64>> = (0..to.counters.len())
        .map(|i| {
            let b = to.counters[i]?;
            let a = from.counters.get(i).copied().flatten()?;
            Some(b.saturating_sub(a))
        })
        .collect();
    if first {
        sum.counters = diffs;
    } else {
        for (slot, d) in sum.counters.iter_mut().zip(diffs) {
            *slot = slot.zip(d).map(|(acc, d)| acc + d);
        }
    }
}

/// Result of a watch.
#[derive(Debug, Default)]
pub struct Outcome {
    pub markers: Vec<Marker>,
    /// Longest time between seeing a marker line and finishing its sample.
    pub max_read_latency: Duration,
}

/// The watcher thread. Dropping it without [`Watcher::finish`] stops it.
pub struct Watcher {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<Outcome>>,
}

impl Watcher {
    /// Poll `path` and sample `source` for every marker. `poll` is the sleep
    /// between two looks at the file.
    pub fn spawn(path: PathBuf, mut source: Box<dyn Source>, poll: Duration) -> Watcher {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            let mut outcome = Outcome::default();
            let mut offset = 0u64;
            let mut pending = String::new();
            loop {
                let stopping = flag.load(Ordering::Acquire);
                let seen = Instant::now();
                if let Ok(mut file) = std::fs::File::open(&path) {
                    if file.seek(SeekFrom::Start(offset)).is_ok() {
                        let mut chunk = Vec::new();
                        if let Ok(n) = file.read_to_end(&mut chunk) {
                            offset += n as u64;
                            pending.push_str(&String::from_utf8_lossy(&chunk));
                        }
                    }
                }
                while let Some(end) = pending.find('\n') {
                    let line: String = pending.drain(..=end).collect();
                    if let Some((start, name)) = marker_of_line(line.trim()) {
                        // after the stop signal the child may be gone (and its pid reused)
                        let sample = if flag.load(Ordering::Acquire) {
                            None
                        } else {
                            source.read()
                        };
                        outcome.max_read_latency = outcome.max_read_latency.max(seen.elapsed());
                        outcome.markers.push(Marker {
                            start,
                            name: name.to_string(),
                            sample,
                        });
                    }
                }
                if stopping {
                    return outcome;
                }
                std::thread::park_timeout(poll);
            }
        });
        Watcher {
            stop,
            handle: Some(handle),
        }
    }

    /// Stop after one more pass over the file and return what was seen.
    pub fn finish(mut self) -> Outcome {
        self.stop.store(true, Ordering::Release);
        let Some(handle) = self.handle.take() else {
            return Outcome::default();
        };
        handle.thread().unpark();
        handle.join().unwrap_or_default()
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// What the run asks the watcher to collect besides CPU time.
#[derive(Clone, Copy)]
pub struct ScopeSettings<'a> {
    /// `--energy`: the backend whose sampler is read at the markers
    pub energy: Option<crate::energy::BackendPreference>,
    /// `--counters`
    pub counters: Option<&'a crate::perf_counters::CounterConfig>,
}

/// Names of the counters of a run, by index of `config.active`; `None` for the
/// ones that are collected but not exported (the hidden `task-clock`).
pub fn counter_names(config: Option<&crate::perf_counters::CounterConfig>) -> Vec<Option<String>> {
    config
        .map(|c| {
            c.active
                .iter()
                .enumerate()
                .map(|(i, spec)| (!c.hidden.contains(&spec.kind)).then(|| c.key(i)))
                .collect()
        })
        .unwrap_or_default()
}

/// Names of the scoped metrics of a run, in export order.
pub fn metric_names(
    energy: bool,
    config: Option<&crate::perf_counters::CounterConfig>,
) -> Vec<String> {
    let mut names = vec![CPU_USER.to_string(), CPU_SYSTEM.to_string()];
    if energy {
        names.push(ENERGY.to_string());
    }
    names.extend(counter_names(config).into_iter().flatten());
    names
}

/// `(metric, value)` pairs of one region's delta; unreadable values are left out.
pub fn series(delta: &Delta, counter_names: &[Option<String>]) -> Vec<(String, f64)> {
    let mut out = vec![
        (CPU_USER.to_string(), delta.cpu_user),
        (CPU_SYSTEM.to_string(), delta.cpu_system),
    ];
    if let Some(joules) = delta.energy {
        out.push((ENERGY.to_string(), joules));
    }
    for (name, value) in counter_names.iter().zip(&delta.counters) {
        if let (Some(name), Some(value)) = (name, value) {
            out.push((name.clone(), *value as f64));
        }
    }
    out
}

/// Build the observer that starts the watcher once the child exists, and the slot
/// the watcher is left in. Call [`crate::timer::clear_spawn_observer`] when the run
/// ends, and `finish` the watcher in the slot.
pub fn install(
    path: PathBuf,
    settings: ScopeSettings<'_>,
) -> std::rc::Rc<std::cell::RefCell<Option<Watcher>>> {
    let slot = std::rc::Rc::new(std::cell::RefCell::new(None));
    let target = std::rc::Rc::clone(&slot);
    let energy = settings.energy;
    let counters = settings.counters.cloned();
    crate::timer::observe_spawn(move |child| {
        let sampler = energy
            .map(crate::energy::get_energy_sampler)
            .filter(|s| s.is_available());
        let n = counters.as_ref().map_or(0, |c| c.active.len());
        let open = counters
            .as_ref()
            .and_then(|c| crate::perf_counters::open_for_pid(c, child.pid));
        let source = LiveSource::new(child.pid, child.handle, sampler, open, n);
        *target.borrow_mut() = Some(Watcher::spawn(path, Box::new(source), POLL));
    });
    slot
}

// ---------------------------------------------------------------------------
// CPU time of a process
// ---------------------------------------------------------------------------

/// Fields of `/proc/<pid>/stat` that matter: `(utime, stime, starttime)` in
/// clock ticks. The command name is in parentheses and may contain spaces and
/// parentheses, so parsing starts after the last `)`.
pub fn parse_proc_stat(text: &str) -> Option<(u64, u64, u64)> {
    let rest = &text[text.rfind(')')? + 1..];
    // rest starts with the state (field 3); utime is field 14, stime 15, starttime 22
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let at = |field: usize| fields.get(field - 3)?.parse::<u64>().ok();
    Some((at(14)?, at(15)?, at(22)?))
}

/// 100 ns `FILETIME` ticks (Windows) to seconds.
pub fn filetime_seconds(low: u32, high: u32) -> f64 {
    ((u64::from(high) << 32) | u64::from(low)) as f64 * 1e-7
}

/// Reads user and system CPU seconds of one process. `None` once it is gone.
pub struct CpuReader {
    #[cfg(target_os = "linux")]
    pid: u32,
    #[cfg(target_os = "linux")]
    ticks_per_second: f64,
    /// `starttime` of the first read: a different one means the pid was reused
    #[cfg(target_os = "linux")]
    started: Option<u64>,
    #[cfg(target_os = "macos")]
    pid: u32,
    #[cfg(windows)]
    handle: usize,
}

impl CpuReader {
    #[cfg(unix)]
    pub fn new(pid: u32, _handle: usize) -> Self {
        #[cfg(target_os = "linux")]
        {
            // SAFETY: plain query of a system constant.
            let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
            CpuReader {
                pid,
                ticks_per_second: if hz > 0 { hz as f64 } else { 100.0 },
                started: None,
            }
        }
        #[cfg(target_os = "macos")]
        {
            CpuReader { pid }
        }
    }

    #[cfg(windows)]
    pub fn new(_pid: u32, handle: usize) -> Self {
        CpuReader { handle }
    }

    #[cfg(target_os = "linux")]
    pub fn read(&mut self) -> Option<(f64, f64)> {
        let text = std::fs::read_to_string(format!("/proc/{}/stat", self.pid)).ok()?;
        let (user, system, started) = parse_proc_stat(&text)?;
        if *self.started.get_or_insert(started) != started {
            return None;
        }
        Some((
            user as f64 / self.ticks_per_second,
            system as f64 / self.ticks_per_second,
        ))
    }

    #[cfg(target_os = "macos")]
    #[allow(deprecated)] // libc marks mach_timebase_info as deprecated in favour of mach2
    pub fn read(&mut self) -> Option<(f64, f64)> {
        // SAFETY: `info` is a plain C struct the call fills; the size passed is its size.
        unsafe {
            let mut info: libc::proc_taskinfo = std::mem::zeroed();
            let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
            let n = libc::proc_pidinfo(
                self.pid as libc::c_int,
                libc::PROC_PIDTASKINFO,
                0,
                (&mut info as *mut libc::proc_taskinfo).cast(),
                size,
            );
            if n != size {
                return None;
            }
            let mut base: libc::mach_timebase_info = std::mem::zeroed();
            if libc::mach_timebase_info(&mut base) != 0 || base.denom == 0 {
                return None;
            }
            let to_seconds =
                |ticks: u64| ticks as f64 * f64::from(base.numer) / f64::from(base.denom) * 1e-9;
            Some((
                to_seconds(info.pti_total_user),
                to_seconds(info.pti_total_system),
            ))
        }
    }

    #[cfg(windows)]
    pub fn read(&mut self) -> Option<(f64, f64)> {
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::Threading::GetProcessTimes;
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
        // SAFETY: the handle is the child's process handle, which stays open until the child
        // is reaped; the four out-pointers are valid FILETIMEs.
        let ok = unsafe {
            GetProcessTimes(
                self.handle as _,
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            )
        };
        (ok != 0).then(|| {
            (
                filetime_seconds(user.dwLowDateTime, user.dwHighDateTime),
                filetime_seconds(kernel.dwLowDateTime, kernel.dwHighDateTime),
            )
        })
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    pub fn read(&mut self) -> Option<(f64, f64)> {
        None
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
impl CpuReader {
    pub fn new(_pid: u32, _handle: usize) -> Self {
        CpuReader {}
    }
}

// ---------------------------------------------------------------------------
// The real source
// ---------------------------------------------------------------------------

/// Samples of the real child: CPU time, the energy sampler, the counters.
pub struct LiveSource {
    cpu: CpuReader,
    energy: Option<Box<dyn crate::energy::EnergySampler>>,
    /// Joules of the windows closed so far
    energy_total: f64,
    counters: Option<crate::perf_counters::OpenCounters>,
    n_counters: usize,
}

impl LiveSource {
    pub fn new(
        pid: u32,
        handle: usize,
        mut energy: Option<Box<dyn crate::energy::EnergySampler>>,
        counters: Option<crate::perf_counters::OpenCounters>,
        n_counters: usize,
    ) -> Self {
        if let Some(sampler) = energy.as_mut() {
            sampler.start();
        }
        LiveSource {
            cpu: CpuReader::new(pid, handle),
            energy,
            energy_total: 0.0,
            counters,
            n_counters,
        }
    }
}

impl Source for LiveSource {
    fn read(&mut self) -> Option<Sample> {
        let (cpu_user, cpu_system) = self.cpu.read()?;
        // A sampler reports the window since `start()`: close it and open the next one, so
        // the running total is the sum of the windows (the gap is a few microseconds).
        let energy = self.energy.as_mut().and_then(|sampler| {
            let window = sampler.stop();
            sampler.start();
            window.map(|joules| {
                self.energy_total += joules;
                self.energy_total
            })
        });
        let counters = self.counters.as_ref().map_or_else(
            || vec![None; self.n_counters],
            |open| {
                open.read()
                    .into_iter()
                    .map(|c| c.map(|c| c.value))
                    .collect()
            },
        );
        Some(Sample {
            cpu_user,
            cpu_system,
            energy,
            counters,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn sample(
        user: f64,
        system: f64,
        energy: Option<f64>,
        counters: &[Option<u64>],
    ) -> Option<Sample> {
        Some(Sample {
            cpu_user: user,
            cpu_system: system,
            energy,
            counters: counters.to_vec(),
        })
    }

    fn marker(start: bool, name: &str, sample: Option<Sample>) -> Marker {
        Marker {
            start,
            name: name.into(),
            sample,
        }
    }

    #[test]
    fn marker_lines() {
        assert_eq!(marker_of_line("start sort 100"), Some((true, "sort")));
        assert_eq!(marker_of_line("stop sort 200"), Some((false, "sort")));
        assert_eq!(marker_of_line("hello 1 rust/1.0.0"), None);
        assert_eq!(marker_of_line("start bad-name 1"), None);
        assert_eq!(marker_of_line("# comment"), None);
        assert_eq!(marker_of_line(""), None);
    }

    #[test]
    fn injected_readings_give_the_region_delta() {
        let markers = [
            marker(true, "a", sample(1.0, 0.5, Some(10.0), &[Some(100), None])),
            marker(
                false,
                "a",
                sample(1.75, 0.5, Some(12.5), &[Some(400), None]),
            ),
        ];
        let sums = totals(&markers);
        let a = sums["a"].as_ref().unwrap();
        assert!((a.cpu_user - 0.75).abs() < 1e-12);
        assert_eq!(a.cpu_system, 0.0);
        assert_eq!(a.energy, Some(2.5));
        assert_eq!(a.counters, vec![Some(300), None]);
    }

    #[test]
    fn pairs_are_summed_and_regions_may_overlap() {
        let markers = [
            marker(true, "a", sample(0.0, 0.0, None, &[Some(0)])),
            marker(true, "b", sample(1.0, 0.0, None, &[Some(10)])),
            marker(false, "b", sample(2.0, 0.0, None, &[Some(20)])),
            marker(false, "a", sample(3.0, 0.0, None, &[Some(30)])),
            marker(true, "a", sample(4.0, 0.0, None, &[Some(40)])),
            marker(false, "a", sample(4.5, 0.0, None, &[Some(45)])),
        ];
        let sums = totals(&markers);
        let a = sums["a"].as_ref().unwrap();
        assert_eq!(a.cpu_user, 3.5);
        assert_eq!(a.counters, vec![Some(35)]);
        assert_eq!(sums["b"].as_ref().unwrap().cpu_user, 1.0);
        assert_eq!(sums["a"].as_ref().unwrap().energy, None);
    }

    #[test]
    fn a_missing_sample_voids_the_region_but_not_the_others() {
        let markers = [
            marker(true, "a", sample(0.0, 0.0, None, &[])),
            marker(false, "a", None),
            marker(true, "b", sample(0.0, 0.0, None, &[])),
            marker(false, "b", sample(1.0, 0.0, None, &[])),
        ];
        let sums = totals(&markers);
        assert_eq!(sums["a"], None);
        assert_eq!(sums["b"].as_ref().unwrap().cpu_user, 1.0);
    }

    #[test]
    fn counter_gap_in_one_pair_makes_the_sum_null() {
        let markers = [
            marker(true, "a", sample(0.0, 0.0, None, &[Some(1)])),
            marker(false, "a", sample(1.0, 0.0, None, &[Some(2)])),
            marker(true, "a", sample(1.0, 0.0, None, &[None])),
            marker(false, "a", sample(2.0, 0.0, None, &[Some(9)])),
        ];
        let a = totals(&markers)["a"].clone().unwrap();
        assert_eq!(a.counters, vec![None]);
        assert_eq!(a.cpu_user, 2.0);
    }

    #[test]
    fn energy_gap_in_a_later_pair_makes_it_null() {
        let markers = [
            marker(true, "a", sample(0.0, 0.0, Some(1.0), &[])),
            marker(false, "a", sample(1.0, 0.0, Some(2.0), &[])),
            marker(true, "a", sample(1.0, 0.0, None, &[])),
            marker(false, "a", sample(2.0, 0.0, Some(5.0), &[])),
        ];
        assert_eq!(totals(&markers)["a"].clone().unwrap().energy, None);
    }

    #[test]
    fn a_counter_that_goes_backwards_does_not_underflow() {
        let markers = [
            marker(true, "a", sample(2.0, 0.0, Some(5.0), &[Some(10)])),
            marker(false, "a", sample(1.0, 0.0, Some(4.0), &[Some(5)])),
        ];
        let a = totals(&markers)["a"].clone().unwrap();
        assert_eq!(
            (a.cpu_user, a.energy, a.counters),
            (0.0, Some(0.0), vec![Some(0)])
        );
    }

    #[test]
    fn series_leave_out_unreadable_values() {
        let delta = Delta {
            cpu_user: 1.5,
            cpu_system: 0.25,
            energy: Some(3.0),
            counters: vec![Some(7), None, Some(9)],
        };
        let names = [
            Some("instructions".to_string()),
            Some("cycles".to_string()),
            None,
        ];
        let got = series(&delta, &names);
        let keys: Vec<&str> = got.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["cpu_user", "cpu_system", "energy", "instructions"]);
        assert_eq!(got[3].1, 7.0);
        assert_eq!(key("sort", "energy"), "region_scope:sort:energy");
        assert_eq!(unit("energy"), "J");
        assert_eq!(unit("instructions"), "count");
        assert_eq!(metric_names(false, None), ["cpu_user", "cpu_system"]);
    }

    #[test]
    fn proc_stat_with_awkward_command_name() {
        let line = "1234 (we ird) name) S 1 1234 1234 0 -1 4194304 100 0 0 0 250 75 0 0 20 0 1 0 987654 1000 100 18446744073709551615";
        assert_eq!(parse_proc_stat(line), Some((250, 75, 987654)));
        assert_eq!(parse_proc_stat("garbage"), None);
        assert_eq!(parse_proc_stat("1 (x) S 1 2"), None);
    }

    #[test]
    fn filetime_units() {
        assert!((filetime_seconds(10_000_000, 0) - 1.0).abs() < 1e-12);
        assert!((filetime_seconds(0, 1) - 429.4967296).abs() < 1e-6);
    }

    struct Counting(f64);
    impl Source for Counting {
        fn read(&mut self) -> Option<Sample> {
            self.0 += 1.0;
            sample(self.0, 0.0, None, &[])
        }
    }

    #[test]
    fn watcher_samples_each_marker_line_once() {
        let dir = std::env::temp_dir().join(format!("pf-scope-{}.log", std::process::id()));
        std::fs::write(&dir, "hello 1 test\n").unwrap();
        let watcher = Watcher::spawn(
            dir.clone(),
            Box::new(Counting(0.0)),
            Duration::from_micros(50),
        );
        let mut file = std::fs::OpenOptions::new().append(true).open(&dir).unwrap();
        // a line written in two pieces is read once, when complete
        write!(file, "start a 1\nstop").unwrap();
        file.flush().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        writeln!(file, " a 2").unwrap();
        file.flush().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let outcome = watcher.finish();
        let _ = std::fs::remove_file(&dir);
        assert_eq!(outcome.markers.len(), 2);
        assert!(outcome.markers[0].start && !outcome.markers[1].start);
        let a = totals(&outcome.markers)["a"].clone().unwrap();
        assert_eq!(a.cpu_user, 1.0);
    }
}
