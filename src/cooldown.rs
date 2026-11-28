//! `--cooldown <DURATION|until-temp=<C>>`: wait between runs so long
//! benchmarks do not start hot. See `docs/specs/cgroup-thermal.md`.
//!
//! The wait happens outside the timed window, between two runs and between two
//! commands. It is not a statistic: it only changes the temperature the next run
//! starts at.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How long `until-temp` waits before it gives up (`--cooldown-timeout`)
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);
/// How often `until-temp` reads the sensors
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// What to wait for
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cooldown {
    /// A fixed time
    Duration(Duration),
    /// Until the hottest sensor is at most this many °C
    UntilTemp(f64),
}

impl Cooldown {
    /// `500ms`, `2s`, `until-temp=60`
    pub fn parse(value: &str) -> Result<Self, String> {
        if let Some(celsius) = value.strip_prefix("until-temp=") {
            return match celsius.trim().parse::<f64>() {
                Ok(c) if c.is_finite() && c > 0.0 && c < 150.0 => Ok(Cooldown::UntilTemp(c)),
                _ => Err(format!(
                    "invalid temperature '{celsius}': expected degrees Celsius, e.g. 'until-temp=60'"
                )),
            };
        }
        crate::util::units::parse_duration(value)
            .map(Cooldown::Duration)
            .map_err(|e| {
                format!("invalid cooldown '{value}': {e} (use a duration such as '2s', or 'until-temp=60')")
            })
    }
}

/// The outcome of one wait
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Outcome {
    /// Nothing to wait for
    Ready,
    Waited(Duration),
    /// `until-temp` ran out of time at this temperature
    GaveUp {
        celsius: f64,
        waited: Duration,
    },
    /// `until-temp` without a readable sensor
    NoSensor,
    Interrupted,
}

fn sleep(total: Duration, step: Duration) -> bool {
    let end = Instant::now() + total;
    loop {
        if crate::util::interrupt::interrupted() {
            return false;
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        std::thread::sleep(left.min(step));
    }
}

/// Wait as `cooldown` says. `root` is where the sensors are read.
pub fn wait(cooldown: Cooldown, root: &Path, timeout: Duration) -> Outcome {
    match cooldown {
        Cooldown::Duration(d) if d.is_zero() => Outcome::Ready,
        Cooldown::Duration(d) => {
            if sleep(d, Duration::from_millis(50)) {
                Outcome::Waited(d)
            } else {
                Outcome::Interrupted
            }
        }
        Cooldown::UntilTemp(target) => {
            let started = Instant::now();
            loop {
                let Some(now) = crate::thermal::max_temperature(root) else {
                    return Outcome::NoSensor;
                };
                if now <= target {
                    let waited = started.elapsed();
                    return if waited < Duration::from_millis(1) {
                        Outcome::Ready
                    } else {
                        Outcome::Waited(waited)
                    };
                }
                if started.elapsed() >= timeout {
                    return Outcome::GaveUp {
                        celsius: now,
                        waited: started.elapsed(),
                    };
                }
                let left = timeout.saturating_sub(started.elapsed());
                if !sleep(left.min(POLL_INTERVAL), Duration::from_millis(50)) {
                    return Outcome::Interrupted;
                }
            }
        }
    }
}

/// Why `until-temp` finds no sensor, per platform (#412)
const NO_SENSOR_REASON: &str = if cfg!(target_os = "macos") {
    " (macOS exposes no CPU temperature without root; '--thermal' records the \
     coarse thermal pressure level instead)"
} else if cfg!(windows) {
    " (the Windows 'Thermal Zone Information' performance counter has no instance on this \
     host, common on desktops and VMs)"
} else {
    ""
};

/// The first wait of a session is skipped: the machine has just been idle
static STARTED: AtomicBool = AtomicBool::new(false);
static WARNED_NO_SENSOR: AtomicBool = AtomicBool::new(false);

/// Called before every run and before every command. Does not wait before
/// the first call of the process. Prints one message when `until-temp` gives
/// up or has no sensor.
pub fn before_run(cooldown: Cooldown, timeout: Duration) {
    // A fixed wait is skipped before the very first run (the machine is idle);
    // `until-temp` also applies there: a hot start is what it is for
    let first = !STARTED.swap(true, Ordering::SeqCst);
    if first && matches!(cooldown, Cooldown::Duration(_)) {
        return;
    }
    match wait(cooldown, &crate::thermal::default_root(), timeout) {
        Outcome::GaveUp { celsius, waited } => eprintln!(
            "{} '--cooldown': the CPU is still at {celsius:.0} °C after waiting {:.0} s \
             ('--cooldown-timeout'); continuing.",
            crate::output::colors::yellow("Warning:"),
            waited.as_secs_f64()
        ),
        Outcome::NoSensor => {
            if !WARNED_NO_SENSOR.swap(true, Ordering::SeqCst) {
                eprintln!(
                    "{} '--cooldown until-temp': no temperature sensor can be read on this \
                     host{}; not waiting.",
                    crate::output::colors::yellow("Warning:"),
                    NO_SENSOR_REASON
                );
            }
        }
        Outcome::Ready | Outcome::Waited(_) | Outcome::Interrupted => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parses_durations_and_temperatures() {
        assert_eq!(
            Cooldown::parse("500ms").unwrap(),
            Cooldown::Duration(Duration::from_millis(500))
        );
        assert_eq!(
            Cooldown::parse("until-temp=60").unwrap(),
            Cooldown::UntilTemp(60.0)
        );
        assert_eq!(
            Cooldown::parse("until-temp=45.5").unwrap(),
            Cooldown::UntilTemp(45.5)
        );
        assert!(Cooldown::parse("until-temp=").is_err());
        assert!(Cooldown::parse("until-temp=hot").is_err());
        assert!(Cooldown::parse("until-temp=0").is_err());
        assert!(Cooldown::parse("until-temp=500").is_err());
        assert!(Cooldown::parse("soon").is_err());
        assert!(Cooldown::parse("").is_err());
    }

    fn zone(root: &Path, millidegrees: &str) {
        let path = root.join("sys/class/thermal/thermal_zone0/temp");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, millidegrees).unwrap();
    }

    #[test]
    fn a_duration_waits_that_long() {
        let started = Instant::now();
        let outcome = wait(
            Cooldown::Duration(Duration::from_millis(60)),
            Path::new("/nonexistent"),
            DEFAULT_TIMEOUT,
        );
        assert_eq!(outcome, Outcome::Waited(Duration::from_millis(60)));
        assert!(started.elapsed() >= Duration::from_millis(60));
        assert_eq!(
            wait(
                Cooldown::Duration(Duration::ZERO),
                Path::new("/nonexistent"),
                DEFAULT_TIMEOUT
            ),
            Outcome::Ready
        );
    }

    #[test]
    fn until_temp_is_ready_when_cool() {
        let dir = tempfile::tempdir().unwrap();
        zone(dir.path(), "55000\n");
        assert_eq!(
            wait(Cooldown::UntilTemp(60.0), dir.path(), DEFAULT_TIMEOUT),
            Outcome::Ready
        );
    }

    #[test]
    fn until_temp_gives_up_after_the_timeout() {
        let dir = tempfile::tempdir().unwrap();
        zone(dir.path(), "90000\n");
        let started = Instant::now();
        let outcome = wait(
            Cooldown::UntilTemp(60.0),
            dir.path(),
            Duration::from_millis(200),
        );
        match outcome {
            Outcome::GaveUp { celsius, waited } => {
                assert_eq!(celsius, 90.0);
                assert!(waited >= Duration::from_millis(200));
            }
            other => panic!("{other:?}"),
        }
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn until_temp_returns_once_the_sensor_cools() {
        let dir = tempfile::tempdir().unwrap();
        zone(dir.path(), "90000\n");
        let root = dir.path().to_path_buf();
        let cooler = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            zone(&root, "50000\n");
        });
        let outcome = wait(
            Cooldown::UntilTemp(60.0),
            dir.path(),
            Duration::from_secs(10),
        );
        cooler.join().unwrap();
        assert!(matches!(outcome, Outcome::Waited(_)), "{outcome:?}");
    }

    #[test]
    fn until_temp_without_a_sensor_does_not_wait() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            wait(Cooldown::UntilTemp(60.0), dir.path(), DEFAULT_TIMEOUT),
            Outcome::NoSensor
        );
    }
}
