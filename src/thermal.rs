//! CPU temperature and thermal throttling counters from Linux sysfs, ACPI thermal
//! zone temperatures on Windows (PDH performance counter, no admin rights) and the
//! coarse thermal pressure level on macOS. One reader serves `--check-system`,
//! the in-run monitoring of `interference.thermal` / `interference.thermal_pressure`
//! and `--cooldown`. See `docs/specs/cgroup-thermal.md` and
//! `docs/specs/thermal-macos-windows.md`.
//!
//! Every function takes the `/` to read below, so tests use fixture trees
//! (`system_check::FAKE_ROOT_ENV` selects one for the whole binary). A source
//! that cannot be read gives `None` or an empty list: never a temperature of 0.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::system_check::entries;

/// The root to read `/sys` and `/proc` below: `/`, or the fixture directory
/// named by [`crate::system_check::FAKE_ROOT_ENV`]
pub fn default_root() -> PathBuf {
    crate::system_check::fake_root()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Temperatures in °C of the thermal zones and the hwmon sensors, unreadable
/// and implausible values (the kernel reports -274 °C and 0 for a broken
/// sensor) left out.
pub fn temperatures(root: &Path) -> Vec<f64> {
    let zones = entries(root, "/sys/class/thermal", "thermal_zone")
        .into_iter()
        .map(|zone| zone.join("temp"));
    let hwmon = entries(root, "/sys/class/hwmon", "hwmon")
        .into_iter()
        .flat_map(|dir| entries_of(&dir, "temp"))
        .filter(|p| p.to_string_lossy().ends_with("_input"));
    let mut found: Vec<f64> = zones
        .chain(hwmon)
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .filter_map(|text| parse_millidegrees(&text))
        .collect();
    // Only the real host has the Windows counter: a fixture root (tests) does not
    if root == Path::new("/") {
        found.extend(host_zone_temperatures());
    }
    found
}

/// `Thermal Zone Information\Temperature` is in Kelvin (a whole number). 300 K -> `26.85`;
/// `None` for a reading that is not a plausible temperature (0 K for a zone without
/// data, NaN, a value already in °C).
pub fn kelvin_to_celsius(kelvin: f64) -> Option<f64> {
    let celsius = kelvin - 273.15;
    (celsius > 0.0 && celsius < 150.0).then_some(celsius)
}

/// Temperatures in °C from the Kelvin readings of every thermal zone instance,
/// implausible ones left out
pub fn celsius_from_kelvin_readings(readings: &[f64]) -> Vec<f64> {
    readings
        .iter()
        .copied()
        .filter_map(kelvin_to_celsius)
        .collect()
}

/// ACPI thermal zone temperatures of the Windows host in °C: empty when the
/// counter or its instances do not exist (many desktops and VMs)
#[cfg(windows)]
fn host_zone_temperatures() -> Vec<f64> {
    celsius_from_kelvin_readings(&pdh::zone_kelvin_readings())
}

#[cfg(not(windows))]
fn host_zone_temperatures() -> Vec<f64> {
    Vec::new()
}

/// Reads `\Thermal Zone Information(*)\Temperature` through PDH. It needs no
/// administrator rights, unlike `MSAcpi_ThermalZoneTemperature` over WMI.
#[cfg(windows)]
mod pdh {
    use std::sync::{Mutex, OnceLock};
    use windows_sys::Win32::System::Performance::{
        PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
        PdhOpenQueryW, PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE_ITEM_W,
        PDH_FMT_DOUBLE, PDH_MORE_DATA,
    };

    /// Handles of a PDH query and its one counter, kept for the life of the process
    struct Query {
        query: isize,
        counter: isize,
    }

    static QUERY: OnceLock<Option<Mutex<Query>>> = OnceLock::new();

    fn open() -> Option<Mutex<Query>> {
        let mut query: isize = 0;
        // SAFETY: a null data source means the live counters; `query` is a valid out-pointer.
        if unsafe { PdhOpenQueryW(std::ptr::null(), 0, &mut query) } != 0 {
            return None;
        }
        let path: Vec<u16> = r"\Thermal Zone Information(*)\Temperature"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut counter: isize = 0;
        // SAFETY: `query` was opened above, `path` is NUL-terminated and outlives the call,
        // `counter` is a valid out-pointer.
        let status = unsafe { PdhAddEnglishCounterW(query, path.as_ptr(), 0, &mut counter) };
        if status != 0 {
            // SAFETY: `query` is open and not used after this.
            unsafe { PdhCloseQuery(query) };
            return None;
        }
        Some(Mutex::new(Query { query, counter }))
    }

    /// One Kelvin reading per thermal zone instance; empty when there is no
    /// counter, no instance or no valid data
    pub fn zone_kelvin_readings() -> Vec<f64> {
        let Some(shared) = QUERY.get_or_init(open) else {
            return Vec::new();
        };
        let Ok(guard) = shared.lock() else {
            return Vec::new();
        };
        let (query, counter) = (guard.query, guard.counter);
        // SAFETY: `query` is open (see `open`).
        if unsafe { PdhCollectQueryData(query) } != 0 {
            return Vec::new();
        }
        let mut bytes: u32 = 0;
        let mut count: u32 = 0;
        // SAFETY: the sizing call: a null buffer with size 0 is how PDH reports the size needed.
        let status = unsafe {
            PdhGetFormattedCounterArrayW(
                counter,
                PDH_FMT_DOUBLE,
                &mut bytes,
                &mut count,
                std::ptr::null_mut(),
            )
        };
        if status != PDH_MORE_DATA || bytes == 0 {
            return Vec::new();
        }
        // u64 elements: the items hold pointers and an f64, they need 8-byte alignment
        let mut buffer: Vec<u64> = vec![0; (bytes as usize).div_ceil(8)];
        let items = buffer.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();
        // SAFETY: `buffer` holds at least `bytes` bytes, aligned for the items.
        let status = unsafe {
            PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut bytes, &mut count, items)
        };
        if status != 0 {
            return Vec::new();
        }
        // SAFETY: PDH filled `count` items at the start of `buffer`, which lives to the
        // end of this function; the values are read as f64 because of PDH_FMT_DOUBLE.
        let items = unsafe { std::slice::from_raw_parts(items, count as usize) };
        items
            .iter()
            .filter(|item| {
                item.FmtValue.CStatus == PDH_CSTATUS_VALID_DATA
                    || item.FmtValue.CStatus == PDH_CSTATUS_NEW_DATA
            })
            // SAFETY: `doubleValue` is the member set for PDH_FMT_DOUBLE.
            .map(|item| unsafe { item.FmtValue.Anonymous.doubleValue })
            .collect()
    }
}

/// Warns once that `--thermal` finds no sensor on Windows; other platforms stay silent
/// (Linux without sensors always was, macOS has its own pressure level)
pub fn warn_no_sensor_once(root: &Path) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    if cfg!(windows) && root == Path::new("/") && !WARNED.swap(true, Ordering::SeqCst) {
        eprintln!(
            "Warning: '--thermal': this Windows host has no 'Thermal Zone Information' \
             temperature counter instance (common on desktops and VMs); nothing is sampled and \
             'interference.thermal' is absent from the export."
        );
    }
}

fn entries_of(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
        .map(|e| e.path())
        .collect();
    paths.sort();
    paths
}

/// `45000` (millidegrees) -> `45.0`; `None` for text that is not a plausible reading
pub fn parse_millidegrees(text: &str) -> Option<f64> {
    let millidegrees: f64 = text.trim().parse().ok()?;
    let celsius = millidegrees / 1000.0;
    (celsius > 0.0 && celsius < 150.0).then_some(celsius)
}

/// The hottest sensor in °C, `None` without a readable sensor
pub fn max_temperature(root: &Path) -> Option<f64> {
    temperatures(root).into_iter().reduce(f64::max)
}

/// Number of thermal throttling events the CPUs reported so far (Intel
/// `thermal_throttle/core_throttle_count` and `package_throttle_count`), summed.
/// `None` when no CPU exposes the counters.
pub fn throttle_events(root: &Path) -> Option<u64> {
    let mut total = None;
    for cpu in entries(root, "/sys/devices/system/cpu", "cpu") {
        for name in ["core_throttle_count", "package_throttle_count"] {
            let path = cpu.join("thermal_throttle").join(name);
            if let Some(count) = std::fs::read_to_string(path)
                .ok()
                .and_then(|t| t.trim().parse::<u64>().ok())
            {
                *total.get_or_insert(0) += count;
            }
        }
    }
    total
}

/// `interference.thermal` of a result
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ThermalTrace {
    /// Hottest sensor before the first run, °C
    pub start_c: f64,
    /// Hottest sensor after the last run, °C
    pub end_c: f64,
    /// Highest reading in between, °C
    pub max_c: f64,
    /// Thermal throttling events the CPUs reported during the benchmark.
    /// Omitted when the CPUs expose no counter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub throttle_events: Option<u64>,
}

/// Samples the temperature around a benchmark. Reads are rate limited, so
/// short runs do not pay for slow ACPI sensors.
#[derive(Debug)]
pub struct ThermalMonitor {
    root: PathBuf,
    start: f64,
    last: f64,
    max: f64,
    throttle_start: Option<u64>,
    last_sample: std::time::Instant,
}

/// Fewest time between two temperature reads while the benchmark runs
pub const SAMPLE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

impl ThermalMonitor {
    /// `None` when no temperature sensor is readable
    pub fn start(root: &Path) -> Option<Self> {
        let now = max_temperature(root)?;
        Some(ThermalMonitor {
            root: root.to_path_buf(),
            start: now,
            last: now,
            max: now,
            throttle_start: throttle_events(root),
            last_sample: std::time::Instant::now(),
        })
    }

    /// Read again if [`SAMPLE_INTERVAL`] has passed since the last read
    pub fn sample(&mut self) {
        if self.last_sample.elapsed() >= SAMPLE_INTERVAL {
            self.read();
        }
    }

    fn read(&mut self) {
        self.last_sample = std::time::Instant::now();
        if let Some(now) = max_temperature(&self.root) {
            self.last = now;
            self.max = self.max.max(now);
        }
    }

    /// The trace so far, without reading the sensors again
    pub fn trace(&self) -> ThermalTrace {
        let throttle_events = match (self.throttle_start, throttle_events(&self.root)) {
            (Some(start), Some(end)) => Some(end.saturating_sub(start)),
            _ => None,
        };
        ThermalTrace {
            start_c: self.start,
            end_c: self.last,
            max_c: self.max,
            throttle_events,
        }
    }

    /// The trace of the benchmark, with a final read
    pub fn finish(mut self) -> ThermalTrace {
        self.read();
        let throttle_events = match (self.throttle_start, throttle_events(&self.root)) {
            (Some(start), Some(end)) => Some(end.saturating_sub(start)),
            _ => None,
        };
        ThermalTrace {
            start_c: self.start,
            end_c: self.last,
            max_c: self.max,
            throttle_events,
        }
    }
}

/// Coarse thermal state of the whole machine, as the OS reports it. macOS: the
/// `com.apple.system.thermalpressurelevel` notification state (the levels of
/// `<libkern/OSThermalNotification.h>`, what `ProcessInfo.thermalState` maps
/// from). It is not a temperature: it rises when the OS starts limiting
/// performance for thermal reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PressureLevel {
    Nominal,
    Moderate,
    Heavy,
    Trapping,
    Sleeping,
}

impl PressureLevel {
    /// `kOSThermalPressureLevel*` value -> level; `None` for an unknown value
    pub fn from_raw(raw: u64) -> Option<Self> {
        Some(match raw {
            0 => PressureLevel::Nominal,
            1 => PressureLevel::Moderate,
            2 => PressureLevel::Heavy,
            3 => PressureLevel::Trapping,
            4 => PressureLevel::Sleeping,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            PressureLevel::Nominal => "nominal",
            PressureLevel::Moderate => "moderate",
            PressureLevel::Heavy => "heavy",
            PressureLevel::Trapping => "trapping",
            PressureLevel::Sleeping => "sleeping",
        }
    }
}

/// The current thermal pressure level of the host. Only the real host has one:
/// a fixture `root` (tests) and platforms without a source give `None`.
pub fn pressure_level(root: &Path) -> Option<PressureLevel> {
    if root != Path::new("/") {
        return None;
    }
    read_pressure_level()
}

#[cfg(target_os = "macos")]
fn read_pressure_level() -> Option<PressureLevel> {
    use std::sync::OnceLock;

    // libSystem, <notify.h>: no root needed
    extern "C" {
        fn notify_register_check(name: *const libc::c_char, token: *mut libc::c_int) -> u32;
        fn notify_get_state(token: libc::c_int, state: *mut u64) -> u32;
    }
    const NOTIFY_STATUS_OK: u32 = 0;
    static TOKEN: OnceLock<Option<libc::c_int>> = OnceLock::new();

    let token = (*TOKEN.get_or_init(|| {
        let mut token: libc::c_int = 0;
        // SAFETY: a NUL-terminated name and a valid out-pointer; the token is
        // kept for the life of the process (never cancelled).
        let status = unsafe {
            notify_register_check(
                c"com.apple.system.thermalpressurelevel".as_ptr(),
                &mut token,
            )
        };
        (status == NOTIFY_STATUS_OK).then_some(token)
    }))?;
    let mut state: u64 = 0;
    // SAFETY: a token registered above and a valid out-pointer.
    let status = unsafe { notify_get_state(token, &mut state) };
    if status != NOTIFY_STATUS_OK {
        return None;
    }
    PressureLevel::from_raw(state)
}

/// No coarse thermal state is read on Linux and Windows (they have temperatures,
/// see `docs/specs/thermal-macos-windows.md`).
#[cfg(not(target_os = "macos"))]
fn read_pressure_level() -> Option<PressureLevel> {
    None
}

/// `interference.thermal_pressure` of a result (macOS)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PressureTrace {
    /// Level before the first run
    pub start: PressureLevel,
    /// Level after the last run
    pub end: PressureLevel,
    /// Highest level in between
    pub max: PressureLevel,
}

/// Samples the thermal pressure level around a benchmark
#[derive(Debug)]
pub struct PressureMonitor {
    root: PathBuf,
    trace: PressureTrace,
}

impl PressureMonitor {
    /// `None` when the host reports no pressure level
    pub fn start(root: &Path) -> Option<Self> {
        Self::starting_at(root, pressure_level(root)?)
    }

    fn starting_at(root: &Path, now: PressureLevel) -> Option<Self> {
        Some(PressureMonitor {
            root: root.to_path_buf(),
            trace: PressureTrace {
                start: now,
                end: now,
                max: now,
            },
        })
    }

    pub fn sample(&mut self) {
        if let Some(now) = pressure_level(&self.root) {
            self.record(now);
        }
    }

    fn record(&mut self, now: PressureLevel) {
        self.trace.end = now;
        self.trace.max = self.trace.max.max(now);
    }

    pub fn trace(&self) -> PressureTrace {
        self.trace
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

    #[test]
    fn pressure_levels_map_the_os_values_in_order() {
        assert_eq!(PressureLevel::from_raw(0), Some(PressureLevel::Nominal));
        assert_eq!(PressureLevel::from_raw(4), Some(PressureLevel::Sleeping));
        assert_eq!(PressureLevel::from_raw(5), None);
        assert!(PressureLevel::Nominal < PressureLevel::Heavy);
        assert_eq!(
            serde_json::to_string(&PressureLevel::Moderate).unwrap(),
            "\"moderate\""
        );
        for raw in 0..5 {
            let level = PressureLevel::from_raw(raw).unwrap();
            assert_eq!(serde_json::to_value(level).unwrap(), level.as_str());
        }
    }

    #[test]
    fn a_fixture_root_has_no_pressure_level() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(pressure_level(dir.path()), None);
        assert!(PressureMonitor::start(dir.path()).is_none());
    }

    #[test]
    fn the_pressure_monitor_keeps_start_end_and_max() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = PressureMonitor::starting_at(dir.path(), PressureLevel::Nominal).unwrap();
        m.record(PressureLevel::Heavy);
        m.record(PressureLevel::Moderate);
        assert_eq!(
            m.trace(),
            PressureTrace {
                start: PressureLevel::Nominal,
                end: PressureLevel::Moderate,
                max: PressureLevel::Heavy
            }
        );
    }

    /// macOS reports a level without root (checked on macOS 27.2, M4)
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_reports_a_pressure_level() {
        assert!(pressure_level(Path::new("/")).is_some());
    }

    #[test]
    fn kelvin_readings_become_celsius() {
        assert!((kelvin_to_celsius(300.0).unwrap() - 26.85).abs() < 1e-9);
        assert!((kelvin_to_celsius(373.0).unwrap() - 99.85).abs() < 1e-9);
        // 0 K (no data), a value already in °C, NaN and an absurd one are not temperatures
        for bad in [0.0, 27.0, 273.15, 500.0, f64::NAN, f64::INFINITY, -5.0] {
            assert_eq!(kelvin_to_celsius(bad), None, "{bad}");
        }
    }

    #[test]
    fn zone_readings_leave_out_the_implausible_ones() {
        let found = celsius_from_kelvin_readings(&[0.0, 310.0, 303.15, 1.0]);
        assert_eq!(found.len(), 2);
        assert!((found[0] - 36.85).abs() < 1e-9);
        assert!((found[1] - 30.0).abs() < 1e-9);
        assert!(celsius_from_kelvin_readings(&[]).is_empty());
    }

    /// The Windows counter is read for the real host only: a fixture root has none
    #[test]
    fn a_fixture_root_reads_no_host_zone() {
        let dir = tempfile::tempdir().unwrap();
        assert!(temperatures(dir.path()).is_empty());
    }

    /// The PDH counter may be absent (VMs, desktops) or present, and the read must
    /// not fail either way. A present sensor gives a plausible temperature.
    #[cfg(windows)]
    #[test]
    fn windows_zone_counter_is_absent_or_plausible() {
        let readings = pdh::zone_kelvin_readings();
        let celsius = max_temperature(Path::new("/"));
        panic!("DIAG readings={readings:?} celsius={celsius:?}");
        match celsius {
            Some(c) => assert!(c > 0.0 && c < 150.0, "{c} °C from {readings:?}"),
            None => assert!(celsius_from_kelvin_readings(&readings).is_empty()),
        }
    }

    #[test]
    fn reads_zones_and_hwmon_sensors() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "sys/class/thermal/thermal_zone0/temp", "45000\n");
        write(root, "sys/class/thermal/thermal_zone1/temp", "61500\n");
        write(root, "sys/class/hwmon/hwmon2/temp1_input", "70250\n");
        write(root, "sys/class/hwmon/hwmon2/temp1_label", "Package\n");
        let mut temps = temperatures(root);
        temps.sort_by(f64::total_cmp);
        assert_eq!(temps, [45.0, 61.5, 70.25]);
        assert_eq!(max_temperature(root), Some(70.25));
    }

    #[test]
    fn broken_sensors_are_left_out_never_zero() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "sys/class/thermal/thermal_zone0/temp", "0\n");
        write(root, "sys/class/thermal/thermal_zone1/temp", "-274000\n");
        write(
            root,
            "sys/class/thermal/thermal_zone2/temp",
            "not a number\n",
        );
        write(root, "sys/class/thermal/thermal_zone3/temp", "50000\n");
        assert_eq!(temperatures(root), [50.0]);
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(max_temperature(empty.path()), None);
        assert!(ThermalMonitor::start(empty.path()).is_none());
    }

    #[test]
    fn sums_the_throttle_counters_or_reports_none() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert_eq!(throttle_events(root), None);
        write(
            root,
            "sys/devices/system/cpu/cpu0/thermal_throttle/core_throttle_count",
            "3\n",
        );
        write(
            root,
            "sys/devices/system/cpu/cpu0/thermal_throttle/package_throttle_count",
            "4\n",
        );
        write(
            root,
            "sys/devices/system/cpu/cpu1/thermal_throttle/core_throttle_count",
            "5\n",
        );
        assert_eq!(throttle_events(root), Some(12));
    }

    #[test]
    fn the_monitor_traces_start_end_and_max_and_counts_new_throttle_events() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let zone = "sys/class/thermal/thermal_zone0/temp";
        let count = "sys/devices/system/cpu/cpu0/thermal_throttle/core_throttle_count";
        write(root, zone, "50000\n");
        write(root, count, "7\n");
        let mut monitor = ThermalMonitor::start(root).unwrap();
        write(root, zone, "80000\n");
        // too soon: not read again
        monitor.sample();
        assert_eq!(monitor.max, 50.0);
        monitor.last_sample -= SAMPLE_INTERVAL;
        monitor.sample();
        assert_eq!(monitor.max, 80.0);
        write(root, zone, "65000\n");
        write(root, count, "9\n");
        let trace = monitor.finish();
        assert_eq!(
            trace,
            ThermalTrace {
                start_c: 50.0,
                end_c: 65.0,
                max_c: 80.0,
                throttle_events: Some(2)
            }
        );
    }
}
