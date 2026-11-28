//! `--check-system`: report environment conditions that make benchmark results
//! noisy or irreproducible (CPU frequency scaling, turbo boost, load, battery,
//! thermal throttling) before benchmarking, with a hint for each problem.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::output::colors;

/// Hidden testing hook: read the Linux `/proc` and `/sys` files below this
/// directory instead of `/`, on any platform.
pub const FAKE_ROOT_ENV: &str = "PERFRATIO_SYSTEM_CHECK_ROOT";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Nothing to worry about
    Ok,
    /// A capability that is unavailable but not needed by default; passes
    /// `--check-system=strict`
    Info,
    /// A condition that often adds noise (load, battery, heat)
    Warn,
    /// A setting that is known to add noise (frequency scaling, turbo boost)
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
    pub hint: Option<String>,
}

impl Check {
    fn new(name: &'static str, status: Status, detail: impl Into<String>) -> Self {
        Check {
            name,
            status,
            detail: detail.into(),
            hint: None,
        }
    }

    fn with_hint(mut self, hint: impl Into<String>) -> Self {
        if self.status != Status::Ok {
            self.hint = Some(hint.into());
        }
        self
    }
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let symbol = match self.status {
            Status::Ok => colors::green("✔"),
            Status::Info => colors::yellow("i"),
            Status::Warn => colors::yellow("!"),
            Status::Fail => colors::red("✖"),
        };
        write!(f, "  {symbol} {:<16} {}", self.name, self.detail)?;
        if let Some(hint) = &self.hint {
            write!(f, "  → {hint}")?;
        }
        Ok(())
    }
}

/// Run all checks available on this platform.
pub fn run_checks() -> Vec<Check> {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    if let Some(root) = std::env::var_os(FAKE_ROOT_ENV) {
        return linux_checks(Path::new(&root), cpus);
    }
    #[cfg(target_os = "linux")]
    {
        linux_checks(Path::new("/"), cpus)
    }
    #[cfg(target_os = "macos")]
    {
        macos_checks(cpus)
    }
    #[cfg(target_os = "windows")]
    {
        let _ = cpus;
        windows_checks()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = cpus;
        Vec::new()
    }
}

/// Scheduler events of `--counters` (#220): informational when the kernel
/// scope they need is denied. `None` when they were not requested or work.
pub fn scheduler_check(config: Option<&crate::perf_counters::CounterConfig>) -> Option<Check> {
    use crate::perf_counters::{Availability, CounterKind, UnavailableReason};
    let config = config?;
    if !config
        .requested
        .iter()
        .any(|s| s.kind == CounterKind::CpuMigrations)
    {
        return None;
    }
    let denied = match &config.availability {
        Availability::Unavailable(UnavailableReason::KernelScopeDenied) => true,
        Availability::Partial(events) => {
            events.iter().any(|s| s.kind == CounterKind::CpuMigrations)
                && config.partial_reason == Some(UnavailableReason::KernelScopeDenied)
        }
        _ => false,
    };
    denied.then(|| {
        Check::new(
            "Scheduler migrations",
            Status::Info,
            "not measurable (kernel scope denied)",
        )
        .with_hint("sudo sysctl kernel.perf_event_paranoid=1")
    })
}

/// `--jobs` with pinned workers (#307): the CPUs of the workers, and a warning
/// when workers share a physical core (more workers than physical cores).
/// `None` when the workers are not pinned.
pub fn worker_cpus_check(
    partition: Option<&crate::util::cpu_partition::Partition>,
) -> Option<Check> {
    let partition = partition?;
    let reserved = if partition.reserved.is_empty() {
        String::new()
    } else {
        format!(", {:?} kept for perfratio", partition.reserved)
    };
    Some(
        match crate::util::cpu_partition::describe_sharing(partition) {
            Some(sharing) => {
                Check::new("Worker CPUs", Status::Warn, format!("{sharing}{reserved}")).with_hint(
                    "use at most as many --jobs as physical cores, or --pin-workers=none",
                )
            }
            None => Check::new(
                "Worker CPUs",
                Status::Ok,
                format!(
                    "{} workers on disjoint cores {:?}{reserved}",
                    partition.workers.len(),
                    partition.workers
                ),
            ),
        },
    )
}

/// The report shown before benchmarking.
pub fn report(checks: &[Check]) -> String {
    let mut out = String::from("System check:\n");
    if checks.is_empty() {
        out.push_str("  (no checks are available on this platform)\n");
    }
    for check in checks {
        out.push_str(&format!("{check}\n"));
    }
    out
}

/// Whether every check passed.
pub fn all_passed(checks: &[Check]) -> bool {
    checks
        .iter()
        .all(|c| matches!(c.status, Status::Ok | Status::Info))
}

// ---------------------------------------------------------------------------
// Linux (sysfs / procfs below `root`)

pub(crate) fn read(root: &Path, path: &str) -> Option<String> {
    std::fs::read_to_string(root.join(path.trim_start_matches('/')))
        .ok()
        .map(|s| s.trim().to_string())
}

/// Entries of a directory below `root` whose name starts with `prefix`, sorted.
pub(crate) fn entries(root: &Path, dir: &str, prefix: &str) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(root.join(dir.trim_start_matches('/')))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
        .map(|e| e.path())
        .collect();
    paths.sort();
    paths
}

fn linux_checks(root: &Path, cpus: usize) -> Vec<Check> {
    let mut checks = Vec::new();

    let governors: Vec<String> = entries(root, "/sys/devices/system/cpu", "cpu")
        .iter()
        .filter_map(|cpu| std::fs::read_to_string(cpu.join("cpufreq/scaling_governor")).ok())
        .map(|g| g.trim().to_string())
        .collect();
    if let Some(check) = governor_check(&governors) {
        checks.push(check);
    }

    let intel_no_turbo = read(root, "/sys/devices/system/cpu/intel_pstate/no_turbo");
    let boost = read(root, "/sys/devices/system/cpu/cpufreq/boost");
    if let Some(check) = turbo_check(intel_no_turbo.as_deref(), boost.as_deref()) {
        checks.push(check);
    }

    if let Some(load) = read(root, "/proc/loadavg").and_then(|l| parse_loadavg(&l)) {
        checks.push(load_check(load, cpus, LINUX_LOAD_PER_CPU));
    }

    let supplies: Vec<(String, String)> = entries(root, "/sys/class/power_supply", "")
        .iter()
        .filter_map(|supply| {
            let kind = std::fs::read_to_string(supply.join("type")).ok()?;
            let online = std::fs::read_to_string(supply.join("online")).unwrap_or_default();
            Some((kind.trim().to_string(), online.trim().to_string()))
        })
        .collect();
    if let Some(check) = power_check_linux(&supplies) {
        checks.push(check);
    }

    // the reader `interference.thermal` and `--cooldown` use too
    let temperatures = crate::thermal::temperatures(root);
    if let Some(check) = thermal_check(&temperatures) {
        checks.push(check);
    }

    let paranoid = read(root, "/proc/sys/kernel/perf_event_paranoid")
        .and_then(|c| crate::pmu::parse_paranoid(&c));
    if let Some(check) = pmu_check(paranoid, pmu_probe(root)) {
        checks.push(check);
    }

    checks
}

/// The real probe runs only against the real `/`, never a fake root.
#[cfg(target_os = "linux")]
fn pmu_probe(root: &Path) -> Option<crate::pmu::Probe> {
    (root == Path::new("/")).then(crate::pmu::probe)
}

#[cfg(not(target_os = "linux"))]
fn pmu_probe(_root: &Path) -> Option<crate::pmu::Probe> {
    None
}

/// Hardware counters (`--counters`) are optional, so a problem is informational.
fn pmu_check(paranoid: Option<i32>, probe: Option<crate::pmu::Probe>) -> Option<Check> {
    if paranoid.is_none() && probe.is_none() {
        return None;
    }
    let status = crate::pmu::decide(paranoid, probe);
    let level = if crate::pmu::is_problem(&status) {
        Status::Info
    } else {
        Status::Ok
    };
    let check = Check::new("HW counters", level, crate::pmu::summary(&status));
    Some(match crate::pmu::hint(&status) {
        Some(hint) => check.with_hint(hint),
        None => check,
    })
}

/// All CPUs should use the `performance` governor.
fn governor_check(governors: &[String]) -> Option<Check> {
    if governors.is_empty() {
        return None;
    }
    // e.g. "powersave on 6 CPUs, performance on 2 CPUs"
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for governor in governors {
        match counts.iter_mut().find(|(g, _)| g == governor) {
            Some((_, n)) => *n += 1,
            None => counts.push((governor, 1)),
        }
    }
    let detail = counts
        .iter()
        .map(|(g, n)| format!("{g} on {n} CPU{}", if *n == 1 { "" } else { "s" }))
        .collect::<Vec<_>>()
        .join(", ");
    let ok = governors.iter().all(|g| g == "performance");
    Some(
        Check::new(
            "CPU governor",
            if ok { Status::Ok } else { Status::Fail },
            detail,
        )
        .with_hint("sudo cpupower frequency-set -g performance"),
    )
}

/// Turbo boost makes the clock depend on temperature and load.
fn turbo_check(intel_no_turbo: Option<&str>, boost: Option<&str>) -> Option<Check> {
    let (enabled, hint) = match (intel_no_turbo, boost) {
        (Some(no_turbo), _) => (
            no_turbo == "0",
            "echo 1 | sudo tee /sys/devices/system/cpu/intel_pstate/no_turbo",
        ),
        (None, Some(boost)) => (
            boost == "1",
            "echo 0 | sudo tee /sys/devices/system/cpu/cpufreq/boost",
        ),
        (None, None) => return None,
    };
    Some(
        Check::new(
            "Turbo boost",
            if enabled { Status::Fail } else { Status::Ok },
            if enabled { "enabled" } else { "disabled" },
        )
        .with_hint(hint),
    )
}

/// First field of `/proc/loadavg`: the 1-minute load average.
fn parse_loadavg(line: &str) -> Option<f64> {
    line.split_whitespace().next()?.parse().ok()
}

/// Linux: the load average counts runnable (and uninterruptible) tasks, so a
/// quiet machine is close to 0.
const LINUX_LOAD_PER_CPU: f64 = 0.1;
/// macOS: the load average also counts many briefly waiting threads and sits
/// around 2–6 on an ordinary idle desktop (#188), so allow more.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
const MACOS_LOAD_PER_CPU: f64 = 0.5;

/// Other processes that keep CPUs busy compete with the benchmark.
fn load_check(load: f64, cpus: usize, per_cpu: f64) -> Check {
    let limit = (per_cpu * cpus as f64).max(1.0);
    Check::new(
        "Load average",
        if load < limit {
            Status::Ok
        } else {
            Status::Warn
        },
        format!("{load:.2} ({cpus} CPUs)"),
    )
    .with_hint("stop other programs, or benchmark on an idle machine")
}

/// `(type, online)` of each `/sys/class/power_supply` entry.
fn power_check_linux(supplies: &[(String, String)]) -> Option<Check> {
    let mains: Vec<&str> = supplies
        .iter()
        .filter(|(kind, _)| kind == "Mains")
        .map(|(_, online)| online.as_str())
        .collect();
    if mains.is_empty() {
        // Desktops and servers usually expose no power supply at all
        return None;
    }
    let on_battery = mains.iter().all(|&online| online == "0");
    Some(power_check(on_battery))
}

fn power_check(on_battery: bool) -> Check {
    Check::new(
        "Power source",
        if on_battery { Status::Warn } else { Status::Ok },
        if on_battery { "battery" } else { "AC" },
    )
    .with_hint("connect the power adapter (on battery, CPUs are often clocked down)")
}

/// Temperatures in °C: a hot CPU gets throttled.
fn thermal_check(temperatures: &[f64]) -> Option<Check> {
    let max = temperatures.iter().copied().fold(f64::NAN, f64::max);
    if max.is_nan() {
        return None;
    }
    Some(
        Check::new(
            "Thermal",
            if max < 85.0 { Status::Ok } else { Status::Warn },
            format!("max {max:.0} °C"),
        )
        .with_hint("let the machine cool down, or improve cooling"),
    )
}

// ---------------------------------------------------------------------------
// macOS

#[cfg(target_os = "macos")]
fn macos_checks(cpus: usize) -> Vec<Check> {
    let mut checks = Vec::new();

    let mut loads = [0.0f64; 3];
    // SAFETY: `loads` has room for the 3 values requested.
    if unsafe { libc::getloadavg(loads.as_mut_ptr(), 3) } >= 1 {
        checks.push(load_check(loads[0], cpus, MACOS_LOAD_PER_CPU));
    }

    let pmset = |args: &[&str]| {
        std::process::Command::new("pmset")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    };
    if let Some(on_battery) = pmset(&["-g", "batt"]).and_then(|out| parse_pmset_batt(&out)) {
        checks.push(power_check(on_battery));
    }
    if let Some(low_power) = pmset(&["-g"]).and_then(|out| parse_pmset_low_power(&out)) {
        checks.push(
            Check::new(
                "Low Power Mode",
                if low_power { Status::Warn } else { Status::Ok },
                if low_power { "on" } else { "off" },
            )
            .with_hint("turn off Low Power Mode in System Settings → Battery"),
        );
    }
    if let Some(limit) = pmset(&["-g", "therm"]).and_then(|out| parse_pmset_speed_limit(&out)) {
        checks.push(
            Check::new(
                "Thermal",
                if limit >= 100 {
                    Status::Ok
                } else {
                    Status::Warn
                },
                format!("CPU speed limit {limit}%"),
            )
            .with_hint("let the machine cool down, or improve cooling"),
        );
    }
    if let Some(level) = crate::thermal::pressure_level(Path::new("/")) {
        checks.push(pressure_check(level));
    }

    checks
}

/// The coarse macOS thermal pressure level (no temperature without root)
#[cfg(any(target_os = "macos", test))]
fn pressure_check(level: crate::thermal::PressureLevel) -> Check {
    let nominal = level == crate::thermal::PressureLevel::Nominal;
    Check::new(
        "Thermal pressure",
        if nominal { Status::Ok } else { Status::Warn },
        level.as_str(),
    )
    .with_hint("let the machine cool down, or improve cooling")
}

/// `pmset -g batt`: "Now drawing from 'Battery Power'" / "'AC Power'".
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
fn parse_pmset_batt(output: &str) -> Option<bool> {
    let line = output.lines().find(|l| l.contains("drawing from"))?;
    Some(line.contains("Battery Power"))
}

/// `pmset -g`: " lowpowermode         1"
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
fn parse_pmset_low_power(output: &str) -> Option<bool> {
    let line = output
        .lines()
        .find(|l| l.split_whitespace().next() == Some("lowpowermode"))?;
    Some(line.split_whitespace().nth(1)? == "1")
}

/// `pmset -g therm`: "CPU_Speed_Limit \t= 100" (absent if never throttled).
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
fn parse_pmset_speed_limit(output: &str) -> Option<u32> {
    let line = output.lines().find(|l| l.contains("CPU_Speed_Limit"))?;
    line.split('=').nth(1)?.trim().parse().ok()
}

// ---------------------------------------------------------------------------
// Windows

#[cfg(target_os = "windows")]
fn windows_checks() -> Vec<Check> {
    let mut checks = Vec::new();

    // 1. Power source
    unsafe {
        let mut status: windows_sys::Win32::System::Power::SYSTEM_POWER_STATUS = std::mem::zeroed();
        // SAFETY: `status` is a valid, writable pointer to SYSTEM_POWER_STATUS.
        if windows_sys::Win32::System::Power::GetSystemPowerStatus(&mut status) != 0 {
            let battery_saver = status.SystemStatusFlag == 1;
            if let Some(check) = power_check_windows(status.ACLineStatus, battery_saver) {
                checks.push(check);
            }
        }
    }

    // 2. Power plan
    if let Some(plan_name) = active_power_plan_windows() {
        checks.push(power_plan_check(&plan_name));
    }

    // 3. CPU load (over 100 ms)
    if let Some(busy_pct) = get_system_times_usage(100) {
        checks.push(cpu_load_check_windows(busy_pct));
    }

    // 4. Processor frequency limit (power policy or thermal throttling)
    if let Some(limits) = processor_limits_windows() {
        checks.push(cpu_throttle_check_windows(&limits));
    }

    checks
}

/// (MaxMhz, MhzLimit) of every logical processor, via `CallNtPowerInformation`.
#[cfg(target_os = "windows")]
fn processor_limits_windows() -> Option<Vec<(u32, u32)>> {
    use windows_sys::Win32::System::Power::{
        CallNtPowerInformation, ProcessorInformation, PROCESSOR_POWER_INFORMATION,
    };

    // The call fills one entry per logical processor and leaves the rest of an
    // oversized buffer zeroed, so unused entries are dropped by `MaxMhz == 0`.
    // SAFETY: the structure is a plain C struct of integers.
    let mut info: Vec<PROCESSOR_POWER_INFORMATION> = vec![unsafe { std::mem::zeroed() }; 1024];
    let bytes = (info.len() * std::mem::size_of::<PROCESSOR_POWER_INFORMATION>()) as u32;
    // SAFETY: no input buffer is needed for this level; `info` is a writable
    // buffer of `bytes` bytes.
    let status = unsafe {
        CallNtPowerInformation(
            ProcessorInformation,
            std::ptr::null(),
            0,
            info.as_mut_ptr().cast(),
            bytes,
        )
    };
    if status != 0 {
        return None;
    }
    let limits: Vec<(u32, u32)> = info
        .iter()
        .filter(|p| p.MaxMhz > 0)
        .map(|p| (p.MaxMhz, p.MhzLimit))
        .collect();
    (!limits.is_empty()).then_some(limits)
}

/// Windows frequency limit check: warns if the OS caps any processor below its
/// maximum frequency (a power plan with a reduced maximum processor state, or
/// thermal/power throttling). `limits` holds (max MHz, current limit MHz) per CPU.
#[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
fn cpu_throttle_check_windows(limits: &[(u32, u32)]) -> Check {
    let limited: Vec<&(u32, u32)> = limits.iter().filter(|(max, limit)| limit < max).collect();
    match limited.first() {
        None => Check::new("CPU frequency limit", Status::Ok, "none"),
        Some((max, limit)) => Check::new(
            "CPU frequency limit",
            Status::Warn,
            format!(
                "{} of {} CPUs capped (e.g. {limit} of {max} MHz)",
                limited.len(),
                limits.len()
            ),
        )
        .with_hint("set the maximum processor state to 100% in the power plan, and check cooling"),
    }
}

#[cfg(target_os = "windows")]
fn filetime_to_u64(ft: windows_sys::Win32::Foundation::FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | (ft.dwLowDateTime as u64)
}

#[cfg(target_os = "windows")]
fn get_system_times_usage(sleep_ms: u64) -> Option<f64> {
    unsafe {
        let mut idle1: windows_sys::Win32::Foundation::FILETIME = std::mem::zeroed();
        let mut kernel1: windows_sys::Win32::Foundation::FILETIME = std::mem::zeroed();
        let mut user1: windows_sys::Win32::Foundation::FILETIME = std::mem::zeroed();
        // SAFETY: Pointers are valid, writable FILETIME structures.
        if windows_sys::Win32::System::Threading::GetSystemTimes(
            &mut idle1,
            &mut kernel1,
            &mut user1,
        ) == 0
        {
            return None;
        }

        std::thread::sleep(std::time::Duration::from_millis(sleep_ms));

        let mut idle2: windows_sys::Win32::Foundation::FILETIME = std::mem::zeroed();
        let mut kernel2: windows_sys::Win32::Foundation::FILETIME = std::mem::zeroed();
        let mut user2: windows_sys::Win32::Foundation::FILETIME = std::mem::zeroed();
        // SAFETY: Pointers are valid, writable FILETIME structures.
        if windows_sys::Win32::System::Threading::GetSystemTimes(
            &mut idle2,
            &mut kernel2,
            &mut user2,
        ) == 0
        {
            return None;
        }

        let idle = filetime_to_u64(idle2).saturating_sub(filetime_to_u64(idle1));
        let kernel = filetime_to_u64(kernel2).saturating_sub(filetime_to_u64(kernel1));
        let user = filetime_to_u64(user2).saturating_sub(filetime_to_u64(user1));

        let total = kernel + user;
        if total == 0 {
            return None;
        }
        let busy = total.saturating_sub(idle);
        Some((busy as f64 / total as f64) * 100.0)
    }
}

#[cfg(target_os = "windows")]
fn active_power_plan_windows() -> Option<String> {
    unsafe {
        let mut guid_ptr: *mut windows_sys::core::GUID = std::ptr::null_mut();
        // SAFETY: PowerGetActiveScheme allocates the active policy GUID and writes its address.
        let err = windows_sys::Win32::System::Power::PowerGetActiveScheme(
            std::ptr::null_mut(),
            &mut guid_ptr,
        );
        if err != 0 || guid_ptr.is_null() {
            return None;
        }

        let mut buf_size: u32 = 0;
        let _ = windows_sys::Win32::System::Power::PowerReadFriendlyName(
            std::ptr::null_mut(),
            guid_ptr,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
            &mut buf_size,
        );

        let name = if buf_size > 1 {
            let mut buf = vec![0u8; buf_size as usize];
            let err = windows_sys::Win32::System::Power::PowerReadFriendlyName(
                std::ptr::null_mut(),
                guid_ptr,
                std::ptr::null(),
                std::ptr::null(),
                buf.as_mut_ptr(),
                &mut buf_size,
            );
            if err == 0 {
                decode_utf16le_name(buf.get(..buf_size as usize).unwrap_or(&buf))
            } else {
                None
            }
        } else {
            None
        };

        let resolved_name = name.unwrap_or_else(|| match (*guid_ptr).data1 {
            0x381b4222 => "Balanced".to_string(),
            0x8c5e7fda => "High performance".to_string(),
            0xa1841308 => "Power saver".to_string(),
            0xe9a42b02 => "Ultimate Performance".to_string(),
            _ => format!("{:08x}", (*guid_ptr).data1),
        });

        // SAFETY: LocalFree frees the GUID allocated by PowerGetActiveScheme.
        windows_sys::Win32::Foundation::LocalFree(guid_ptr as _);

        Some(resolved_name)
    }
}

/// PowerReadFriendlyName returns UTF-16LE bytes in a byte buffer.
#[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
fn decode_utf16le_name(bytes: &[u8]) -> Option<String> {
    let units: Vec<u16> = bytes
        .chunks(2)
        .filter_map(|pair| match pair {
            [low, high] => Some(u16::from_le_bytes([*low, *high])),
            _ => None,
        })
        .collect();
    let name = String::from_utf16_lossy(&units)
        .trim_matches('\0')
        .trim()
        .to_string();
    (!name.is_empty()).then_some(name)
}

/// Windows power status check: AC line status (0 = offline/battery, 1 = online/AC, 255 = unknown)
/// and battery saver status flag.
#[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
fn power_check_windows(ac_line_status: u8, battery_saver: bool) -> Option<Check> {
    if ac_line_status == 255 {
        return None;
    }
    let on_battery = ac_line_status == 0;
    if on_battery {
        Some(
            Check::new("Power source", Status::Warn, "battery")
                .with_hint("connect the power adapter (on battery, CPUs are often clocked down)"),
        )
    } else if battery_saver {
        Some(
            Check::new("Power source", Status::Warn, "AC (battery saver on)")
                .with_hint("turn off Battery Saver in Settings → System → Power & battery"),
        )
    } else {
        Some(Check::new("Power source", Status::Ok, "AC"))
    }
}

/// Windows power plan check: warns unless configured for "High performance" or "Ultimate Performance".
#[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
fn power_plan_check(plan_name: &str) -> Check {
    let is_high_perf = plan_name.eq_ignore_ascii_case("High performance")
        || plan_name.eq_ignore_ascii_case("Ultimate Performance")
        || plan_name.to_ascii_lowercase().contains("performance");
    Check::new(
        "Power plan",
        if is_high_perf {
            Status::Ok
        } else {
            Status::Warn
        },
        plan_name,
    )
    .with_hint("set power plan to 'High performance' (powercfg /setactive 8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c)")
}

/// Windows CPU load check: warns if system CPU busy percentage is 10% or higher.
#[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
fn cpu_load_check_windows(cpu_percent: f64) -> Check {
    Check::new(
        "CPU load",
        if cpu_percent < 10.0 {
            Status::Ok
        } else {
            Status::Warn
        },
        format!("{cpu_percent:.1}% busy"),
    )
    .with_hint("stop other programs, or benchmark on an idle machine")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn governor() {
        assert!(governor_check(&[]).is_none());
        let ok = governor_check(&strings(&["performance", "performance"])).unwrap();
        assert_eq!(ok.status, Status::Ok);
        assert_eq!(ok.detail, "performance on 2 CPUs");
        assert!(ok.hint.is_none());

        let mixed = governor_check(&strings(&["powersave", "performance", "powersave"])).unwrap();
        assert_eq!(mixed.status, Status::Fail);
        assert_eq!(mixed.detail, "powersave on 2 CPUs, performance on 1 CPU");
        assert!(mixed.hint.unwrap().contains("cpupower"));
    }

    #[test]
    fn turbo() {
        assert!(turbo_check(None, None).is_none());
        assert_eq!(turbo_check(Some("0"), None).unwrap().status, Status::Fail);
        assert_eq!(turbo_check(Some("1"), None).unwrap().status, Status::Ok);
        let amd = turbo_check(None, Some("1")).unwrap();
        assert_eq!(amd.status, Status::Fail);
        assert!(amd.hint.unwrap().contains("cpufreq/boost"));
        assert_eq!(turbo_check(None, Some("0")).unwrap().status, Status::Ok);
    }

    #[test]
    fn load() {
        assert_eq!(parse_loadavg("0.21 0.30 0.25 1/345 6789"), Some(0.21));
        assert_eq!(parse_loadavg(""), None);
        let linux = |load, cpus| load_check(load, cpus, LINUX_LOAD_PER_CPU).status;
        assert_eq!(linux(0.21, 8), Status::Ok);
        assert_eq!(linux(0.9, 2), Status::Ok); // below 1.0
        assert_eq!(linux(3.5, 8), Status::Warn);
        assert_eq!(linux(3.5, 64), Status::Ok); // below 6.4
        assert_eq!(
            load_check(0.21, 8, LINUX_LOAD_PER_CPU).detail,
            "0.21 (8 CPUs)"
        );

        // An idle macOS desktop (#188): 3.99 on 10 CPUs is fine, 6 is not
        let macos = |load, cpus| load_check(load, cpus, MACOS_LOAD_PER_CPU).status;
        assert_eq!(macos(3.99, 10), Status::Ok);
        assert_eq!(macos(6.0, 10), Status::Warn);
    }

    #[test]
    fn power() {
        let supply = |kind: &str, online: &str| (kind.to_string(), online.to_string());
        assert!(power_check_linux(&[]).is_none());
        assert!(power_check_linux(&[supply("Battery", "")]).is_none());
        let ac = power_check_linux(&[supply("Mains", "1"), supply("Battery", "")]).unwrap();
        assert_eq!((ac.status, ac.detail.as_str()), (Status::Ok, "AC"));
        let battery = power_check_linux(&[supply("Mains", "0")]).unwrap();
        assert_eq!(battery.status, Status::Warn);
    }

    #[test]
    fn thermal_pressure() {
        use crate::thermal::PressureLevel;
        let ok = pressure_check(PressureLevel::Nominal);
        assert_eq!(ok.status, Status::Ok);
        assert_eq!(ok.detail, "nominal");
        assert_eq!(pressure_check(PressureLevel::Heavy).status, Status::Warn);
    }

    #[test]
    fn thermal() {
        assert!(thermal_check(&[]).is_none());
        assert_eq!(thermal_check(&[45.0, 48.2]).unwrap().detail, "max 48 °C");
        assert_eq!(thermal_check(&[45.0, 91.0]).unwrap().status, Status::Warn);
    }

    #[test]
    fn pmset_parsing() {
        let batt = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=1)\t80%; discharging";
        assert_eq!(parse_pmset_batt(batt), Some(true));
        assert_eq!(
            parse_pmset_batt("Now drawing from 'AC Power'\n"),
            Some(false)
        );
        assert_eq!(parse_pmset_batt(""), None);

        let settings = "System-wide power settings:\n lowpowermode         1\n sleep 1\n";
        assert_eq!(parse_pmset_low_power(settings), Some(true));
        assert_eq!(parse_pmset_low_power(" lowpowermode 0\n"), Some(false));
        assert_eq!(parse_pmset_low_power(" sleep 1\n"), None);

        let therm =
            "CPU_Scheduler_Limit \t= 100\nCPU_Available_CPUs \t= 8\nCPU_Speed_Limit \t= 80\n";
        assert_eq!(parse_pmset_speed_limit(therm), Some(80));
        assert_eq!(
            parse_pmset_speed_limit("Note: No thermal warning level has been recorded\n"),
            None
        );
    }

    #[test]
    fn fake_linux_root() {
        let dir = tempfile::tempdir().unwrap();
        let write = |path: &str, content: &str| {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        };
        write(
            "sys/devices/system/cpu/cpu0/cpufreq/scaling_governor",
            "powersave\n",
        );
        write(
            "sys/devices/system/cpu/cpu1/cpufreq/scaling_governor",
            "powersave\n",
        );
        write("sys/devices/system/cpu/intel_pstate/no_turbo", "1\n");
        write("proc/loadavg", "0.05 0.10 0.15 1/100 42\n");
        write("sys/class/power_supply/AC/type", "Mains\n");
        write("sys/class/power_supply/AC/online", "1\n");
        write("sys/class/thermal/thermal_zone0/temp", "47000\n");
        write("proc/sys/kernel/perf_event_paranoid", "3\n");

        let checks = linux_checks(dir.path(), 8);
        let summary: Vec<(&str, Status, &str)> = checks
            .iter()
            .map(|c| (c.name, c.status, c.detail.as_str()))
            .collect();
        assert_eq!(
            summary,
            [
                ("CPU governor", Status::Fail, "powersave on 2 CPUs"),
                ("Turbo boost", Status::Ok, "disabled"),
                ("Load average", Status::Ok, "0.05 (8 CPUs)"),
                ("Power source", Status::Ok, "AC"),
                ("Thermal", Status::Ok, "max 47 °C"),
                (
                    "HW counters",
                    Status::Info,
                    "permission denied (perf_event_paranoid=3)"
                ),
            ]
        );
        assert!(!all_passed(&checks)); // the governor fails
    }

    #[test]
    fn pmu_is_informational() {
        use crate::pmu::Probe;
        assert!(pmu_check(None, None).is_none());
        let denied = pmu_check(Some(3), None).unwrap();
        assert_eq!(denied.status, Status::Info);
        assert!(denied.hint.unwrap().contains("perf_event_paranoid=2"));
        assert!(all_passed(&[pmu_check(Some(3), None).unwrap()]));
        let ok = pmu_check(Some(1), Some(Probe::Opened)).unwrap();
        assert_eq!(ok.status, Status::Ok);
        assert_eq!(pmu_check(Some(2), None).unwrap().status, Status::Ok);
        let vm = pmu_check(Some(2), Some(Probe::Failed(2))).unwrap();
        assert_eq!(vm.status, Status::Info);
    }

    #[test]
    fn windows_power() {
        assert!(power_check_windows(255, false).is_none());
        assert!(power_check_windows(255, true).is_none());

        let batt = power_check_windows(0, false).unwrap();
        assert_eq!(batt.status, Status::Warn);
        assert_eq!(batt.detail, "battery");
        assert!(batt.hint.unwrap().contains("connect the power adapter"));

        let ac = power_check_windows(1, false).unwrap();
        assert_eq!(ac.status, Status::Ok);
        assert_eq!(ac.detail, "AC");
        assert!(ac.hint.is_none());

        let ac_saver = power_check_windows(1, true).unwrap();
        assert_eq!(ac_saver.status, Status::Warn);
        assert_eq!(ac_saver.detail, "AC (battery saver on)");
        assert!(ac_saver.hint.unwrap().contains("turn off Battery Saver"));
    }

    #[test]
    fn windows_power_plan() {
        let hp = power_plan_check("High performance");
        assert_eq!(hp.status, Status::Ok);
        assert_eq!(hp.detail, "High performance");

        let ult = power_plan_check("Ultimate Performance");
        assert_eq!(ult.status, Status::Ok);
        assert_eq!(ult.detail, "Ultimate Performance");

        let custom_perf = power_plan_check("My Custom Performance Plan");
        assert_eq!(custom_perf.status, Status::Ok);

        let balanced = power_plan_check("Balanced");
        assert_eq!(balanced.status, Status::Warn);
        assert_eq!(balanced.detail, "Balanced");
        assert!(balanced.hint.unwrap().contains("powercfg /setactive"));

        let saver = power_plan_check("Power saver");
        assert_eq!(saver.status, Status::Warn);
        assert_eq!(saver.detail, "Power saver");
    }

    #[test]
    fn windows_power_plan_name_decodes_utf16le_bytes() {
        assert_eq!(
            decode_utf16le_name(&[b'P', 0, b'l', 0, b'a', 0, b'n', 0, 0, 0]),
            Some("Plan".to_string())
        );
        assert_eq!(decode_utf16le_name(&[0, 0]), None);
    }

    #[test]
    fn windows_cpu_frequency_limit() {
        let ok = cpu_throttle_check_windows(&[(3000, 3000), (3000, 3000)]);
        assert_eq!(ok.status, Status::Ok);
        let capped = cpu_throttle_check_windows(&[(3000, 3000), (3000, 1200)]);
        assert_eq!(capped.status, Status::Warn);
        assert!(capped.detail.contains("1 of 2 CPUs"), "{}", capped.detail);
        assert!(
            capped.detail.contains("1200 of 3000 MHz"),
            "{}",
            capped.detail
        );
    }

    #[test]
    fn windows_cpu_load() {
        let low = cpu_load_check_windows(2.5);
        assert_eq!(low.status, Status::Ok);
        assert_eq!(low.detail, "2.5% busy");

        let border_low = cpu_load_check_windows(9.9);
        assert_eq!(border_low.status, Status::Ok);
        assert_eq!(border_low.detail, "9.9% busy");

        let border_high = cpu_load_check_windows(10.0);
        assert_eq!(border_high.status, Status::Warn);
        assert_eq!(border_high.detail, "10.0% busy");
        assert!(border_high.hint.unwrap().contains("idle machine"));

        let high = cpu_load_check_windows(75.3);
        assert_eq!(high.status, Status::Warn);
        assert_eq!(high.detail, "75.3% busy");
    }

    #[test]
    fn scheduler_check_reports_a_denied_kernel_scope() {
        use crate::perf_counters::{
            Availability, CounterConfig, CounterKind, CounterSpec, Scope, UnavailableReason,
        };
        let config = |requested: &[CounterKind], availability, partial_reason| CounterConfig {
            requested: requested
                .iter()
                .map(|&kind| CounterSpec {
                    kind,
                    scope: Scope::All,
                })
                .collect(),
            active: vec![],
            scope: Scope::All,
            requested_scope: Scope::All,
            availability,
            partial_reason,
            hidden: vec![],
            mock: false,
            simd: None,
        };
        let migrations = [CounterKind::CpuMigrations];
        let denied = config(
            &migrations,
            Availability::Unavailable(UnavailableReason::KernelScopeDenied),
            None,
        );
        let check = scheduler_check(Some(&denied)).unwrap();
        assert_eq!(check.status, Status::Info);
        assert_eq!(check.name, "Scheduler migrations");
        assert!(check.hint.unwrap().contains("perf_event_paranoid=1"));
        let partial = config(
            &[CounterKind::Cycles, CounterKind::CpuMigrations],
            Availability::Partial(vec![CounterSpec {
                kind: CounterKind::CpuMigrations,
                scope: Scope::All,
            }]),
            Some(UnavailableReason::KernelScopeDenied),
        );
        assert!(scheduler_check(Some(&partial)).is_some());
        // measured, not requested, or denied for another reason: nothing to say
        assert!(
            scheduler_check(Some(&config(&migrations, Availability::Measured, None))).is_none()
        );
        let other = config(
            &migrations,
            Availability::Unavailable(UnavailableReason::NoPmu),
            None,
        );
        assert!(scheduler_check(Some(&other)).is_none());
        let page_faults = config(
            &[CounterKind::PageFaults],
            Availability::Unavailable(UnavailableReason::KernelScopeDenied),
            None,
        );
        assert!(scheduler_check(Some(&page_faults)).is_none());
        assert!(scheduler_check(None).is_none());
    }
}
