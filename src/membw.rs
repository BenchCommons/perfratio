//! `--membw`: memory bandwidth and NUMA placement (`docs/specs/membw-numa.md`, #378).
//!
//! Two independent sources:
//!
//! * **Bandwidth**: the Intel uncore integrated memory controller (`uncore_imc_*` PMUs,
//!   events `cas_count_read` and `cas_count_write`), discovered through
//!   `/sys/bus/event_source/devices`. The counters are system wide per socket, so the
//!   value includes every process on the machine during the run.
//! * **NUMA placement**: `/proc/<pid>/numa_maps` of the benchmarked process, sampled while
//!   it runs. The result is the largest sample (resident pages per node, in bytes).
//!
//! Everything that parses text takes the text or a root directory, so the CI tests run on
//! fixtures. Opening the uncore counters needs bare-metal Linux and
//! `perf_event_paranoid <= 0` or `CAP_PERFMON`; otherwise the feature degrades with a reason.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// Why bandwidth cannot be measured (open enum in the export).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unavailable {
    UnsupportedPlatform,
    /// No `uncore_imc` PMU: a VM, an Arm machine or an older CPU
    NoUncoreImc,
    /// AMD Data Fabric PMU present; its event encodings differ per family and are not mapped
    AmdDfUnsupported,
    PermissionDenied,
    Other,
}

impl Unavailable {
    pub fn as_str(self) -> &'static str {
        match self {
            Unavailable::UnsupportedPlatform => "unsupported_platform",
            Unavailable::NoUncoreImc => "no_uncore_imc",
            Unavailable::AmdDfUnsupported => "amd_df_unsupported",
            Unavailable::PermissionDenied => "perf_paranoid",
            Unavailable::Other => "other",
        }
    }

    /// Sentence for the warning on stderr
    pub fn explain(self) -> &'static str {
        match self {
            Unavailable::UnsupportedPlatform => "memory bandwidth counters need Linux",
            Unavailable::NoUncoreImc => {
                "no uncore_imc PMU was found (virtual machine, Arm or an older CPU)"
            }
            Unavailable::AmdDfUnsupported => {
                "AMD Data Fabric bandwidth events are not supported yet"
            }
            Unavailable::PermissionDenied => {
                "opening the uncore counters was denied; lower kernel.perf_event_paranoid to 0 \
                 or run with CAP_PERFMON"
            }
            Unavailable::Other => "the uncore counters could not be opened",
        }
    }
}

// ---------------------------------------------------------------------------
// Uncore IMC discovery (sysfs)
// ---------------------------------------------------------------------------

/// One `cas_count_*` event of an IMC PMU
#[derive(Debug, Clone, PartialEq)]
pub struct ImcEvent {
    pub config: u64,
    /// Bytes per count (`.scale` times the `.unit`)
    pub bytes_per_count: f64,
}

/// One memory controller PMU, e.g. `uncore_imc_0`
#[derive(Debug, Clone, PartialEq)]
pub struct ImcPmu {
    pub name: String,
    pub type_id: u32,
    /// CPU to open the system-wide event on (first of `cpumask`)
    pub cpu: i32,
    pub read: ImcEvent,
    pub write: ImcEvent,
}

/// Bit range of a `format/<term>` file such as `config:0-7`
fn parse_format(text: &str) -> Option<(u32, u32)> {
    let range = text.trim().strip_prefix("config:")?;
    let (low, high) = match range.split_once('-') {
        Some((low, high)) => (low.parse().ok()?, high.parse().ok()?),
        None => {
            let bit = range.parse().ok()?;
            (bit, bit)
        }
    };
    // A bit range of a 64-bit `config`: anything else would overflow the shifts below
    (low <= high && high <= 63).then_some((low, high))
}

fn parse_number(text: &str) -> Option<u64> {
    let text = text.trim();
    match text.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
}

/// `event=0x04,umask=0x03` to a `config` value, using the PMU's `format` terms
/// (`event` and `umask` as bit ranges of `config`).
pub fn parse_event_config(spec: &str, formats: &BTreeMap<String, (u32, u32)>) -> Option<u64> {
    let mut config = 0u64;
    for term in spec.trim().split(',').filter(|t| !t.is_empty()) {
        let (name, value) = term.split_once('=')?;
        let (low, high) = *formats.get(name.trim())?;
        let width = high - low + 1;
        let value = parse_number(value)?;
        if width < 64 && value >> width != 0 {
            return None;
        }
        config |= value << low;
    }
    Some(config)
}

/// Bytes in one unit of an event's `.unit` file (`MiB` when the file is absent, as the
/// kernel's IMC events document)
fn unit_bytes(unit: Option<&str>) -> Option<f64> {
    match unit.map(str::trim).unwrap_or("MiB") {
        "B" | "Bytes" => Some(1.0),
        "KiB" => Some(1024.0),
        "MiB" => Some(1024.0 * 1024.0),
        "GiB" => Some(1024.0 * 1024.0 * 1024.0),
        _ => None,
    }
}

/// First CPU of a `cpumask` such as `0` or `0,18` or `0-3`
pub fn first_cpu(mask: &str) -> Option<i32> {
    let first = mask.trim().split(',').next()?;
    first.split('-').next()?.parse().ok()
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

/// The IMC PMUs below `root` (`/` on a real system), or the reason there are none.
/// `uncore_imc_free_running_*` PMUs have other events and are skipped.
pub fn discover(root: &Path) -> Result<Vec<ImcPmu>, Unavailable> {
    let devices = root.join("sys/bus/event_source/devices");
    let mut names: Vec<String> = std::fs::read_dir(&devices)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    let mut pmus = Vec::new();
    for name in names
        .iter()
        .filter(|n| n.starts_with("uncore_imc") && !n.contains("free_running"))
    {
        let dir = devices.join(name);
        if let Some(pmu) = read_pmu(name, &dir) {
            pmus.push(pmu);
        }
    }
    if pmus.is_empty() {
        let amd = names.iter().any(|n| n.starts_with("amd_df"));
        return Err(if amd {
            Unavailable::AmdDfUnsupported
        } else {
            Unavailable::NoUncoreImc
        });
    }
    Ok(pmus)
}

fn read_pmu(name: &str, dir: &Path) -> Option<ImcPmu> {
    let type_id = read_trimmed(&dir.join("type"))?.parse().ok()?;
    let cpu = first_cpu(&read_trimmed(&dir.join("cpumask"))?)?;
    let mut formats = BTreeMap::new();
    if let Ok(rd) = std::fs::read_dir(dir.join("format")) {
        for entry in rd.filter_map(|e| e.ok()) {
            let term = entry.file_name().into_string().ok()?;
            if let Some(range) = read_trimmed(&entry.path())
                .as_deref()
                .and_then(parse_format)
            {
                formats.insert(term, range);
            }
        }
    }
    let event = |which: &str| -> Option<ImcEvent> {
        let events = dir.join("events");
        let config = parse_event_config(&read_trimmed(&events.join(which))?, &formats)?;
        let scale: f64 = read_trimmed(&events.join(format!("{which}.scale")))?
            .parse()
            .ok()?;
        let unit = unit_bytes(read_trimmed(&events.join(format!("{which}.unit"))).as_deref())?;
        Some(ImcEvent {
            config,
            bytes_per_count: scale * unit,
        })
    };
    Some(ImcPmu {
        name: name.to_string(),
        type_id,
        cpu,
        read: event("cas_count_read")?,
        write: event("cas_count_write")?,
    })
}

/// Width of the uncore counters; a counter that wraps between two reads still gives the
/// right difference
const COUNTER_MASK: u64 = (1 << 48) - 1;

/// Bytes read and written between two snapshots, summed over the PMUs.
/// A snapshot holds `(read_count, write_count)` for each PMU in order.
pub fn delta_bytes(pmus: &[ImcPmu], before: &[(u64, u64)], after: &[(u64, u64)]) -> (u64, u64) {
    let mut read = 0.0;
    let mut write = 0.0;
    for ((pmu, b), a) in pmus.iter().zip(before).zip(after) {
        read += (a.0.wrapping_sub(b.0) & COUNTER_MASK) as f64 * pmu.read.bytes_per_count;
        write += (a.1.wrapping_sub(b.1) & COUNTER_MASK) as f64 * pmu.write.bytes_per_count;
    }
    (read.round() as u64, write.round() as u64)
}

// ---------------------------------------------------------------------------
// NUMA placement (numa_maps)
// ---------------------------------------------------------------------------

/// Bytes per NUMA node from the text of `/proc/<pid>/numa_maps`: every mapping line lists
/// `N<node>=<pages>` and `kernelpagesize_kB=<n>` (4 kB when absent).
pub fn parse_numa_maps(text: &str) -> BTreeMap<u32, u64> {
    let mut nodes = BTreeMap::new();
    for line in text.lines() {
        let mut page_bytes = 4096u64;
        let mut pages: Vec<(u32, u64)> = Vec::new();
        for token in line.split_whitespace().skip(1) {
            let Some((key, value)) = token.split_once('=') else {
                continue;
            };
            if key == "kernelpagesize_kB" {
                if let Ok(kb) = value.parse::<u64>() {
                    page_bytes = kb * 1024;
                }
            } else if let Some(node) = key.strip_prefix('N').and_then(|n| n.parse::<u32>().ok()) {
                if let Ok(count) = value.parse::<u64>() {
                    pages.push((node, count));
                }
            }
        }
        for (node, count) in pages {
            *nodes.entry(node).or_insert(0) += count * page_bytes;
        }
    }
    nodes
}

/// Why NUMA placement cannot be sampled (`None`: it can)
pub fn numa_unavailable() -> Option<&'static str> {
    if !cfg!(target_os = "linux") {
        return Some("unsupported_platform");
    }
    if std::fs::metadata("/proc/self/numa_maps").is_err() {
        // kernel built without CONFIG_NUMA
        return Some("no_numa_maps");
    }
    None
}

/// Polls `numa_maps` of one process until [`Sampler::finish`]
pub struct Sampler {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<Option<BTreeMap<u32, u64>>>>,
}

const POLL: std::time::Duration = std::time::Duration::from_millis(20);

impl Sampler {
    pub fn spawn(pid: u32) -> Self {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            let path = format!("/proc/{pid}/numa_maps");
            let mut best: Option<(u64, BTreeMap<u32, u64>)> = None;
            loop {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    let sample = parse_numa_maps(&text);
                    let total: u64 = sample.values().sum();
                    if total > 0 && best.as_ref().is_none_or(|(t, _)| total >= *t) {
                        best = Some((total, sample));
                    }
                }
                if flag.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(POLL);
            }
            best.map(|(_, sample)| sample)
        });
        Sampler {
            stop,
            handle: Some(handle),
        }
    }

    /// The largest sample, `None` when none could be read (the run was too short)
    pub fn finish(mut self) -> Option<BTreeMap<u32, u64>> {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        self.handle.take()?.join().ok().flatten()
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Process-wide state
// ---------------------------------------------------------------------------

/// Bandwidth and placement of one run
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Run {
    pub read_bytes: Option<u64>,
    pub write_bytes: Option<u64>,
    /// Resident bytes per NUMA node
    pub node_bytes: Option<BTreeMap<u32, u64>>,
}

enum Source {
    /// `--debug-mode`: synthetic values
    Mock,
    Imc(Imc),
    Unavailable(Unavailable),
}

struct State {
    source: Source,
}

static STATE: OnceLock<State> = OnceLock::new();

/// Turns the feature on for this process. `mock` is `--debug-mode`. Returns the reason
/// bandwidth is unavailable, if it is.
pub fn configure(mock: bool) -> Option<Unavailable> {
    let source = if mock {
        Source::Mock
    } else {
        match Imc::open(Path::new("/")) {
            Ok(imc) => Source::Imc(imc),
            Err(reason) => Source::Unavailable(reason),
        }
    };
    let state = STATE.get_or_init(|| State { source });
    match state.source {
        Source::Unavailable(reason) => Some(reason),
        _ => None,
    }
}

/// `--membw` was given
pub fn enabled() -> bool {
    STATE.get().is_some()
}

/// Why bandwidth is unavailable (`None`: measured, or not requested)
pub fn unavailable() -> Option<Unavailable> {
    match STATE.get()?.source {
        Source::Unavailable(reason) => Some(reason),
        _ => None,
    }
}

/// Whether the run is simulated (`--debug-mode`)
pub fn is_mock() -> bool {
    matches!(STATE.get().map(|s| &s.source), Some(Source::Mock))
}

/// Counter values before a run
pub struct Snapshot(Vec<(u64, u64)>);

/// Reads the counters right before a run
pub fn begin() -> Option<Snapshot> {
    match &STATE.get()?.source {
        Source::Imc(imc) => imc.read().map(Snapshot),
        _ => None,
    }
}

/// Bandwidth of the run that started at `before`: `(read_bytes, write_bytes)`
pub fn end(before: Option<Snapshot>) -> Option<(u64, u64)> {
    let Source::Imc(imc) = &STATE.get()?.source else {
        return None;
    };
    let after = imc.read()?;
    Some(delta_bytes(&imc.pmus, &before?.0, &after))
}

/// Synthetic values for `--debug-mode`: about 2 GB/s read and 1 GB/s written, with 80% of
/// the resident pages on node 0
pub fn mock_run(time_real: f64) -> Run {
    let seconds = time_real.max(0.0);
    Run {
        read_bytes: Some((2.0e9 * seconds) as u64),
        write_bytes: Some((1.0e9 * seconds) as u64),
        node_bytes: Some(BTreeMap::from([(0, 80 << 20), (1, 20 << 20)])),
    }
}

struct Imc {
    pmus: Vec<ImcPmu>,
    #[cfg(target_os = "linux")]
    fds: Vec<(std::os::fd::OwnedFd, std::os::fd::OwnedFd)>,
}

impl Imc {
    #[cfg(target_os = "linux")]
    fn open(root: &Path) -> Result<Self, Unavailable> {
        use std::os::fd::{FromRawFd, OwnedFd};
        let pmus = discover(root)?;
        let open = |pmu: &ImcPmu, config: u64| -> Result<OwnedFd, Unavailable> {
            // `struct perf_event_attr` as 16 words: type/size, config; flags 0 starts the
            // counter enabled
            let mut attr = [0u64; 16];
            attr[0] =
                u64::from(pmu.type_id) | (u64::from(std::mem::size_of_val(&attr) as u32) << 32);
            attr[1] = config;
            // SAFETY: `attr` is a valid perf_event_attr of the size it declares; the other
            // arguments are plain integers (any process, one CPU, no group, close on exec).
            let fd = unsafe {
                libc::syscall(
                    libc::SYS_perf_event_open,
                    attr.as_ptr(),
                    -1 as libc::pid_t,
                    pmu.cpu as libc::c_int,
                    -1 as libc::c_int,
                    8 as libc::c_ulong,
                )
            };
            if fd < 0 {
                return Err(match std::io::Error::last_os_error().raw_os_error() {
                    Some(libc::EACCES | libc::EPERM) => Unavailable::PermissionDenied,
                    _ => Unavailable::Other,
                });
            }
            // SAFETY: the kernel returned a new fd that nothing else owns.
            Ok(unsafe { OwnedFd::from_raw_fd(fd as libc::c_int) })
        };
        let mut fds = Vec::new();
        for pmu in &pmus {
            fds.push((open(pmu, pmu.read.config)?, open(pmu, pmu.write.config)?));
        }
        Ok(Imc { pmus, fds })
    }

    #[cfg(not(target_os = "linux"))]
    fn open(_root: &Path) -> Result<Self, Unavailable> {
        Err(Unavailable::UnsupportedPlatform)
    }

    #[cfg(target_os = "linux")]
    fn read(&self) -> Option<Vec<(u64, u64)>> {
        use std::os::fd::AsRawFd;
        let one = |fd: &std::os::fd::OwnedFd| -> Option<u64> {
            let mut value = 0u64;
            // SAFETY: reads at most 8 bytes into a live u64.
            let n = unsafe { libc::read(fd.as_raw_fd(), (&raw mut value).cast(), 8) };
            (n == 8).then_some(value)
        };
        self.fds
            .iter()
            .map(|(r, w)| Some((one(r)?, one(w)?)))
            .collect()
    }

    #[cfg(not(target_os = "linux"))]
    fn read(&self) -> Option<Vec<(u64, u64)>> {
        None
    }
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

/// `results[].membw`: per-run series aligned with `times`; `null` where a run has no value
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BandwidthSeries {
    /// Where the numbers come from: `uncore_imc` (`mock` with `--debug-mode`)
    pub source: String,
    pub read_bytes: Vec<Option<u64>>,
    pub write_bytes: Vec<Option<u64>>,
    /// (read + write) divided by the wall time of the run
    pub bytes_per_second: Vec<Option<f64>>,
}

/// `results[].numa`: resident bytes per node
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NumaSeries {
    /// Per run, keyed by node number; `null` where no sample could be read
    pub node_bytes: Vec<Option<BTreeMap<String, u64>>>,
    /// Mean share of the resident bytes on each node over the sampled runs
    pub mean_node_fraction: BTreeMap<String, f64>,
    /// Node with the largest mean share
    pub dominant_node: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Export {
    pub bandwidth: Option<BandwidthSeries>,
    /// Summaries of `read_bytes`, `write_bytes` and `bytes_per_second`
    pub summary: BTreeMap<String, crate::benchmark::benchmark_result::MetricSummary>,
    pub numa: Option<NumaSeries>,
}

fn series<T: Copy>(runs: &[Option<Run>], f: impl Fn(&Run) -> Option<T>) -> Vec<Option<T>> {
    runs.iter().map(|r| r.as_ref().and_then(&f)).collect()
}

/// Series and summaries of a benchmark, `None` when `--membw` was not given or no run
/// produced anything. `times` are the wall times of the same runs.
pub fn export(runs: &[Option<Run>], times: &[f64]) -> Option<Export> {
    if runs.is_empty() || !enabled() {
        return None;
    }
    let read = series(runs, |r| r.read_bytes);
    let write = series(runs, |r| r.write_bytes);
    let bandwidth = if read.iter().chain(&write).any(Option::is_some) {
        let rate = |bytes: &[Option<u64>], i: usize| bytes[i].map(|b| b as f64 / times[i]);
        let total: Vec<Option<f64>> = (0..runs.len())
            .map(|i| match (read[i], write[i], times.get(i)) {
                (Some(r), Some(w), Some(&t)) if t > 0.0 => Some((r + w) as f64 / t),
                _ => None,
            })
            .collect();
        let mut summary = BTreeMap::new();
        let mut add = |name: &str, values: Vec<f64>| {
            if let Some(s) = crate::benchmark::benchmark_result::MetricSummary::from_values(&values)
            {
                summary.insert(name.to_string(), s);
            }
        };
        add(
            "read_bytes_per_second",
            (0..runs.len())
                .filter(|&i| times.get(i).is_some_and(|&t| t > 0.0))
                .filter_map(|i| rate(&read, i))
                .collect(),
        );
        add(
            "write_bytes_per_second",
            (0..runs.len())
                .filter(|&i| times.get(i).is_some_and(|&t| t > 0.0))
                .filter_map(|i| rate(&write, i))
                .collect(),
        );
        add(
            "bytes_per_second",
            total.iter().flatten().copied().collect(),
        );
        Some((
            BandwidthSeries {
                source: if is_mock() { "mock" } else { "uncore_imc" }.to_string(),
                read_bytes: read,
                write_bytes: write,
                bytes_per_second: total,
            },
            summary,
        ))
    } else {
        None
    };
    let numa = numa_series(runs);
    if bandwidth.is_none() && numa.is_none() {
        return None;
    }
    let (bandwidth, summary) = match bandwidth {
        Some((b, s)) => (Some(b), s),
        None => (None, BTreeMap::new()),
    };
    Some(Export {
        bandwidth,
        summary,
        numa,
    })
}

fn numa_series(runs: &[Option<Run>]) -> Option<NumaSeries> {
    let per_run: Vec<Option<BTreeMap<u32, u64>>> = runs
        .iter()
        .map(|r| {
            r.as_ref()
                .and_then(|r| r.node_bytes.clone())
                .filter(|n| n.values().sum::<u64>() > 0)
        })
        .collect();
    let sampled: Vec<&BTreeMap<u32, u64>> = per_run.iter().flatten().collect();
    if sampled.is_empty() {
        return None;
    }
    let mut fractions: BTreeMap<u32, f64> = BTreeMap::new();
    for nodes in &sampled {
        let total = nodes.values().sum::<u64>() as f64;
        for (&node, &bytes) in *nodes {
            *fractions.entry(node).or_insert(0.0) += bytes as f64 / total / sampled.len() as f64;
        }
    }
    let dominant_node = fractions
        .iter()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map(|(&n, _)| n)?;
    Some(NumaSeries {
        node_bytes: per_run
            .into_iter()
            .map(|r| r.map(|m| m.into_iter().map(|(n, b)| (n.to_string(), b)).collect()))
            .collect(),
        mean_node_fraction: fractions
            .into_iter()
            .map(|(n, f)| (n.to_string(), f))
            .collect(),
        dominant_node,
    })
}

/// `12.3 GB/s` (decimal prefixes, as memory bandwidth is usually quoted)
pub fn format_rate(bytes_per_second: f64) -> String {
    const UNITS: [(&str, f64); 3] = [("GB/s", 1e9), ("MB/s", 1e6), ("kB/s", 1e3)];
    for (unit, size) in UNITS {
        if bytes_per_second >= size {
            return format!("{:.1} {unit}", bytes_per_second / size);
        }
    }
    format!("{bytes_per_second:.0} B/s")
}

/// The terminal line of a benchmark
pub fn terminal_line(export: &Export) -> String {
    let mut parts = Vec::new();
    if let Some(total) = export.summary.get("bytes_per_second") {
        let mut text = format_rate(total.mean);
        if let (Some(r), Some(w)) = (
            export.summary.get("read_bytes_per_second"),
            export.summary.get("write_bytes_per_second"),
        ) {
            text.push_str(&format!(
                " (read {}, write {})",
                format_rate(r.mean),
                format_rate(w.mean)
            ));
        }
        parts.push(text);
    }
    if let Some(numa) = &export.numa {
        let nodes: Vec<String> = numa
            .mean_node_fraction
            .iter()
            .map(|(node, f)| format!("node{node} {:.0}%", f * 100.0))
            .collect();
        parts.push(format!("NUMA {}", nodes.join(", ")));
    }
    format!("  Memory:             {}", parts.join("; "))
}

#[cfg(test)]
mod tests {
    #[test]
    fn invalid_bit_ranges_are_rejected_without_panicking() {
        assert_eq!(super::parse_format("config:0-7"), Some((0, 7)));
        assert_eq!(super::parse_format("config:63"), Some((63, 63)));
        assert_eq!(super::parse_format("config:8-4"), None);
        assert_eq!(super::parse_format("config:0-64"), None);
        assert_eq!(super::parse_format("config:70"), None);
    }

    use super::*;

    fn write(root: &Path, path: &str, text: &str) {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }

    fn intel_pmu(root: &Path, name: &str, type_id: u32, cpumask: &str) {
        let base = format!("sys/bus/event_source/devices/{name}");
        write(root, &format!("{base}/type"), &format!("{type_id}\n"));
        write(root, &format!("{base}/cpumask"), &format!("{cpumask}\n"));
        write(root, &format!("{base}/format/event"), "config:0-7\n");
        write(root, &format!("{base}/format/umask"), "config:8-15\n");
        for (event, spec) in [
            ("cas_count_read", "event=0x04,umask=0x03"),
            ("cas_count_write", "event=0x04,umask=0x0c"),
        ] {
            write(
                root,
                &format!("{base}/events/{event}"),
                &format!("{spec}\n"),
            );
            write(
                root,
                &format!("{base}/events/{event}.scale"),
                "6.103515625e-05\n",
            );
            write(root, &format!("{base}/events/{event}.unit"), "MiB\n");
        }
    }

    #[test]
    fn discovers_intel_imc_pmus() {
        let root = tempfile::tempdir().unwrap();
        intel_pmu(root.path(), "uncore_imc_0", 14, "0,18");
        intel_pmu(root.path(), "uncore_imc_1", 15, "0");
        write(
            root.path(),
            "sys/bus/event_source/devices/uncore_imc_free_running_0/type",
            "20\n",
        );
        let pmus = discover(root.path()).unwrap();
        assert_eq!(pmus.len(), 2);
        assert_eq!(pmus[0].name, "uncore_imc_0");
        assert_eq!(pmus[0].type_id, 14);
        assert_eq!(pmus[0].cpu, 0);
        assert_eq!(pmus[0].read.config, 0x0304);
        assert_eq!(pmus[0].write.config, 0x0c04);
        // 64 bytes per count
        assert!((pmus[0].read.bytes_per_count - 64.0).abs() < 1e-9);
    }

    #[test]
    fn no_imc_gives_a_reason() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(discover(root.path()), Err(Unavailable::NoUncoreImc));
        write(
            root.path(),
            "sys/bus/event_source/devices/amd_df/type",
            "12\n",
        );
        assert_eq!(discover(root.path()), Err(Unavailable::AmdDfUnsupported));
    }

    #[test]
    fn a_pmu_without_events_is_skipped() {
        let root = tempfile::tempdir().unwrap();
        write(
            root.path(),
            "sys/bus/event_source/devices/uncore_imc_0/type",
            "14\n",
        );
        assert_eq!(discover(root.path()), Err(Unavailable::NoUncoreImc));
    }

    #[test]
    fn event_config_follows_the_format_terms() {
        let formats = BTreeMap::from([
            ("event".to_string(), (0, 7)),
            ("umask".to_string(), (8, 15)),
        ]);
        assert_eq!(
            parse_event_config("event=0x04,umask=0x03", &formats),
            Some(0x0304)
        );
        assert_eq!(parse_event_config("event=4", &formats), Some(4));
        // value wider than its field, unknown term
        assert_eq!(parse_event_config("event=0x1ff", &formats), None);
        assert_eq!(parse_event_config("edge=1", &formats), None);
        assert_eq!(parse_format("config:8-15"), Some((8, 15)));
        assert_eq!(parse_format("config:18"), Some((18, 18)));
        assert_eq!(parse_format("config1:0-7"), None);
    }

    #[test]
    fn first_cpu_of_a_mask() {
        assert_eq!(first_cpu("0,18\n"), Some(0));
        assert_eq!(first_cpu("4-7"), Some(4));
        assert_eq!(first_cpu(""), None);
    }

    #[test]
    fn delta_converts_counts_to_bytes_and_handles_wraparound() {
        let root = tempfile::tempdir().unwrap();
        intel_pmu(root.path(), "uncore_imc_0", 14, "0");
        intel_pmu(root.path(), "uncore_imc_1", 15, "0");
        let pmus = discover(root.path()).unwrap();
        let before = [(100, 10), (COUNTER_MASK - 1, 0)];
        let after = [(1100, 20), (2, 5)];
        // read: 1000 + 4 counts of 64 B; write: 10 + 5 counts
        assert_eq!(
            delta_bytes(&pmus, &before, &after),
            ((1000 + 4) * 64, 15 * 64)
        );
    }

    #[test]
    fn numa_maps_sums_pages_per_node() {
        let text = "\
00400000 default file=/bin/cat mapped=2 N0=2 kernelpagesize_kB=4
7f0000000000 default anon=512 dirty=512 active=0 N0=256 N1=256 kernelpagesize_kB=4
7f0010000000 interleave:0-1 anon=2 huge dirty=2 N1=2 kernelpagesize_kB=2048
7ffd0000 default stack anon=3 dirty=3 N0=3
";
        let nodes = parse_numa_maps(text);
        assert_eq!(nodes[&0], (2 + 256 + 3) * 4096);
        assert_eq!(nodes[&1], 256 * 4096 + 2 * 2048 * 1024);
        assert!(parse_numa_maps("").is_empty());
        // `anon=` and `dirty=` are not node tokens
        assert!(parse_numa_maps("00400000 default anon=5 dirty=5").is_empty());
    }

    fn run(read: Option<u64>, write: Option<u64>, nodes: &[(u32, u64)]) -> Option<Run> {
        Some(Run {
            read_bytes: read,
            write_bytes: write,
            node_bytes: (!nodes.is_empty()).then(|| nodes.iter().copied().collect()),
        })
    }

    #[test]
    fn numa_series_averages_the_node_shares() {
        let runs = [
            run(None, None, &[(0, 75), (1, 25)]),
            run(None, None, &[(0, 50), (1, 50)]),
            run(None, None, &[]),
            None,
        ];
        let numa = numa_series(&runs).unwrap();
        assert_eq!(numa.node_bytes.len(), 4);
        assert!(numa.node_bytes[2].is_none() && numa.node_bytes[3].is_none());
        assert!((numa.mean_node_fraction["0"] - 0.625).abs() < 1e-12);
        assert!((numa.mean_node_fraction["1"] - 0.375).abs() < 1e-12);
        assert_eq!(numa.dominant_node, 0);
        assert!(numa_series(&[None, run(None, None, &[])]).is_none());
    }

    #[test]
    fn format_rate_picks_a_unit() {
        assert_eq!(format_rate(12.34e9), "12.3 GB/s");
        assert_eq!(format_rate(5.0e6), "5.0 MB/s");
        assert_eq!(format_rate(999.0), "999 B/s");
    }

    #[test]
    fn mock_run_scales_with_time() {
        let run = mock_run(0.5);
        assert_eq!(run.read_bytes, Some(1_000_000_000));
        assert_eq!(run.write_bytes, Some(500_000_000));
    }

    #[test]
    fn reasons_are_stable() {
        assert_eq!(Unavailable::NoUncoreImc.as_str(), "no_uncore_imc");
        assert_eq!(Unavailable::PermissionDenied.as_str(), "perf_paranoid");
    }
}
