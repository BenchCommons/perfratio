pub mod perf_rapl;
pub mod rapl;

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
    /// sysfs powercap first, perf PMU if sysfs is unreadable
    #[default]
    Auto,
    Sysfs,
    Perf,
}

impl BackendPreference {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "auto" => Some(Self::Auto),
            "sysfs" => Some(Self::Sysfs),
            "perf" => Some(Self::Perf),
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
}

impl EnergyBackend {
    /// Value of `perfratio.energy_backend` in the JSON export.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SysfsPowercap => "sysfs_powercap",
            Self::PerfPmu => "perf_pmu",
        }
    }
}

/// Picks a working sampler for `pref`, or None if no backend is usable.
pub fn select_sampler(pref: BackendPreference) -> Option<(Box<dyn EnergySampler>, EnergyBackend)> {
    #[cfg(target_os = "linux")]
    {
        if pref != BackendPreference::Perf {
            if let Some(s) = rapl::LinuxRaplSampler::try_new() {
                return Some((Box::new(s), EnergyBackend::SysfsPowercap));
            }
        }
        if pref != BackendPreference::Sysfs {
            if let Some(s) = perf_rapl::PerfRaplSampler::try_new() {
                return Some((Box::new(s), EnergyBackend::PerfPmu));
            }
        }
    }
    let _ = pref;
    None
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

#[cfg(test)]
mod tests {
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
    }
}
