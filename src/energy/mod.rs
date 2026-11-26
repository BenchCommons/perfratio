pub mod ioreport;
pub mod perf_rapl;
pub mod rapl;
pub mod windows_emi;

pub trait EnergySampler: Send + Sync {
    /// Start an energy measurement window
    fn start(&mut self);

    /// Stop measurement and return energy consumed in Joules during window
    fn stop(&mut self) -> Option<f64>;

    /// Check if energy measurement is supported and available on this platform
    fn is_available(&self) -> bool;
}

/// Environment variable that forces an energy backend (same values as `--energy-backend`).
pub const ENERGY_BACKEND_ENV: &str = "PERFRATIO_ENERGY_BACKEND";

/// Which energy backend the user asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackendPreference {
    /// The native source of the OS: sysfs powercap then perf PMU (Linux), IOReport (macOS),
    /// EMI (Windows)
    #[default]
    Auto,
    Sysfs,
    Perf,
    /// macOS Apple Silicon IOReport "Energy Model"
    Ioreport,
    /// Windows Energy Meter Interface
    Emi,
}

impl BackendPreference {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "auto" => Some(Self::Auto),
            "sysfs" => Some(Self::Sysfs),
            "perf" => Some(Self::Perf),
            "ioreport" => Some(Self::Ioreport),
            "emi" => Some(Self::Emi),
            _ => None,
        }
    }

    /// The CLI value wins over the environment variable.
    pub fn resolve(cli: Option<&str>, env: Option<&str>) -> Result<Self, String> {
        match cli.or(env) {
            None => Ok(Self::Auto),
            Some(v) => Self::parse(v).ok_or_else(|| v.to_string()),
        }
    }
}

/// The backend that actually produced (or would produce) the energy samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnergyBackend {
    SysfsPowercap,
    PerfPmu,
    /// macOS IOReport "Energy Model" channels
    IoReport,
    /// Windows Energy Meter Interface
    WindowsEmi,
}

impl EnergyBackend {
    /// Value of `perfratio.energy_backend` in the JSON export.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SysfsPowercap => "sysfs_powercap",
            Self::PerfPmu => "perf_pmu",
            Self::IoReport => "ioreport",
            Self::WindowsEmi => "windows_emi",
        }
    }
}

/// Why no energy backend is usable; maps to `measurements.energy_unavailable_reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unavailable {
    UnsupportedPlatform,
    /// Neither the sysfs files nor the PMU exist
    RaplNotFound,
    /// Files or the PMU exist but cannot be read or opened (EACCES, paranoid)
    RaplUnreadable,
    /// Intel Mac: no no-root energy source
    UnsupportedCpu,
    /// macOS: IOReport or its "Energy Model" channels are missing (VM, old OS)
    IoreportNotFound,
    /// macOS: IOReport answers but the CPU energy counters stay at 0 (macOS 27 without
    /// Apple's entitlement), so energy would be silently wrong
    IoreportFrozen,
    /// Windows: no EMI energy meter device on this machine
    EmiNotFound,
    /// Windows: EMI devices exist but cannot be opened or read (needs admin)
    EmiUnreadable,
}

impl Unavailable {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedPlatform => "unsupported_platform",
            Self::RaplNotFound => "rapl_not_found",
            Self::RaplUnreadable => "rapl_unreadable",
            Self::UnsupportedCpu => "unsupported_cpu",
            Self::IoreportNotFound => "ioreport_not_found",
            Self::IoreportFrozen => "ioreport_frozen",
            Self::EmiNotFound => "emi_not_found",
            Self::EmiUnreadable => "emi_unreadable",
        }
    }
}

type Selected = (Box<dyn EnergySampler>, EnergyBackend);
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
type PerfOpener<'a> = &'a dyn Fn() -> Result<Box<dyn EnergySampler>, perf_rapl::PerfRaplError>;

/// Backend selection with injectable sysfs root and perf opener.
/// Applies the fallback and error classification rules of the perf-rapl spec.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn select_with(
    pref: BackendPreference,
    sysfs_root: &std::path::Path,
    open_perf: PerfOpener<'_>,
) -> Result<Selected, Unavailable> {
    let mut unreadable = false;
    if pref != BackendPreference::Perf {
        match rapl::LinuxRaplSampler::try_new_at(sysfs_root) {
            Some(s) => return Ok((Box::new(s), EnergyBackend::SysfsPowercap)),
            None => unreadable |= rapl::domains_present(sysfs_root),
        }
    }
    if pref != BackendPreference::Sysfs {
        match open_perf() {
            Ok(s) => return Ok((s, EnergyBackend::PerfPmu)),
            Err(e) => unreadable |= e.is_unreadable(),
        }
    }
    Err(if unreadable {
        Unavailable::RaplUnreadable
    } else {
        Unavailable::RaplNotFound
    })
}

/// Picks a working sampler for `pref`, or the reason no backend is usable.
/// A backend that belongs to another OS is `unsupported_platform`.
pub fn select_sampler_detailed(pref: BackendPreference) -> Result<Selected, Unavailable> {
    #[cfg(target_os = "linux")]
    {
        if matches!(pref, BackendPreference::Ioreport | BackendPreference::Emi) {
            return Err(Unavailable::UnsupportedPlatform);
        }
        select_with(pref, std::path::Path::new(rapl::SYSFS_ROOT), &|| {
            perf_rapl::PerfRaplSampler::try_new().map(|s| Box::new(s) as Box<dyn EnergySampler>)
        })
    }
    #[cfg(target_os = "macos")]
    {
        if !matches!(pref, BackendPreference::Auto | BackendPreference::Ioreport) {
            return Err(Unavailable::UnsupportedPlatform);
        }
        if cfg!(target_arch = "x86_64") {
            return Err(Unavailable::UnsupportedCpu);
        }
        ioreport::IoReportSampler::try_new()
            .map(|s| {
                (
                    Box::new(s) as Box<dyn EnergySampler>,
                    EnergyBackend::IoReport,
                )
            })
            .map_err(|e| match e {
                ioreport::IoReportError::Frozen => Unavailable::IoreportFrozen,
                _ => Unavailable::IoreportNotFound,
            })
    }
    #[cfg(windows)]
    {
        if !matches!(pref, BackendPreference::Auto | BackendPreference::Emi) {
            return Err(Unavailable::UnsupportedPlatform);
        }
        windows_emi::EmiSampler::try_new()
            .map(|s| {
                (
                    Box::new(s) as Box<dyn EnergySampler>,
                    EnergyBackend::WindowsEmi,
                )
            })
            .map_err(|e| match e {
                windows_emi::EmiError::NotFound => Unavailable::EmiNotFound,
                windows_emi::EmiError::Unreadable => Unavailable::EmiUnreadable,
            })
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = pref;
        Err(Unavailable::UnsupportedPlatform)
    }
}

/// Picks a working sampler for `pref`, or None if no backend is usable.
pub fn select_sampler(pref: BackendPreference) -> Option<Selected> {
    select_sampler_detailed(pref).ok()
}

pub fn get_energy_sampler(pref: BackendPreference) -> Box<dyn EnergySampler> {
    match select_sampler(pref) {
        Some((sampler, _)) => sampler,
        None => Box::new(DummySampler::new()),
    }
}

pub struct DummySampler {
    start_time: Option<std::time::Instant>,
}

impl DummySampler {
    pub fn new() -> Self {
        Self { start_time: None }
    }
}

impl Default for DummySampler {
    fn default() -> Self {
        Self::new()
    }
}

impl EnergySampler for DummySampler {
    fn start(&mut self) {
        self.start_time = Some(std::time::Instant::now());
    }

    fn stop(&mut self) -> Option<f64> {
        // Dummy/fallback sampler does not provide hardware energy measurements
        None
    }

    fn is_available(&self) -> bool {
        false
    }
}

pub struct MockEnergySampler {
    active: bool,
}

impl MockEnergySampler {
    pub fn new() -> Self {
        Self { active: false }
    }
}

impl Default for MockEnergySampler {
    fn default() -> Self {
        Self::new()
    }
}

impl EnergySampler for MockEnergySampler {
    fn start(&mut self) {
        self.active = true;
    }

    fn stop(&mut self) -> Option<f64> {
        if self.active {
            self.active = false;
            Some(1.25)
        } else {
            None
        }
    }

    fn is_available(&self) -> bool {
        true
    }
}

/// Idle power in Watts from a window of `dur` during which nothing is benchmarked.
/// `sleep` is injected so tests do not wait.
pub fn measure_idle_power_with(
    sampler: &mut dyn EnergySampler,
    dur: std::time::Duration,
    sleep: impl FnOnce(std::time::Duration),
) -> Option<f64> {
    sampler.start();
    let t = std::time::Instant::now();
    sleep(dur);
    let joules = sampler.stop()?;
    idle_watts(joules, t.elapsed().as_secs_f64())
}

fn idle_watts(joules: f64, secs: f64) -> Option<f64> {
    (secs > 0.0 && joules >= 0.0).then(|| joules / secs)
}

/// Idle power, measured once per process on first use.
pub fn idle_power(dur: std::time::Duration, pref: BackendPreference) -> Option<f64> {
    static IDLE: std::sync::OnceLock<Option<f64>> = std::sync::OnceLock::new();
    *IDLE.get_or_init(|| {
        let mut sampler = get_energy_sampler(pref);
        if !sampler.is_available() {
            return None;
        }
        measure_idle_power_with(sampler.as_mut(), dur, std::thread::sleep)
    })
}

/// Net energy of one run: gross minus idle power over the run time, never below 0.
pub fn net_energy(gross: f64, idle_watts: f64, seconds: f64) -> f64 {
    (gross - idle_watts * seconds).max(0.0)
}

/// Runs shorter than this are dominated by RAPL quantization noise.
pub const SHORT_RUN_SECONDS: f64 = 0.010;

#[cfg(test)]
mod tests {
    #![cfg_attr(windows, allow(dead_code, unused_imports))]
    use super::*;

    #[test]
    fn preference_resolution() {
        assert_eq!(
            BackendPreference::resolve(None, None).unwrap(),
            BackendPreference::Auto
        );
        assert_eq!(
            BackendPreference::resolve(None, Some("perf")).unwrap(),
            BackendPreference::Perf
        );
        // CLI beats env
        assert_eq!(
            BackendPreference::resolve(Some("sysfs"), Some("perf")).unwrap(),
            BackendPreference::Sysfs
        );
        assert!(BackendPreference::resolve(None, Some("bogus")).is_err());
        assert_eq!(
            BackendPreference::parse("ioreport"),
            Some(BackendPreference::Ioreport)
        );
        assert_eq!(
            BackendPreference::parse("emi"),
            Some(BackendPreference::Emi)
        );
    }

    #[test]
    fn backend_and_reason_names() {
        assert_eq!(EnergyBackend::IoReport.as_str(), "ioreport");
        assert_eq!(EnergyBackend::WindowsEmi.as_str(), "windows_emi");
        assert_eq!(Unavailable::UnsupportedCpu.as_str(), "unsupported_cpu");
        assert_eq!(Unavailable::IoreportNotFound.as_str(), "ioreport_not_found");
        assert_eq!(Unavailable::EmiNotFound.as_str(), "emi_not_found");
        assert_eq!(Unavailable::EmiUnreadable.as_str(), "emi_unreadable");
    }

    struct Stub;
    impl EnergySampler for Stub {
        fn start(&mut self) {}
        fn stop(&mut self) -> Option<f64> {
            Some(1.0)
        }
        fn is_available(&self) -> bool {
            true
        }
    }

    fn fake_sysfs(root: &std::path::Path, content: Option<&str>) {
        let d = root.join("intel-rapl:0");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("name"), "package-0\n").unwrap();
        if let Some(c) = content {
            std::fs::write(d.join("energy_uj"), c).unwrap();
        }
    }

    type PerfResult = Result<Box<dyn EnergySampler>, perf_rapl::PerfRaplError>;

    fn perf_ok() -> PerfResult {
        Ok(Box::new(Stub))
    }

    fn pick(
        pref: BackendPreference,
        root: &std::path::Path,
        perf: fn() -> PerfResult,
    ) -> Result<EnergyBackend, Unavailable> {
        select_with(pref, root, &perf).map(|(_, b)| b)
    }

    #[test]
    #[cfg(not(windows))] // sysfs fixture dirs contain ":"
    fn auto_prefers_sysfs() {
        let dir = tempfile::tempdir().unwrap();
        fake_sysfs(dir.path(), Some("123\n"));
        assert_eq!(
            pick(BackendPreference::Auto, dir.path(), perf_ok),
            Ok(EnergyBackend::SysfsPowercap)
        );
    }

    #[test]
    #[cfg(not(windows))] // sysfs fixture dirs contain ":"
    fn auto_falls_back_to_perf_when_sysfs_missing_or_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        // no sysfs at all
        let missing = dir.path().join("nope");
        assert_eq!(
            pick(BackendPreference::Auto, &missing, perf_ok),
            Ok(EnergyBackend::PerfPmu)
        );
        // energy_uj present but not parseable/readable
        fake_sysfs(dir.path(), Some("garbage"));
        assert_eq!(
            pick(BackendPreference::Auto, dir.path(), perf_ok),
            Ok(EnergyBackend::PerfPmu)
        );
    }

    #[test]
    #[cfg(not(windows))] // sysfs fixture dirs contain ":"
    fn forced_backends_do_not_fall_back() {
        let dir = tempfile::tempdir().unwrap();
        fake_sysfs(dir.path(), Some("123\n"));
        assert_eq!(
            pick(BackendPreference::Perf, dir.path(), perf_ok),
            Ok(EnergyBackend::PerfPmu)
        );
        let missing = dir.path().join("nope");
        assert_eq!(
            pick(BackendPreference::Sysfs, &missing, perf_ok),
            Err(Unavailable::RaplNotFound)
        );
        fn denied() -> PerfResult {
            Err(perf_rapl::PerfRaplError::Os(13))
        }
        // forcing perf under a strict paranoid is unreadable, no sysfs fallback
        assert_eq!(
            pick(BackendPreference::Perf, dir.path(), denied),
            Err(Unavailable::RaplUnreadable)
        );
    }

    #[test]
    #[cfg(not(windows))] // sysfs fixture dirs contain ":"
    fn classifies_unavailable_reasons() {
        fn denied() -> PerfResult {
            Err(perf_rapl::PerfRaplError::Os(13))
        }
        fn no_pmu() -> PerfResult {
            Err(perf_rapl::PerfRaplError::NotFound)
        }
        fn enodev() -> PerfResult {
            Err(perf_rapl::PerfRaplError::Os(19))
        }
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        let auto = BackendPreference::Auto;
        assert_eq!(pick(auto, &missing, no_pmu), Err(Unavailable::RaplNotFound));
        assert_eq!(pick(auto, &missing, enodev), Err(Unavailable::RaplNotFound));
        assert_eq!(
            pick(auto, &missing, denied),
            Err(Unavailable::RaplUnreadable)
        );
        // sysfs file exists but is unreadable content, PMU absent
        fake_sysfs(dir.path(), Some("garbage"));
        assert_eq!(
            pick(auto, dir.path(), no_pmu),
            Err(Unavailable::RaplUnreadable)
        );
        // domain directory without energy_uj counts as not found
        let dir2 = tempfile::tempdir().unwrap();
        fake_sysfs(dir2.path(), None);
        assert_eq!(
            pick(auto, dir2.path(), no_pmu),
            Err(Unavailable::RaplNotFound)
        );
    }
}

#[cfg(test)]
mod baseline_tests {
    use super::*;
    use std::time::Duration;

    struct Mock(Option<f64>);
    impl EnergySampler for Mock {
        fn start(&mut self) {}
        fn stop(&mut self) -> Option<f64> {
            self.0
        }
        fn is_available(&self) -> bool {
            true
        }
    }

    #[test]
    fn idle_power_divides_by_elapsed() {
        assert_eq!(idle_watts(6.0, 2.0), Some(3.0));
        assert_eq!(idle_watts(1.0, 0.0), None);
        assert_eq!(idle_watts(-1.0, 1.0), None);
    }

    #[test]
    fn idle_power_none_without_reading() {
        let mut m = Mock(None);
        assert_eq!(
            measure_idle_power_with(&mut m, Duration::from_millis(1), |_| {}),
            None
        );
    }

    #[test]
    fn idle_power_with_mock_sampler_is_positive() {
        let mut m = Mock(Some(1.0));
        let w =
            measure_idle_power_with(&mut m, Duration::from_millis(5), std::thread::sleep).unwrap();
        assert!(w > 0.0 && w.is_finite());
    }

    #[test]
    fn net_energy_clamps_at_zero() {
        assert!((net_energy(1.0, 3.0, 0.1) - 0.7).abs() < 1e-12);
        assert_eq!(net_energy(0.1, 3.0, 1.0), 0.0);
    }
}
