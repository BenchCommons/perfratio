//! External power meter backend (`--power-meter`, `docs/specs/energy-attribution-and-power-meter.md`, #382).
//!
//! The meter is any command that prints one number on stdout. Two modes:
//!
//! - `power`: the command prints the instantaneous power (`12.5`, `12.5W`, `800mW`, `0.1 kW`).
//!   A poller thread runs it every `--power-meter-interval` and the energy of a run window is
//!   the integral of the power, holding each reading until the next one (zero-order hold).
//! - `counter`: the command prints a cumulative energy counter (`J`, `kJ`, `Wh`, `mWh`, `kWh`).
//!   The energy of a window is the counter read after the run minus the counter read before it.
//!
//! The samplers take the reading as a closure, so tests drive them without a process.

use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::EnergySampler;

/// Default `--power-meter-interval`
pub const DEFAULT_INTERVAL: Duration = Duration::from_millis(500);
/// A meter command that runs longer than this is killed and counts as a failed reading
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeterMode {
    /// The command prints instantaneous power
    Power,
    /// The command prints a cumulative energy counter
    Counter,
}

impl MeterMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "power" => Some(Self::Power),
            "counter" => Some(Self::Counter),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeterConfig {
    pub command: String,
    pub mode: MeterMode,
    pub interval: Duration,
}

static CONFIG: Mutex<Option<MeterConfig>> = Mutex::new(None);

/// Stores the meter settings of this process (called once by the option parser).
pub fn configure(config: MeterConfig) {
    *CONFIG.lock().unwrap_or_else(|e| e.into_inner()) = Some(config);
}

pub fn configured() -> Option<MeterConfig> {
    CONFIG.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Reads the meter once: Watts in `power` mode, Joules in `counter` mode.
pub type Reader = Arc<dyn Fn() -> Result<f64, String> + Send + Sync>;

/// Builds the reader that runs the user's command through the shell.
pub fn command_reader(config: &MeterConfig) -> Reader {
    let command = config.command.clone();
    let mode = config.mode;
    Arc::new(move || {
        let out = run_command(&command, COMMAND_TIMEOUT)?;
        parse_reading(&out, mode)
    })
}

/// Runs `command` through the shell and returns its stdout (at most 4 KiB).
/// A command that runs longer than `timeout` is killed with its whole process group (Unix), and
/// the reader reports over a channel that is awaited only until the same deadline, since a
/// grandchild may keep the pipe open.
fn run_command(command: &str, timeout: Duration) -> Result<String, String> {
    use std::process::{Command, Stdio};
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(command);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = Command::new("sh");
        c.arg("-c").arg(command);
        {
            use std::os::unix::process::CommandExt;
            c.process_group(0);
        }
        c
    };
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start the meter command: {e}"))?;
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = (&mut stdout).take(4096).read_to_string(&mut buf);
        let _ = tx.send(buf);
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(None) => {
                kill_group(&mut child);
                let _ = child.wait();
                return Err(timeout_error(timeout));
            }
            Err(e) => return Err(format!("cannot wait for the meter command: {e}")),
        }
    };
    // A descendant may keep the pipe open after `sh` exits: wait for the reader only until the
    // same deadline, then kill the process group.
    let out = match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(out) => out,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            kill_group(&mut child);
            return Err(timeout_error(timeout));
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => String::new(),
    };
    if status.success() {
        Ok(out)
    } else {
        Err(format!("the meter command failed ({status})"))
    }
}

fn timeout_error(timeout: Duration) -> String {
    format!(
        "the meter command did not finish within {} s",
        timeout.as_secs()
    )
}

/// Kills the meter's process group (Unix) and the child itself.
fn kill_group(child: &mut std::process::Child) {
    #[cfg(unix)]
    // SAFETY: killpg on the group created by process_group(0); no memory is touched.
    unsafe {
        libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
    }
    let _ = child.kill();
}

/// Parses a meter reading: the first number of the output, with an optional unit either
/// attached (`12.5W`) or as the next word (`12.5 W`). Returns Watts (`power`) or Joules
/// (`counter`). Negative, infinite and NaN values are errors.
pub fn parse_reading(text: &str, mode: MeterMode) -> Result<f64, String> {
    let mut words = text.split_whitespace();
    let first = words.next().ok_or("the meter command printed nothing")?;
    let split = first
        .find(|c: char| c.is_alphabetic() && c != 'e' && c != 'E')
        .unwrap_or(first.len());
    let (number, attached) = first.split_at(split);
    let value: f64 = number
        .parse()
        .map_err(|_| format!("'{first}' is not a number"))?;
    let unit = if attached.is_empty() {
        words.next().unwrap_or("")
    } else {
        attached
    };
    let factor = match (mode, unit) {
        (MeterMode::Power, "" | "W") => 1.0,
        (MeterMode::Power, "mW") => 1e-3,
        (MeterMode::Power, "kW") => 1e3,
        (MeterMode::Counter, "" | "J") => 1.0,
        (MeterMode::Counter, "kJ") => 1e3,
        (MeterMode::Counter, "Wh") => 3600.0,
        (MeterMode::Counter, "mWh") => 3.6,
        (MeterMode::Counter, "kWh") => 3.6e6,
        _ => {
            let expected = match mode {
                MeterMode::Power => "W, mW or kW",
                MeterMode::Counter => "J, kJ, mWh, Wh or kWh",
            };
            return Err(format!("unknown unit '{unit}' (expected {expected})"));
        }
    };
    let value = value * factor;
    if value.is_finite() && value >= 0.0 {
        Ok(value)
    } else {
        Err(format!("the reading '{first}' is negative or not finite"))
    }
}

/// Energy of the window `[t0, t1]` (seconds) from power samples `(time, watts)` sorted by
/// time. Each sample holds until the next one; before the first sample the first value
/// holds, after the last one the last value. `None` for no samples or an inverted window.
pub fn integrate(samples: &[(f64, f64)], t0: f64, t1: f64) -> Option<f64> {
    if samples.is_empty() || t1 < t0 {
        return None;
    }
    let mut joules = 0.0;
    for (i, &(time, watts)) in samples.iter().enumerate() {
        let from = if i == 0 { f64::NEG_INFINITY } else { time };
        let to = samples.get(i + 1).map_or(f64::INFINITY, |next| next.0);
        let overlap = to.min(t1) - from.max(t0);
        if overlap > 0.0 {
            joules += watts * overlap;
        }
    }
    Some(joules)
}

struct Shared {
    /// `(seconds since origin, watts)`; the time is the middle of the command execution
    samples: Mutex<Vec<(f64, f64)>>,
    stop: AtomicBool,
    failures: AtomicUsize,
}

/// `power` mode: a poller thread feeds `Shared`, windows are integrated on `stop()`.
pub struct PowerMeterSampler {
    shared: Arc<Shared>,
    origin: Instant,
    interval: Duration,
    window_start: Option<f64>,
    poller: Option<std::thread::JoinHandle<()>>,
}

impl PowerMeterSampler {
    /// Reads the meter once to check it works, then starts polling.
    pub fn try_new(read: Reader, interval: Duration) -> Result<Self, String> {
        let origin = Instant::now();
        let first = read()?;
        let shared = Arc::new(Shared {
            samples: Mutex::new(vec![(origin.elapsed().as_secs_f64(), first)]),
            stop: AtomicBool::new(false),
            failures: AtomicUsize::new(0),
        });
        let poller = {
            let shared = shared.clone();
            std::thread::spawn(move || {
                while !shared.stop.load(Ordering::Relaxed) {
                    let begin = origin.elapsed().as_secs_f64();
                    match read() {
                        Ok(watts) => {
                            let mid = (begin + origin.elapsed().as_secs_f64()) / 2.0;
                            shared
                                .samples
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .push((mid, watts));
                        }
                        Err(_) => {
                            shared.failures.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    let wake = Instant::now() + interval;
                    while Instant::now() < wake && !shared.stop.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(2).min(interval));
                    }
                }
            })
        };
        Ok(Self {
            shared,
            origin,
            interval,
            window_start: None,
            poller: Some(poller),
        })
    }

    /// Readings that failed since the start (the export still reports the runs that worked)
    pub fn failed_readings(&self) -> usize {
        self.shared.failures.load(Ordering::Relaxed)
    }
}

impl Drop for PowerMeterSampler {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(poller) = self.poller.take() {
            let _ = poller.join();
        }
    }
}

impl EnergySampler for PowerMeterSampler {
    fn start(&mut self) {
        self.window_start = Some(self.origin.elapsed().as_secs_f64());
    }

    fn stop(&mut self) -> Option<f64> {
        let t0 = self.window_start.take()?;
        let t1 = self.origin.elapsed().as_secs_f64();
        let mut samples = self
            .shared
            .samples
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let last = samples.last()?.0;
        // The meter stopped answering: do not hold an old value over the window
        if t1 - last > 3.0 * self.interval.as_secs_f64() + 1.0 {
            return None;
        }
        let joules = integrate(&samples, t0, t1);
        // keep the last sample before the window end (it still holds), drop older ones
        let keep_from = samples.partition_point(|s| s.0 <= t1).saturating_sub(1);
        samples.drain(..keep_from);
        joules
    }

    fn is_available(&self) -> bool {
        true
    }
}

/// `counter` mode: the window energy is the difference of two readings.
pub struct CounterMeterSampler {
    read: Reader,
    before: Option<f64>,
}

impl CounterMeterSampler {
    pub fn try_new(read: Reader) -> Result<Self, String> {
        read()?;
        Ok(Self { read, before: None })
    }
}

impl EnergySampler for CounterMeterSampler {
    fn start(&mut self) {
        self.before = (self.read)().ok();
    }

    fn stop(&mut self) -> Option<f64> {
        let before = self.before.take()?;
        let after = (self.read)().ok()?;
        // A counter that went down was reset or wrapped: no value rather than a wrong one
        (after >= before).then_some(after - before)
    }

    fn is_available(&self) -> bool {
        true
    }
}

/// The sampler for `config`, or the reason the meter cannot be used.
pub fn build(config: &MeterConfig, read: Reader) -> Result<Box<dyn EnergySampler>, String> {
    match config.mode {
        MeterMode::Power => {
            PowerMeterSampler::try_new(read, config.interval).map(|s| Box::new(s) as _)
        }
        MeterMode::Counter => CounterMeterSampler::try_new(read).map(|s| Box::new(s) as _),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    #[cfg(unix)]
    #[test]
    fn a_background_grandchild_holding_the_pipe_does_not_block_a_normal_exit() {
        let start = Instant::now();
        let r = run_command("sleep 30 & echo 5", Duration::from_millis(500));
        assert!(r.unwrap_err().contains("did not finish"));
        assert!(start.elapsed() < Duration::from_secs(10));
    }

    #[cfg(unix)]
    #[test]
    fn a_hung_command_is_killed_at_the_timeout_even_with_a_grandchild() {
        let start = Instant::now();
        let r = run_command("sleep 30; echo 5", Duration::from_millis(300));
        assert!(r.unwrap_err().contains("did not finish"));
        assert!(start.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn readings_with_and_without_units() {
        use MeterMode::*;
        assert_eq!(parse_reading("12.5\n", Power), Ok(12.5));
        assert_eq!(parse_reading("12.5W", Power), Ok(12.5));
        assert_eq!(parse_reading("  800 mW \n", Power), Ok(0.8));
        assert_eq!(parse_reading("0.1kW", Power), Ok(100.0));
        assert_eq!(parse_reading("1e2", Power), Ok(100.0));
        assert_eq!(parse_reading("2 Wh", Counter), Ok(7200.0));
        assert_eq!(parse_reading("1.5kWh", Counter), Ok(5.4e6));
        assert_eq!(parse_reading("10 kJ", Counter), Ok(10_000.0));
    }

    #[test]
    fn bad_readings_are_errors() {
        use MeterMode::*;
        for (text, mode) in [
            ("", Power),
            ("abc", Power),
            ("-3", Power),
            ("inf", Power),
            ("NaN", Power),
            ("5 Wh", Power),
            ("5 W", Counter),
            ("5 MW", Power),
        ] {
            assert!(parse_reading(text, mode).is_err(), "{text:?}");
        }
    }

    #[test]
    fn integration_holds_each_sample_until_the_next() {
        let samples = [(0.0, 10.0), (1.0, 20.0), (2.0, 0.0)];
        // 0.5 s at 10 W + 0.5 s at 20 W
        assert_eq!(integrate(&samples, 0.5, 1.5), Some(15.0));
        // inside one hold
        let inside = integrate(&samples, 1.2, 1.4).unwrap();
        assert!((inside - 4.0).abs() < 1e-9, "{inside}");
        // before the first sample the first value holds, after the last the last one
        assert_eq!(integrate(&samples, -1.0, 0.0), Some(10.0));
        assert_eq!(integrate(&samples, 2.0, 5.0), Some(0.0));
        // whole range
        assert_eq!(integrate(&samples, 0.0, 2.0), Some(30.0));
        assert_eq!(integrate(&samples, 1.0, 1.0), Some(0.0));
        assert_eq!(integrate(&samples, 2.0, 1.0), None);
        assert_eq!(integrate(&[], 0.0, 1.0), None);
    }

    fn reader_of(f: impl Fn() -> Result<f64, String> + Send + Sync + 'static) -> Reader {
        Arc::new(f)
    }

    #[test]
    fn the_power_sampler_integrates_a_constant_meter_over_the_window() {
        let mut sampler =
            PowerMeterSampler::try_new(reader_of(|| Ok(10.0)), Duration::from_millis(10)).unwrap();
        std::thread::sleep(Duration::from_millis(30));
        sampler.start();
        let begin = Instant::now();
        std::thread::sleep(Duration::from_millis(120));
        let joules = sampler.stop().unwrap();
        let seconds = begin.elapsed().as_secs_f64();
        assert!(
            (joules / seconds - 10.0).abs() < 0.5,
            "{joules} J in {seconds} s"
        );
        assert!(sampler.is_available());
    }

    #[test]
    fn the_power_sampler_follows_a_changing_meter() {
        let calls = Arc::new(AtomicU64::new(0));
        let c = calls.clone();
        // 10 W for the first 5 readings, then 30 W
        let read = reader_of(move || {
            Ok(if c.fetch_add(1, Ordering::Relaxed) < 5 {
                10.0
            } else {
                30.0
            })
        });
        let mut sampler = PowerMeterSampler::try_new(read, Duration::from_millis(10)).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        sampler.start();
        let begin = Instant::now();
        std::thread::sleep(Duration::from_millis(100));
        let joules = sampler.stop().unwrap();
        let watts = joules / begin.elapsed().as_secs_f64();
        assert!((watts - 30.0).abs() < 1.5, "{watts}");
    }

    #[test]
    fn a_meter_that_fails_at_once_is_rejected() {
        let err = PowerMeterSampler::try_new(
            reader_of(|| Err("no route to the meter".into())),
            DEFAULT_INTERVAL,
        )
        .err()
        .unwrap();
        assert!(err.contains("no route"));
    }

    #[test]
    fn a_window_without_start_has_no_energy() {
        let mut sampler =
            PowerMeterSampler::try_new(reader_of(|| Ok(1.0)), Duration::from_millis(10)).unwrap();
        assert_eq!(sampler.stop(), None);
    }

    #[test]
    fn the_counter_sampler_subtracts_the_readings() {
        let value = Arc::new(Mutex::new(100.0));
        let v = value.clone();
        let mut sampler =
            CounterMeterSampler::try_new(reader_of(move || Ok(*v.lock().unwrap()))).unwrap();
        sampler.start();
        *value.lock().unwrap() = 104.5;
        assert_eq!(sampler.stop(), Some(4.5));
        // a reset between the readings
        sampler.start();
        *value.lock().unwrap() = 1.0;
        assert_eq!(sampler.stop(), None);
        // a failed reading after the run
        let fail = Arc::new(AtomicBool::new(false));
        let f = fail.clone();
        let mut sampler = CounterMeterSampler::try_new(reader_of(move || {
            if f.load(Ordering::Relaxed) {
                Err("timeout".into())
            } else {
                Ok(1.0)
            }
        }))
        .unwrap();
        sampler.start();
        fail.store(true, Ordering::Relaxed);
        assert_eq!(sampler.stop(), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_command_reader_runs_a_shell_command() {
        let config = |command: &str, mode| MeterConfig {
            command: command.into(),
            mode,
            interval: DEFAULT_INTERVAL,
        };
        let read = command_reader(&config("echo 7.5 W", MeterMode::Power));
        assert_eq!(read(), Ok(7.5));
        let read = command_reader(&config("echo 1 Wh", MeterMode::Counter));
        assert_eq!(read(), Ok(3600.0));
        let read = command_reader(&config("exit 3", MeterMode::Power));
        assert!(read().unwrap_err().contains("failed"));
        let read = command_reader(&config("echo hello", MeterMode::Power));
        assert!(read().unwrap_err().contains("not a number"));
    }
}
