//! Run metadata attached to the machine-readable exports (JSON, CSV).

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Result};
use serde::Serialize;

/// Information about the perfratio invocation that produced an export.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ExportMetadata {
    pub version: &'static str,
    pub command_line: Vec<String>,
    /// Start of the run, RFC 3339 in UTC
    pub started_at: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    pub system: SystemInfo,
    /// Energy backend that produced the samples (`sysfs_powercap` or `perf_pmu`).
    /// Omitted unless `--energy` is given and a backend is usable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub energy_backend: Option<&'static str>,
    /// Which measurement groups were measured, unavailable or not requested
    pub measurements: Measurements,
}

/// Status of one measurement group (`schemaVersion` 1.0, open enum).
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementStatus {
    /// Requested and the backend works
    Measured,
    /// Requested; only some of the events work (`counters`)
    Partial,
    /// Requested, but the backend could not be used
    Unavailable,
    /// The flag was not given
    NotRequested,
}

/// Host-level status per measurement group, identical for every result.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Measurements {
    pub time: MeasurementStatus,
    pub memory: MeasurementStatus,
    pub energy: MeasurementStatus,
    /// Why energy is `unavailable`; an open enum (see `energy::Unavailable::as_str`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub energy_unavailable_reason: Option<&'static str>,
    pub resources: MeasurementStatus,
    /// Hardware counters (`--counters`)
    pub counters: MeasurementStatus,
    /// Why counters are `unavailable`: `unsupported_platform`, `perf_paranoid`,
    /// `no_pmu` or `other` (open enum)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counters_unavailable_reason: Option<&'static str>,
    /// Requested events that could not be opened (status `partial`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counters_unavailable_events: Option<Vec<&'static str>>,
    /// `--simd` floating-point breakdown (new in 1.9)
    pub simd: MeasurementStatus,
    /// Why `simd` is `unavailable`: `unsupported_platform`, `unsupported_cpu`,
    /// `perf_paranoid`, `no_pmu`, `hybrid_cpu_unpinned` or `other` (open enum)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub simd_unavailable_reason: Option<&'static str>,
}

impl Measurements {
    /// `energy` is the backend selection result, `None` if `--energy` was not given.
    pub fn new(
        energy: Option<Result<crate::energy::EnergyBackend, crate::energy::Unavailable>>,
        resources_requested: bool,
    ) -> Self {
        use MeasurementStatus::*;
        let (energy, reason) = match energy {
            None => (NotRequested, None),
            Some(Ok(_)) => (Measured, None),
            Some(Err(reason)) => (Unavailable, Some(reason.as_str())),
        };
        Measurements {
            time: Measured,
            memory: Measured,
            energy,
            energy_unavailable_reason: reason,
            resources: if resources_requested {
                Measured
            } else {
                NotRequested
            },
            counters: NotRequested,
            counters_unavailable_reason: None,
            counters_unavailable_events: None,
            simd: NotRequested,
            simd_unavailable_reason: None,
        }
    }

    /// Records the outcome of `--counters` (`None`: not requested).
    pub fn with_counters(mut self, config: Option<&crate::perf_counters::CounterConfig>) -> Self {
        use crate::perf_counters::Availability;
        let Some(config) = config else {
            return self;
        };
        if let Some(simd) = &config.simd {
            match simd.status {
                crate::simd::Status::Measured => self.simd = MeasurementStatus::Measured,
                crate::simd::Status::Unavailable(reason) => {
                    self.simd = MeasurementStatus::Unavailable;
                    self.simd_unavailable_reason = Some(reason.as_str());
                }
            }
        }
        // `--simd` alone leaves `counters` as not requested
        if config.requested.is_empty() {
            return self;
        }
        match &config.availability {
            Availability::Measured => self.counters = MeasurementStatus::Measured,
            Availability::Partial(events) => {
                self.counters = MeasurementStatus::Partial;
                self.counters_unavailable_events = Some(events.iter().map(|k| k.name()).collect());
            }
            Availability::Unavailable(reason) => {
                self.counters = MeasurementStatus::Unavailable;
                self.counters_unavailable_reason = Some(reason.as_str());
            }
        }
        self
    }
}

/// Environment the benchmarks ran in. Deliberately no hostname or user name.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SystemInfo {
    pub os: &'static str,
    pub arch: &'static str,
    pub cpus: usize,
    /// CPU model name, if it can be determined
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_model: Option<String>,
    /// Kernel / OS release (e.g. "6.8.0-45-generic", "24.1.0")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernel: Option<String>,
}

impl ExportMetadata {
    pub fn new(labels: BTreeMap<String, String>, measurements: Measurements) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        ExportMetadata {
            version: env!("CARGO_PKG_VERSION"),
            command_line: command_line(std::env::args_os()),
            started_at: format_rfc3339_utc(now),
            labels,
            system: SystemInfo {
                os: std::env::consts::OS,
                arch: std::env::consts::ARCH,
                cpus: std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(1),
                cpu_model: cpu_model(),
                kernel: kernel_release(),
            },
            energy_backend: None,
            measurements,
        }
    }
}

/// The CPU model, e.g. "Intel(R) Core(TM) i7-8550U CPU @ 1.80GHz" or "Apple M2".
fn cpu_model() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
        // Device-tree boards (e.g. "Raspberry Pi 4 Model B Rev 1.4")
        let board = std::fs::read_to_string("/sys/firmware/devicetree/base/model").ok();
        linux_cpu_model(&cpuinfo, board.as_deref())
    }
    #[cfg(target_os = "macos")]
    {
        sysctl_string(c"machdep.cpu.brand_string")
    }
    #[cfg(windows)]
    {
        // The registry has the marketing name; the environment variable only
        // has the family/model/stepping.
        windows_registry_cpu_name().or_else(|| std::env::var("PROCESSOR_IDENTIFIER").ok())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        None
    }
}

/// `ProcessorNameString` of the first CPU, e.g. "AMD Ryzen 7 5800X 8-Core Processor".
#[cfg(windows)]
fn windows_registry_cpu_name() -> Option<String> {
    use windows_sys::w;
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};

    let mut buf = [0u16; 256];
    let mut bytes = std::mem::size_of_val(&buf) as u32;
    // SAFETY: the key and value names are NUL-terminated wide strings; `buf` has
    // room for `bytes` bytes, which RegGetValueW updates to the size it wrote.
    let err = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!("HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0"),
            w!("ProcessorNameString"),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    if err != 0 {
        return None;
    }
    let len = (bytes as usize / 2).min(buf.len());
    let name = String::from_utf16_lossy(&buf[..len]);
    let name = name.trim_end_matches('\0').trim();
    (!name.is_empty()).then(|| name.to_string())
}

#[cfg(target_os = "macos")]
fn sysctl_string(name: &std::ffi::CStr) -> Option<String> {
    let mut buf = [0u8; 256];
    let mut len = buf.len();
    // SAFETY: `name` is NUL-terminated; `buf` has room for `len` bytes, and
    // sysctlbyname writes at most that many and updates `len`.
    let ret = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if ret != 0 {
        return None;
    }
    let value = std::ffi::CStr::from_bytes_until_nul(&buf[..len.min(buf.len())]).ok()?;
    Some(value.to_string_lossy().trim().to_string()).filter(|s| !s.is_empty())
}

/// The CPU model from `/proc/cpuinfo`. On ARM, where there is no model name
/// (#188), fall back to the device-tree board model, then to the CPU
/// implementer and part numbers (like `lscpu`).
#[cfg(any(target_os = "linux", test))]
fn linux_cpu_model(cpuinfo: &str, device_tree_model: Option<&str>) -> Option<String> {
    let field = |name: &str| {
        cpuinfo
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(key, _)| key.trim() == name)
            .map(|(_, value)| value.trim())
            .filter(|value| !value.is_empty())
    };
    if let Some(model) = ["model name", "Model", "Hardware"]
        .iter()
        .find_map(|name| field(name))
    {
        return Some(model.to_string());
    }
    if let Some(board) = device_tree_model
        .map(|m| m.trim_end_matches('\0').trim())
        .filter(|m| !m.is_empty())
    {
        return Some(board.to_string());
    }
    let parse_hex = |value: &str| u32::from_str_radix(value.trim_start_matches("0x"), 16).ok();
    let implementer = field("CPU implementer").and_then(parse_hex)?;
    let part = field("CPU part").and_then(parse_hex);
    Some(arm_cpu_name(implementer, part))
}

/// Names for the MIDR implementer and part numbers of common ARM cores.
#[cfg(any(target_os = "linux", test))]
fn arm_cpu_name(implementer: u32, part: Option<u32>) -> String {
    let vendor = match implementer {
        0x41 => "ARM",
        0x42 => "Broadcom",
        0x43 => "Cavium",
        0x46 => "Fujitsu",
        0x48 => "HiSilicon",
        0x4e => "NVIDIA",
        0x51 => "Qualcomm",
        0x53 => "Samsung",
        0x61 => "Apple",
        0x6d => "Microsoft",
        0xc0 => "Ampere",
        _ => return format!("ARM CPU (implementer {implementer:#04x})"),
    };
    let core = match (implementer, part) {
        (0x41, Some(0xd03)) => Some("Cortex-A53"),
        (0x41, Some(0xd05)) => Some("Cortex-A55"),
        (0x41, Some(0xd07)) => Some("Cortex-A57"),
        (0x41, Some(0xd08)) => Some("Cortex-A72"),
        (0x41, Some(0xd0b)) => Some("Cortex-A76"),
        (0x41, Some(0xd0c)) => Some("Neoverse-N1"),
        (0x41, Some(0xd40)) => Some("Neoverse-V1"),
        (0x41, Some(0xd49)) => Some("Neoverse-N2"),
        (0x41, Some(0xd4f)) => Some("Neoverse-V2"),
        (0xc0, Some(0xac3)) => Some("Ampere-1"),
        _ => None,
    };
    match (core, part) {
        (Some(core), _) => format!("{vendor} {core}"),
        (None, Some(part)) => format!("{vendor} CPU (part {part:#05x})"),
        (None, None) => format!("{vendor} CPU"),
    }
}

/// The kernel release from `uname`, on Unix.
fn kernel_release() -> Option<String> {
    #[cfg(unix)]
    {
        // SAFETY: an all-zero utsname is valid, and uname fills it in.
        let mut name: libc::utsname = unsafe { std::mem::zeroed() };
        // SAFETY: `name` is a valid, writable utsname.
        if unsafe { libc::uname(&mut name) } != 0 {
            return None;
        }
        // SAFETY: uname NUL-terminates the fields.
        let release = unsafe { std::ffi::CStr::from_ptr(name.release.as_ptr()) };
        Some(release.to_string_lossy().into_owned())
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// The command line, with the program reduced to its file name so that the
/// export doesn't reveal the installation path (e.g. a home directory).
fn command_line(args: impl IntoIterator<Item = std::ffi::OsString>) -> Vec<String> {
    let mut args: Vec<String> = args
        .into_iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if let Some(program) = args.first_mut() {
        if let Some(name) = std::path::Path::new(program.as_str()).file_name() {
            *program = name.to_string_lossy().into_owned();
        }
    }
    args
}

/// Parse `--label KEY=VALUE` arguments. Keys may contain letters, digits,
/// `_`, `.` and `-`; duplicate keys are rejected.
pub fn parse_labels<'a>(
    args: impl IntoIterator<Item = &'a str>,
) -> Result<BTreeMap<String, String>> {
    let mut labels = BTreeMap::new();
    for arg in args {
        let Some((key, value)) = arg.split_once('=') else {
            bail!("Invalid label '{arg}': expected KEY=VALUE");
        };
        if key.is_empty()
            || !key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
        {
            bail!("Invalid label key '{key}': use letters, digits, '_', '.' or '-'");
        }
        if labels.insert(key.to_string(), value.to_string()).is_some() {
            bail!("Duplicate label key '{key}'");
        }
    }
    Ok(labels)
}

/// Format seconds since the Unix epoch as `YYYY-MM-DDTHH:MM:SSZ`.
fn format_rfc3339_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);

    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_rfc3339() {
        assert_eq!(format_rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_rfc3339_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_rfc3339_utc(1_790_000_000), "2026-09-21T14:13:20Z");
        assert_eq!(format_rfc3339_utc(4_102_444_799), "2099-12-31T23:59:59Z");
    }

    #[test]
    fn command_line_strips_the_program_path() {
        let args =
            ["/home/alice/.cargo/bin/perfratio", "-N", "sleep 0.1"].map(std::ffi::OsString::from);
        assert_eq!(command_line(args), vec!["perfratio", "-N", "sleep 0.1"]);
    }

    #[test]
    fn measurements_reflect_requests_and_energy_availability() {
        use MeasurementStatus::*;
        let m = Measurements::new(None, false);
        assert_eq!(
            (m.energy, m.energy_unavailable_reason),
            (NotRequested, None)
        );
        assert_eq!(
            (m.time, m.memory, m.resources),
            (Measured, Measured, NotRequested)
        );

        let m = Measurements::new(Some(Ok(crate::energy::EnergyBackend::PerfPmu)), true);
        assert_eq!((m.energy, m.energy_unavailable_reason), (Measured, None));
        assert_eq!(m.resources, Measured);

        let m = Measurements::new(Some(Err(crate::energy::Unavailable::RaplUnreadable)), false);
        assert_eq!(m.energy, Unavailable);
        assert_eq!(m.energy_unavailable_reason, Some("rapl_unreadable"));
    }

    #[test]
    fn parses_labels() {
        let labels = parse_labels(["commit=abc123", "runner=graviton-3", "empty="]).unwrap();
        assert_eq!(labels["commit"], "abc123");
        assert_eq!(labels["runner"], "graviton-3");
        assert_eq!(labels["empty"], "");

        let labels = parse_labels(["url=https://x.y/?a=b"]).unwrap();
        assert_eq!(labels["url"], "https://x.y/?a=b");
    }

    #[test]
    fn rejects_invalid_labels() {
        assert!(parse_labels(["novalue"]).is_err());
        assert!(parse_labels(["=value"]).is_err());
        assert!(parse_labels(["bad key=1"]).is_err());
        assert!(parse_labels(["a=1", "a=2"]).is_err());
    }
}

#[test]
fn test_linux_cpu_model() {
    let x86 = "processor\t: 0\nvendor_id\t: GenuineIntel\nmodel name\t: Intel(R) Core(TM) i7-8550U CPU @ 1.80GHz\n";
    assert_eq!(
        linux_cpu_model(x86, None).as_deref(),
        Some("Intel(R) Core(TM) i7-8550U CPU @ 1.80GHz")
    );

    // aarch64 (e.g. an AWS Graviton2): no model name
    let arm = "processor\t: 0\nBogoMIPS\t: 243.75\nCPU implementer\t: 0x41\nCPU architecture: 8\nCPU part\t: 0xd0c\n";
    assert_eq!(
        linux_cpu_model(arm, None).as_deref(),
        Some("ARM Neoverse-N1")
    );
    // A device-tree board model wins over the core name
    assert_eq!(
        linux_cpu_model(arm, Some("Raspberry Pi 4 Model B Rev 1.4\0")).as_deref(),
        Some("Raspberry Pi 4 Model B Rev 1.4")
    );
    // Apple Silicon under virtualization reports part 0x000
    let apple = "CPU implementer\t: 0x61\nCPU part\t: 0x000\n";
    assert_eq!(
        linux_cpu_model(apple, None).as_deref(),
        Some("Apple CPU (part 0x000)")
    );
    assert_eq!(
        linux_cpu_model("CPU implementer\t: 0x99\n", None).as_deref(),
        Some("ARM CPU (implementer 0x99)")
    );
    assert_eq!(linux_cpu_model("", None), None);
    assert_eq!(linux_cpu_model("", Some("\0")), None);
}
