//! Persistent process mode (`--persistent`), see `docs/specs/persistent-mode.md`.
//!
//! perfratio starts the command once and drives every iteration through a
//! versioned line protocol on the child's stdin and stdout. Time stamps and the
//! `hello` line are the wire format of the region protocol (`region.rs`): an
//! iteration is the region `iteration`, so [`crate::region::parse`] reads it.
//!
//! ```text
//! perfratio -> program (stdin)       run <n>   |   quit
//! program -> perfratio (stdout)      @perfratio hello 1 [<sdk-id>]      once, when ready
//!                                    @perfratio start iteration <t_ns>
//!                                    @perfratio stop iteration <t_ns>
//!                                    @perfratio done <n>   |   @perfratio fail <n> <message>
//! ```
//!
//! Stdout lines without the `@perfratio ` prefix are the program's own output
//! and are discarded.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::benchmark::executor::{BenchmarkIteration, Executor};
use crate::benchmark::timing_result::TimingResult;
use crate::command::Command;
use crate::options::{CmdFailureAction, CommandOutputPolicy, Options};
use crate::util::units::Second;

/// The protocol version this perfratio speaks (the same number as the region protocol).
pub const PROTOCOL_VERSION: u32 = crate::region::PROTOCOL_VERSION;
/// Environment variable that tells the program it runs in persistent mode.
pub const ENV_PROTOCOL: &str = "PERFRATIO_PERSISTENT_PROTOCOL";
/// Prefix of every protocol line the program writes to stdout.
pub const LINE_PREFIX: &str = "@perfratio ";
/// Region whose interval is the time of one iteration.
pub const ITERATION_REGION: &str = "iteration";
/// How long the program may take to say `hello`.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);
/// How long the program may take to exit after `quit`.
const QUIT_GRACE: Duration = Duration::from_secs(2);
/// Fewer iterations cannot tell warm-up from noise.
pub const MIN_ITERATIONS: usize = 10;
/// Width of the rolling median.
const WINDOW: usize = 5;
/// Relative band around the steady-state median that counts as steady.
const RELATIVE_BAND: f64 = 0.10;

/// Outcome of the warm-up analysis of one benchmark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SteadyStatus {
    /// The series reaches a steady state in its first half.
    SteadyState,
    /// The second half is still drifting or the series settles too late.
    NoSteadyState,
    /// Fewer than [`MIN_ITERATIONS`] iterations.
    TooFewIterations,
}

/// Statistics of the iterations after the warm-up phase, in seconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SteadyState {
    /// Index of the first steady iteration (0-based, among the measured iterations).
    pub first_iteration: usize,
    pub count: usize,
    pub mean: Second,
    pub stddev: Option<Second>,
    pub min: Second,
    pub max: Second,
}

/// Result of [`analyze`].
#[derive(Debug, Clone, PartialEq)]
pub struct Analysis {
    pub status: SteadyStatus,
    /// Iterations before the steady state; `None` unless the status is `steady_state`.
    pub warmup_iterations: Option<usize>,
    pub steady_state: Option<SteadyState>,
}

fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

/// Detect the warm-up phase of a series of iteration times.
///
/// The steady-state level is the median of the second half. An iteration belongs to the
/// warm-up when the rolling median (window 5) around it lies outside the band
/// `max(10% of the level, 3 sigma)`. The warm-up ends after the
/// last such window. A second half whose own halves differ by more than the drift band is still
/// drifting, and a warm-up that reaches into the second half never settles: both give
/// `no_steady_state`.
pub fn analyze(times: &[Second]) -> Analysis {
    let n = times.len();
    if n < MIN_ITERATIONS {
        return Analysis {
            status: SteadyStatus::TooFewIterations,
            warmup_iterations: None,
            steady_state: None,
        };
    }
    let none = Analysis {
        status: SteadyStatus::NoSteadyState,
        warmup_iterations: None,
        steady_state: None,
    };
    let tail = &times[n / 2..];
    let level = median(tail);
    // noise of the steady phase from consecutive differences: a drift or a step moves the
    // differences by a constant, so unlike the spread of the values they do not inflate it
    let steps: Vec<f64> = tail.windows(2).map(|w| w[1] - w[0]).collect();
    let step_median = median(&steps);
    let step_deviations: Vec<f64> = steps.iter().map(|d| (d - step_median).abs()).collect();
    let sigma = 1.4826 * median(&step_deviations) / std::f64::consts::SQRT_2;
    let floor = RELATIVE_BAND * level.abs();
    let band = floor.max(3.0 * sigma);
    let half = tail.len() / 2;
    let drift_band = floor.max(3.0 * 1.77 * sigma / (half as f64).sqrt());
    if (median(&tail[..half]) - median(&tail[half..])).abs() > drift_band {
        return none;
    }
    let last_bad = (0..=n - WINDOW)
        .rev()
        .find(|&j| (median(&times[j..j + WINDOW]) - level).abs() > band);
    let first = last_bad.map_or(0, |j| j + WINDOW / 2 + 1);
    if first >= n / 2 {
        return none;
    }
    let steady = &times[first..];
    let count = steady.len();
    let mean = steady.iter().sum::<f64>() / count as f64;
    let stddev = (count > 1).then(|| {
        (steady.iter().map(|t| (t - mean).powi(2)).sum::<f64>() / (count - 1) as f64).sqrt()
    });
    Analysis {
        status: SteadyStatus::SteadyState,
        warmup_iterations: Some(first),
        steady_state: Some(SteadyState {
            first_iteration: first,
            count,
            mean,
            stddev,
            min: steady.iter().copied().fold(f64::INFINITY, f64::min),
            max: steady.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        }),
    }
}

/// `results[].iterations`: the persistent-mode summary of a benchmark.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IterationsReport {
    /// Persistent protocol version spoken.
    pub protocol: u32,
    /// Identifier the SDK sent in its `hello`, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sdk: Option<String>,
    /// Number of measured iterations (the length of `times`).
    pub count: usize,
    pub status: SteadyStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warmup_iterations: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steady_state: Option<SteadyState>,
}

impl IterationsReport {
    pub fn from_times(times: &[Second], sdk: Option<String>) -> Self {
        let analysis = analyze(times);
        IterationsReport {
            protocol: PROTOCOL_VERSION,
            sdk,
            count: times.len(),
            status: analysis.status,
            warmup_iterations: analysis.warmup_iterations,
            steady_state: analysis.steady_state,
        }
    }
}

/// What one iteration reported.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Done(Second),
    Failed(String),
}

/// Turn the lines of one response (prefix removed, `done`/`fail` last) into an outcome.
/// Pure so the protocol is testable without a process.
pub fn parse_response(lines: &[String], expected: u64) -> Result<Outcome> {
    let (last, events) = lines
        .split_last()
        .ok_or_else(|| anyhow!("persistent protocol: empty response"))?;
    let mut parts = last.splitn(3, ' ');
    let kind = parts.next().unwrap_or_default();
    let number: u64 = parts
        .next()
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| anyhow!("persistent protocol: '{last}' needs the iteration number"))?;
    if number != expected {
        bail!("persistent protocol: the program answered iteration {number}, expected {expected}");
    }
    let text = || {
        let mut text = format!("hello {PROTOCOL_VERSION}\n");
        for line in events {
            text.push_str(line);
            text.push('\n');
        }
        text
    };
    let seconds = || -> Result<Second> {
        let report = crate::region::parse(&text())?.expect("a hello line is present");
        report
            .regions
            .get(ITERATION_REGION)
            .map(|t| t.seconds)
            .ok_or_else(|| {
                anyhow!(
                    "iteration {expected} reported no '{ITERATION_REGION}' interval: send \
                     'start {ITERATION_REGION} <t_ns>' and 'stop {ITERATION_REGION} <t_ns>' \
                     before 'done'"
                )
            })
    };
    match kind {
        "done" => Ok(Outcome::Done(seconds()?)),
        "fail" => Ok(Outcome::Failed(parts.next().unwrap_or("").to_string())),
        other => bail!("persistent protocol: unexpected '{other}' instead of done/fail"),
    }
}

/// The started program.
struct Process {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    sdk: Option<String>,
    next: u64,
}

impl Process {
    fn start(mut command: std::process::Command, stderr: Stdio) -> Result<Self> {
        command
            .env(ENV_PROTOCOL, PROTOCOL_VERSION.to_string())
            .env(crate::region::ENV_PROTOCOL, PROTOCOL_VERSION.to_string())
            .env_remove(crate::region::ENV_FILE)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr);
        let mut child = command
            .spawn()
            .context("Could not start the persistent process")?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("stdout is piped");
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(|l| l.ok()) {
                if let Some(message) = line.trim_end().strip_prefix(LINE_PREFIX) {
                    if tx.send(message.to_string()).is_err() {
                        break;
                    }
                }
            }
        });
        let mut process = Process {
            child,
            stdin,
            lines,
            sdk: None,
            next: 0,
        };
        let hello = process
            .next_line(Some(Instant::now() + HANDSHAKE_TIMEOUT))
            .context("The persistent process did not say 'hello'. Does it use the perfratio persistent SDK (see docs/specs/persistent-mode.md)?")?;
        if !hello.starts_with("hello ") {
            bail!("persistent protocol: expected 'hello {PROTOCOL_VERSION}' first, got '{hello}'");
        }
        let report = crate::region::parse(&format!("{hello}\n"))?.expect("hello was parsed");
        process.sdk = report.sdk;
        Ok(process)
    }

    fn next_line(&mut self, deadline: Option<Instant>) -> Result<String> {
        loop {
            match self.lines.recv_timeout(Duration::from_millis(50)) {
                Ok(line) => return Ok(line),
                Err(RecvTimeoutError::Timeout) => {
                    if deadline.is_some_and(|d| Instant::now() >= d) {
                        let _ = self.child.kill();
                        bail!("the persistent process did not answer in time");
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    let status = self.child.wait().ok();
                    bail!(
                        "the persistent process exited{} before it answered",
                        status.map_or(String::new(), |s| format!(" ({s})"))
                    );
                }
            }
        }
    }

    fn iterate(&mut self, timeout: Option<Duration>) -> Result<Outcome> {
        let number = self.next;
        self.next += 1;
        let deadline = timeout.map(|t| Instant::now() + t);
        let stdin = self.stdin.as_mut().expect("stdin is open while running");
        writeln!(stdin, "run {number}")
            .and_then(|_| stdin.flush())
            .map_err(|e| anyhow!("could not send iteration {number}: {e}"))?;
        let mut lines = Vec::new();
        loop {
            let line = self.next_line(deadline)?;
            let terminal = line.starts_with("done ") || line.starts_with("fail ");
            lines.push(line);
            if terminal {
                return parse_response(&lines, number);
            }
        }
    }

    fn shutdown(&mut self) {
        if let Some(mut stdin) = self.stdin.take() {
            let _ = writeln!(stdin, "quit");
        }
        let deadline = Instant::now() + QUIT_GRACE;
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn exit_status(code: i32) -> ExitStatus {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        ExitStatus::from_raw(code << 8)
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::ExitStatusExt;
        ExitStatus::from_raw(code as u32)
    }
}

/// Runs warm-up and benchmark iterations in one long-lived process per command and leaves
/// every other run (setup, prepare, conclude, cleanup) to `inner`.
pub struct PersistentExecutor<'a> {
    inner: Box<dyn Executor + 'a>,
    options: &'a Options,
    /// The process of the benchmark that is running: command line and process
    current: Mutex<Option<(String, Process)>>,
}

impl<'a> PersistentExecutor<'a> {
    pub fn new(inner: Box<dyn Executor + 'a>, options: &'a Options) -> Self {
        PersistentExecutor {
            inner,
            options,
            current: Mutex::new(None),
        }
    }
}

impl Executor for PersistentExecutor<'_> {
    fn run_command_and_measure(
        &self,
        command: &Command<'_>,
        iteration: BenchmarkIteration,
        command_failure_action: Option<CmdFailureAction>,
        output_policy: &CommandOutputPolicy,
    ) -> Result<(TimingResult, ExitStatus)> {
        if iteration == BenchmarkIteration::NonBenchmarkRun {
            return self.inner.run_command_and_measure(
                command,
                iteration,
                command_failure_action,
                output_policy,
            );
        }
        let key = command.get_command_line();
        let mut current = self.current.lock().unwrap_or_else(|e| e.into_inner());
        if current.as_ref().is_none_or(|(k, _)| *k != key) {
            // the previous benchmark is over: stop its process before the next one starts
            *current = None;
            let stderr = match output_policy {
                CommandOutputPolicy::Inherit => Stdio::inherit(),
                _ => Stdio::null(),
            };
            *current = Some((key, Process::start(command.get_command()?, stderr)?));
        }
        let (_, process) = current.as_mut().expect("process was started");
        let outcome = process.iterate(self.options.timeout)?;
        let sdk = process.sdk.clone();
        let (seconds, status) = match outcome {
            Outcome::Done(seconds) => (seconds, exit_status(0)),
            Outcome::Failed(message) => {
                let action = command_failure_action
                    .unwrap_or_else(|| self.options.command_failure_action.clone());
                let ignored = match action {
                    CmdFailureAction::RaiseError => false,
                    CmdFailureAction::IgnoreAllFailures => true,
                    CmdFailureAction::IgnoreSpecificFailures(codes) => codes.contains(&1),
                };
                if !ignored {
                    bail!(
                        "The persistent process reported a failed iteration: {message}. Use \
                         '-i' to ignore failures."
                    );
                }
                (0.0, exit_status(1))
            }
        };
        Ok((
            TimingResult {
                time_real: seconds,
                time_total: seconds,
                region_sdk: sdk,
                ..Default::default()
            },
            status,
        ))
    }

    fn calibrate(&mut self) -> Result<()> {
        self.inner.calibrate()
    }

    fn time_overhead(&self) -> Second {
        // the program times the iteration itself: no shell and no spawn is involved
        0.0
    }

    fn uses_shell(&self) -> bool {
        false
    }

    fn floor(&self) -> crate::calibration::FloorState {
        self.inner.floor()
    }

    fn set_floor(&mut self, floor: crate::calibration::FloorState) {
        self.inner.set_floor(floor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn too_few_iterations_are_not_analyzed() {
        let a = analyze(&[1.0; 9]);
        assert_eq!(a.status, SteadyStatus::TooFewIterations);
        assert_eq!(a.steady_state, None);
    }

    #[test]
    fn flat_series_has_no_warmup() {
        let a = analyze(&[1.0; 20]);
        assert_eq!(a.status, SteadyStatus::SteadyState);
        assert_eq!(a.warmup_iterations, Some(0));
        assert_eq!(a.steady_state.unwrap().count, 20);
    }

    #[test]
    fn step_warmup_is_detected() {
        let mut series = vec![10.0, 10.0, 10.0];
        series.extend([1.0; 17]);
        let a = analyze(&series);
        assert_eq!(a.warmup_iterations, Some(3));
        let steady = a.steady_state.unwrap();
        assert_eq!((steady.first_iteration, steady.count), (3, 17));
        assert_eq!((steady.mean, steady.min, steady.max), (1.0, 1.0, 1.0));
    }

    #[test]
    fn a_single_outlier_in_the_steady_phase_is_not_warmup() {
        let mut series = vec![5.0, 4.0, 3.0];
        series.extend([1.0; 20]);
        series[15] = 9.0;
        let a = analyze(&series);
        assert_eq!(a.status, SteadyStatus::SteadyState);
        assert_eq!(a.warmup_iterations, Some(3));
    }

    #[test]
    fn a_drifting_series_never_settles() {
        let series: Vec<f64> = (1..=40).map(|i| 100.0 - i as f64).collect();
        assert_eq!(analyze(&series).status, SteadyStatus::NoSteadyState);
    }

    #[test]
    fn a_late_settling_series_never_settles() {
        let mut series = vec![10.0; 15];
        series.extend([1.0; 5]);
        assert_eq!(analyze(&series).status, SteadyStatus::NoSteadyState);
    }

    #[test]
    fn response_time_comes_from_the_iteration_region() {
        let ok = parse_response(
            &lines(&["start iteration 1000", "stop iteration 3500", "done 4"]),
            4,
        )
        .unwrap();
        assert_eq!(ok, Outcome::Done(2.5e-6));
    }

    #[test]
    fn response_errors_are_specific() {
        let err = |l: &[&str], n| parse_response(&lines(l), n).unwrap_err().to_string();
        assert!(err(&["done 1"], 1).contains("no 'iteration' interval"));
        assert!(err(&["done 2"], 1).contains("expected 1"));
        assert!(err(&["start iteration 5", "done 1"], 1).contains("never stopped"));
        assert!(err(&["stop iteration 5", "done 1"], 1).contains("without a start"));
        assert_eq!(
            parse_response(&lines(&["fail 3 boom now"]), 3).unwrap(),
            Outcome::Failed("boom now".into())
        );
    }
}
