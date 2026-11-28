//! CPU temperature and thermal throttling counters from Linux sysfs. One
//! reader serves `--check-system`, the in-run monitoring of `interference.thermal`
//! and `--cooldown`. See `docs/specs/cgroup-thermal.md`.
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
    zones
        .chain(hwmon)
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .filter_map(|text| parse_millidegrees(&text))
        .collect()
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
