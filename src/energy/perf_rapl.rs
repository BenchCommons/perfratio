//! Linux perf PMU RAPL backend (`power/energy-pkg/`), usable without root
//! access to `/sys/class/powercap/*/energy_uj` when `perf_event_open` is
//! permitted (`CAP_PERFMON` or `kernel.perf_event_paranoid <= 0`).
//!
//! The sysfs parsing is pure and works on any OS so it can be unit-tested
//! against a fake tree; only the `perf_event_open` calls are Linux-only.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::fs;
use std::path::Path;

/// Default location of the `power` PMU description.
pub const POWER_PMU_ROOT: &str = "/sys/bus/event_source/devices/power";

/// Description of the `power/energy-pkg/` event read from sysfs.
#[derive(Debug, Clone, PartialEq)]
pub struct PmuConfig {
    /// PMU type for `perf_event_attr.type` (`<root>/type`)
    pub pmu_type: u32,
    /// Event config for `perf_event_attr.config` (`<root>/events/energy-pkg`)
    pub config: u64,
    /// Joules per counter increment (`<root>/events/energy-pkg.scale`)
    pub scale: f64,
    /// One CPU per package to open the counter on (`<root>/cpumask`)
    pub cpus: Vec<u32>,
}

pub fn parse_type(s: &str) -> Option<u32> {
    s.trim().parse().ok()
}

/// Parses `event=0x02` (comma-separated terms are allowed) into the config value.
pub fn parse_event_config(s: &str) -> Option<u64> {
    let term = s
        .trim()
        .split(',')
        .find_map(|t| t.trim().strip_prefix("event="))?;
    match term.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => term.parse().ok(),
    }
}

/// Parses the scale, e.g. `2.3283064365386962890625e-10`. Must be positive and finite.
pub fn parse_scale(s: &str) -> Option<f64> {
    let v: f64 = s.trim().parse().ok()?;
    (v.is_finite() && v > 0.0).then_some(v)
}

/// Only `Joules` is accepted; the kernel reports RAPL energy in Joules.
pub fn parse_unit(s: &str) -> bool {
    s.trim().eq_ignore_ascii_case("joules")
}

/// Parses a CPU list such as `0`, `0,32` or `0-1,8`.
pub fn parse_cpumask(s: &str) -> Option<Vec<u32>> {
    let mut cpus = Vec::new();
    for part in s.trim().split(',').filter(|p| !p.trim().is_empty()) {
        match part.trim().split_once('-') {
            Some((a, b)) => {
                let (a, b): (u32, u32) = (a.parse().ok()?, b.parse().ok()?);
                if a > b {
                    return None;
                }
                cpus.extend(a..=b);
            }
            None => cpus.push(part.trim().parse().ok()?),
        }
    }
    (!cpus.is_empty()).then_some(cpus)
}

/// Why the perf PMU backend could not be created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PerfRaplError {
    /// No usable `power/energy-pkg/` description in sysfs (no PMU, wrong unit, bad files)
    NotFound,
    /// `perf_event_open` or a counter read failed with this `errno`
    Os(i32),
}

impl PerfRaplError {
    /// Maps the error to a PMU status using the `pmu` diagnostics.
    pub fn pmu_status(self) -> Option<crate::pmu::PmuStatus> {
        match self {
            Self::NotFound => None,
            Self::Os(errno) => Some(crate::pmu::decide(
                None,
                Some(crate::pmu::Probe::Failed(errno)),
            )),
        }
    }

    /// True if the PMU exists but cannot be used (`rapl_unreadable`).
    /// `ENOENT`/`ENODEV` from `perf_event_open` mean the PMU is not there.
    pub fn is_unreadable(self) -> bool {
        matches!(
            self.pmu_status(),
            Some(
                crate::pmu::PmuStatus::Denied { .. }
                    | crate::pmu::PmuStatus::Other { .. }
                    | crate::pmu::PmuStatus::Unknown { .. }
            )
        )
    }
}

impl PmuConfig {
    pub fn from_sysfs(root: &Path) -> Option<Self> {
        let read = |p: &str| fs::read_to_string(root.join(p)).ok();
        if let Some(unit) = read("events/energy-pkg.unit") {
            if !parse_unit(&unit) {
                return None;
            }
        }
        Some(Self {
            pmu_type: parse_type(&read("type")?)?,
            config: parse_event_config(&read("events/energy-pkg")?)?,
            scale: parse_scale(&read("events/energy-pkg.scale")?)?,
            cpus: parse_cpumask(&read("cpumask")?)?,
        })
    }
}

/// Energy in Joules between two raw counter values (the counter is 64 bits wide).
pub fn counts_to_joules(start: u64, stop: u64, scale: f64) -> f64 {
    stop.wrapping_sub(start) as f64 * scale
}

#[cfg(target_os = "linux")]
pub use linux::PerfRaplSampler;

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use crate::energy::EnergySampler;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    const PERF_FLAG_FD_CLOEXEC: libc::c_ulong = 8;
    /// `PERF_ATTR_SIZE_VER0`
    const ATTR_SIZE: usize = 64;

    /// Sums `power/energy-pkg/` over all packages via `perf_event_open`.
    pub struct PerfRaplSampler {
        fds: Vec<OwnedFd>,
        scale: f64,
        start_readings: Option<Vec<u64>>,
    }

    fn open_counter(pmu_type: u32, config: u64, cpu: u32) -> Result<OwnedFd, PerfRaplError> {
        // Zeroed attr: counting mode, enabled-on-open is not needed because
        // `disabled` (bit 0 of the flags word) stays 0.
        let mut attr = [0u8; ATTR_SIZE];
        attr[0..4].copy_from_slice(&pmu_type.to_ne_bytes());
        attr[4..8].copy_from_slice(&(ATTR_SIZE as u32).to_ne_bytes());
        attr[8..16].copy_from_slice(&config.to_ne_bytes());
        // SAFETY: `attr` is a valid, initialized perf_event_attr of ATTR_SIZE
        // bytes; the other arguments are plain integers.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_perf_event_open,
                attr.as_ptr(),
                -1_i32,
                cpu as i32,
                -1_i32,
                PERF_FLAG_FD_CLOEXEC,
            )
        };
        if fd < 0 {
            let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            return Err(PerfRaplError::Os(errno));
        }
        // SAFETY: the kernel returned a new fd that we exclusively own.
        Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
    }

    fn read_counter(fd: &OwnedFd) -> Result<u64, PerfRaplError> {
        let mut buf = [0u8; 8];
        // SAFETY: `buf` is valid for 8 writable bytes.
        let n = unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
        if n == 8 {
            Ok(u64::from_ne_bytes(buf))
        } else {
            // Capture errno first; a short read has none, report EIO.
            let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            Err(PerfRaplError::Os(if n < 0 && errno != 0 {
                errno
            } else {
                5
            }))
        }
    }

    impl PerfRaplSampler {
        pub fn try_new() -> Result<Self, PerfRaplError> {
            Self::try_new_at(Path::new(POWER_PMU_ROOT))
        }

        /// Fails if the PMU is missing ([`PerfRaplError::NotFound`]) or any counter
        /// cannot be opened ([`PerfRaplError::Os`]), e.g. `EACCES` under a strict
        /// `perf_event_paranoid`.
        pub fn try_new_at(root: &Path) -> Result<Self, PerfRaplError> {
            let cfg = PmuConfig::from_sysfs(root).ok_or(PerfRaplError::NotFound)?;
            let fds = cfg
                .cpus
                .iter()
                .map(|&cpu| open_counter(cfg.pmu_type, cfg.config, cpu))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Self {
                fds,
                scale: cfg.scale,
                start_readings: None,
            })
        }

        fn read_all(&self) -> Result<Vec<u64>, PerfRaplError> {
            self.fds.iter().map(read_counter).collect()
        }
    }

    impl EnergySampler for PerfRaplSampler {
        fn start(&mut self) {
            self.start_readings = self.read_all().ok();
        }

        fn stop(&mut self) -> Option<f64> {
            let start = self.start_readings.take()?;
            let stop = self.read_all().ok()?;
            Some(
                start
                    .iter()
                    .zip(&stop)
                    .map(|(&a, &b)| counts_to_joules(a, b, self.scale))
                    .sum(),
            )
        }

        fn is_available(&self) -> bool {
            !self.fds.is_empty()
        }
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parses_scale_and_event() {
        assert_eq!(
            parse_scale("2.3283064365386962890625e-10\n"),
            Some(2.3283064365386963e-10)
        );
        assert_eq!(parse_scale("0"), None);
        assert_eq!(parse_scale("nan"), None);
        assert_eq!(parse_scale("x"), None);
        assert_eq!(parse_event_config("event=0x02\n"), Some(2));
        assert_eq!(parse_event_config("event=3"), Some(3));
        assert_eq!(parse_event_config("config=1"), None);
        assert_eq!(parse_type("12\n"), Some(12));
        assert_eq!(parse_type("x"), None);
        assert!(parse_unit("Joules\n"));
        assert!(!parse_unit("Watts"));
    }

    #[test]
    fn parses_cpumask() {
        assert_eq!(parse_cpumask("0\n"), Some(vec![0]));
        assert_eq!(parse_cpumask("0,32"), Some(vec![0, 32]));
        assert_eq!(parse_cpumask("0-2,8"), Some(vec![0, 1, 2, 8]));
        assert_eq!(parse_cpumask("2-1"), None);
        assert_eq!(parse_cpumask("\n"), None);
    }

    #[test]
    fn counts_convert_with_scale_and_wrap() {
        let scale = 2.3283064365386963e-10;
        let j = counts_to_joules(100, 100 + (1u64 << 32), scale);
        assert!((j - 1.0).abs() < 1e-9);
        // u64 wrap
        assert_eq!(counts_to_joules(u64::MAX, 1, 1.0), 2.0);
    }

    fn fake_pmu(root: &Path, unit: Option<&str>) {
        fs::create_dir_all(root.join("events")).unwrap();
        fs::write(root.join("type"), "14\n").unwrap();
        fs::write(root.join("cpumask"), "0\n").unwrap();
        fs::write(root.join("events/energy-pkg"), "event=0x02\n").unwrap();
        fs::write(
            root.join("events/energy-pkg.scale"),
            "2.3283064365386962890625e-10\n",
        )
        .unwrap();
        if let Some(u) = unit {
            fs::write(root.join("events/energy-pkg.unit"), u).unwrap();
        }
    }

    #[test]
    fn reads_pmu_config_from_sysfs_root() {
        let dir = tempdir().unwrap();
        fake_pmu(dir.path(), Some("Joules\n"));
        let cfg = PmuConfig::from_sysfs(dir.path()).unwrap();
        assert_eq!(cfg.pmu_type, 14);
        assert_eq!(cfg.config, 2);
        assert_eq!(cfg.cpus, vec![0]);
        assert!((cfg.scale - 2.3283064365386963e-10).abs() < 1e-20);
    }

    #[test]
    fn classifies_perf_errors() {
        use crate::pmu::PmuStatus;
        assert!(!PerfRaplError::NotFound.is_unreadable());
        assert_eq!(PerfRaplError::NotFound.pmu_status(), None);
        // EACCES, EPERM: PMU present, not allowed
        assert!(PerfRaplError::Os(13).is_unreadable());
        assert!(PerfRaplError::Os(1).is_unreadable());
        assert_eq!(
            PerfRaplError::Os(13).pmu_status(),
            Some(PmuStatus::Denied { paranoid: None })
        );
        // ENOENT, ENODEV: no such PMU
        assert!(!PerfRaplError::Os(2).is_unreadable());
        assert!(!PerfRaplError::Os(19).is_unreadable());
        // anything else is a real failure of an existing PMU
        assert!(PerfRaplError::Os(22).is_unreadable());
    }

    #[test]
    fn rejects_missing_pmu_and_wrong_unit() {
        let dir = tempdir().unwrap();
        assert_eq!(PmuConfig::from_sysfs(dir.path()), None);
        fake_pmu(dir.path(), Some("Watts\n"));
        assert_eq!(PmuConfig::from_sysfs(dir.path()), None);
    }
}
