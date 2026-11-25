pub mod rapl;

pub trait EnergySampler: Send + Sync {
    /// Start an energy measurement window
    fn start(&mut self);

    /// Stop measurement and return energy consumed in Joules during window
    fn stop(&mut self) -> Option<f64>;

    /// Check if energy measurement is supported and available on this platform
    fn is_available(&self) -> bool;
}

/// Why energy measurement cannot be used on this host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnergyUnavailable {
    UnsupportedPlatform,
    RaplNotFound,
    RaplUnreadable,
}

impl EnergyUnavailable {
    /// Value of `perfratio.measurements.energy_unavailable_reason` in the JSON export.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedPlatform => "unsupported_platform",
            Self::RaplNotFound => "rapl_not_found",
            Self::RaplUnreadable => "rapl_unreadable",
        }
    }
}

/// Check whether `get_energy_sampler` would return a working sampler.
pub fn probe() -> Result<(), EnergyUnavailable> {
    #[cfg(target_os = "linux")]
    {
        rapl::probe_at(std::path::Path::new("/sys/class/powercap/intel-rapl"))
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(EnergyUnavailable::UnsupportedPlatform)
    }
}

pub fn get_energy_sampler() -> Box<dyn EnergySampler> {
    #[cfg(target_os = "linux")]
    {
        if let Some(sampler) = rapl::LinuxRaplSampler::try_new() {
            return Box::new(sampler);
        }
    }
    Box::new(DummySampler::new())
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
pub fn idle_power(dur: std::time::Duration) -> Option<f64> {
    static IDLE: std::sync::OnceLock<Option<f64>> = std::sync::OnceLock::new();
    *IDLE.get_or_init(|| {
        let mut sampler = get_energy_sampler();
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
