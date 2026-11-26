#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::process::ExitStatus;

use crate::command::Command;
use crate::options::{
    CmdFailureAction, CommandInputPolicy, CommandOutputPolicy, Options, OutputStyleOption, Shell,
};
use crate::output::progress_bar::get_progress_bar;
use crate::timer::{execute_and_measure, TimerResult};
use crate::util::priority::Priority;
use crate::util::randomized_environment_offset;
use crate::util::units::Second;

use super::timing_result::{InstanceTiming, ResourceCounters, TimingResult};

use crate::stats::basic::mean;
use anyhow::{anyhow, bail, Context, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchmarkIteration {
    NonBenchmarkRun,
    Warmup(u64),
    Benchmark(u64),
}

impl BenchmarkIteration {
    pub fn to_env_var_value(&self) -> Option<String> {
        match self {
            BenchmarkIteration::NonBenchmarkRun => None,
            BenchmarkIteration::Warmup(i) => Some(format!("warmup-{i}")),
            BenchmarkIteration::Benchmark(i) => Some(format!("{i}")),
        }
    }
}

pub trait Executor {
    /// Run the given command and measure the execution time
    fn run_command_and_measure(
        &self,
        command: &Command<'_>,
        iteration: BenchmarkIteration,
        command_failure_action: Option<CmdFailureAction>,
        output_policy: &CommandOutputPolicy,
    ) -> Result<(TimingResult, ExitStatus)>;

    /// Perform a calibration of this executor. For example,
    /// when running commands through a shell, we need to
    /// measure the shell spawning time separately in order
    /// to subtract it from the full runtime later.
    fn calibrate(&mut self) -> Result<()>;

    /// Return the time overhead for this executor when
    /// performing a measurement. This should return the time
    /// that is being used in addition to the actual runtime
    /// of the command.
    fn time_overhead(&self) -> Second;

    /// Whether commands run through an intermediate shell (whose spawning
    /// time is subtracted, which limits the accuracy for very fast commands)
    fn uses_shell(&self) -> bool {
        false
    }
}

/// How every benchmarked (and intermediate) process is started
#[derive(Clone, Copy)]
struct ProcessSettings<'a> {
    /// --affinity
    affinity: Option<&'a [usize]>,
    /// --priority
    priority: Priority,
    /// --timeout
    timeout: Option<std::time::Duration>,
    /// --until
    until: Option<&'a crate::options::UntilSettings>,
    /// --output-metric
    output_metrics: &'a [crate::output_metric::OutputMetric],
    /// --output-metric-stderr
    output_metrics_stderr: bool,
}

impl<'a> ProcessSettings<'a> {
    fn from_options(options: &'a Options) -> Self {
        ProcessSettings {
            affinity: options.affinity.as_deref(),
            priority: options.priority,
            timeout: options.timeout,
            until: options.until.as_ref(),
            output_metrics: &options.output_metrics,
            output_metrics_stderr: options.output_metrics_stderr,
        }
    }
}

fn run_command_and_measure_common(
    mut command: std::process::Command,
    iteration: BenchmarkIteration,
    command_failure_action: CmdFailureAction,
    command_input_policy: &CommandInputPolicy,
    command_output_policy: &CommandOutputPolicy,
    command_name: &str,
    process: ProcessSettings,
) -> Result<TimerResult> {
    let ProcessSettings {
        affinity,
        priority,
        timeout,
        until,
        output_metrics,
        output_metrics_stderr,
    } = process;
    let (until, capture_metrics) = match iteration {
        BenchmarkIteration::NonBenchmarkRun => (None, false),
        BenchmarkIteration::Warmup(_) | BenchmarkIteration::Benchmark(_) => {
            (until, !output_metrics.is_empty())
        }
    };

    let stdin = command_input_policy.get_stdin()?;
    let (stdout, stderr) = match until {
        Some(u) if !u.match_stderr => (std::process::Stdio::piped(), std::process::Stdio::null()),
        Some(_) => (std::process::Stdio::null(), std::process::Stdio::piped()),
        None => {
            let (mut out, err) = command_output_policy.get_stdout_stderr()?;
            let mut err = err;
            if capture_metrics {
                if output_metrics_stderr {
                    err = std::process::Stdio::piped();
                } else {
                    out = std::process::Stdio::piped();
                }
            }
            (out, err)
        }
    };
    command.stdin(stdin).stdout(stdout).stderr(stderr);

    command.env(
        "PERFRATIO_RANDOMIZED_ENVIRONMENT_OFFSET",
        randomized_environment_offset::value(),
    );

    if let Some(value) = iteration.to_env_var_value() {
        command.env("PERFRATIO_ITERATION", &value);
        // Deprecated aliases, removed in the next release (see warn_legacy_iteration_vars)
        command.env(crate::util::legacy::LEGACY_ITERATION_VARS[0], &value);
        command.env(crate::util::legacy::LEGACY_ITERATION_VARS[1], value);
    }

    #[cfg(target_os = "linux")]
    if let Some(cpus) = affinity {
        crate::util::affinity::apply(&mut command, cpus);
    }

    #[cfg(unix)]
    crate::util::priority::apply(&mut command, priority);

    let interrupted_before = crate::util::interrupt::interrupted();
    let capture = *command_output_policy == CommandOutputPolicy::CaptureTail;
    let metrics_limit = capture_metrics.then_some(1 << 20);
    let stderr_capture_limit = metrics_limit.filter(|_| output_metrics_stderr);
    let stdout_capture_limit = if capture_metrics && !output_metrics_stderr {
        metrics_limit
    } else if capture {
        Some(crate::timer::CAPTURE_LIMIT)
    } else {
        None
    };
    let mut result = execute_and_measure(
        command,
        affinity,
        priority,
        capture,
        timeout,
        until,
        stdout_capture_limit,
        stderr_capture_limit,
    )
    .map_err(|error| {
        // A priority that needs privileges fails in the child: explain how
        // to get them
        let denied = error
            .downcast_ref::<std::io::Error>()
            .is_some_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied);
        match crate::util::priority::permission_hint(priority) {
            Some(hint) if denied => error.context(format!(
                "could not set '--priority {}': {hint}",
                priority.as_str()
            )),
            _ => error,
        }
    })
    .with_context(|| format!("Failed to run command '{command_name}'"))?;

    // If an interruption occurred while running the command, discard the run
    // regardless of whether the child exited 0 (e.g. child caught SIGINT and exited gracefully).
    if !interrupted_before && crate::util::interrupt::interrupted() {
        bail!(crate::error::Interrupted);
    }

    if !result.status.success() {
        if crate::util::interrupt::interrupted() {
            bail!(crate::error::Interrupted);
        }

        // If the process was terminated by the timeout watchdog, return Ok(result)
        // so callers can handle the timeout cleanly without failing as an unhandled error.
        if result.timed_out {
            return Ok(result);
        }

        use crate::util::exit_code::extract_exit_code;

        let should_fail = match command_failure_action {
            CmdFailureAction::RaiseError => true,
            CmdFailureAction::IgnoreAllFailures => false,
            CmdFailureAction::IgnoreSpecificFailures(ref codes) => {
                // Only fail if the exit code is not in the list of codes to ignore
                if let Some(exit_code) = extract_exit_code(result.status) {
                    !codes.contains(&exit_code)
                } else {
                    // If we can't extract an exit code, treat it as a failure
                    true
                }
            }
        };

        if should_fail {
            let when = match iteration {
                BenchmarkIteration::NonBenchmarkRun => "a non-benchmark run".to_string(),
                BenchmarkIteration::Warmup(0) => "the first warmup run".to_string(),
                BenchmarkIteration::Warmup(i) => format!("warmup iteration {i}"),
                BenchmarkIteration::Benchmark(0) => "the first benchmark run".to_string(),
                BenchmarkIteration::Benchmark(i) => format!("benchmark iteration {i}"),
            };
            let cause = if result.until_matched == Some(false) {
                "Command exited without matching '--until' pattern".to_string()
            } else {
                result
                    .status
                    .code()
                    .map_or("The process has been terminated by a signal".into(), |c| {
                        format!("Command terminated with non-zero exit code {c}")
                    })
            };
            match &result.captured {
                Some(captured) => bail!(
                    "{cause} in {when}. Use the '-i'/'--ignore-exit-code' option if you want \
                     to ignore this.\n{}",
                    format_captured_output(captured)
                ),
                None => bail!(
                    "{cause} in {when}. Use the '-i'/'--ignore-exit-code' option if you want \
                     to ignore this. Alternatively, use the '--show-output-on-failure' (or \
                     '--show-output') option to debug what went wrong."
                ),
            }
        } else if let Some(captured) = &result.captured {
            warn_failed_run_output(command_name, iteration, captured);
        }
    }

    if capture_metrics {
        let stdout = result
            .captured
            .as_ref()
            .map(|c| {
                if output_metrics_stderr {
                    c.stderr.as_slice()
                } else {
                    c.stdout.as_slice()
                }
            })
            .unwrap_or(&[]);
        for metric in output_metrics {
            if let Some(val) = metric.extract(stdout) {
                result.custom_metrics.insert(metric.name.clone(), val);
            } else {
                let should_fail = match command_failure_action {
                    CmdFailureAction::RaiseError => true,
                    CmdFailureAction::IgnoreAllFailures => false,
                    CmdFailureAction::IgnoreSpecificFailures(_) => true,
                };
                let when = match iteration {
                    BenchmarkIteration::NonBenchmarkRun => "a non-benchmark run".to_string(),
                    BenchmarkIteration::Warmup(0) => "the first warmup run".to_string(),
                    BenchmarkIteration::Warmup(i) => format!("warmup iteration {i}"),
                    BenchmarkIteration::Benchmark(0) => "the first benchmark run".to_string(),
                    BenchmarkIteration::Benchmark(i) => format!("benchmark iteration {i}"),
                };
                if should_fail {
                    bail!(
                        "Metric '{}' could not be extracted from output in {when}. Use the '-i'/'--ignore-failure' option if you want to ignore this.",
                        metric.name
                    );
                } else {
                    eprintln!(
                        "{} metric '{}' could not be extracted from output in {when} (ignored).",
                        crate::output::colors::yellow("Warning:"),
                        metric.name
                    );
                }
            }
        }
    }

    Ok(result)
}

/// Number of lines of each stream shown for a failed run
const CAPTURED_LINES: usize = 20;
/// Longer lines (e.g. binary output) are cut to their last this many
/// characters (the end of the output is usually the interesting part)
const CAPTURED_LINE_CHARS: usize = 300;
/// With '-i', the output of at most this many failed runs is shown
const MAX_FAILED_RUN_OUTPUTS: usize = 3;

/// The last lines of a captured stream, with control characters escaped
/// (the output is untrusted and must not control the terminal).
fn tail_lines(bytes: &[u8]) -> Option<(usize, String)> {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.lines().collect();
    if lines.iter().all(|line| line.trim().is_empty()) {
        return None;
    }
    let tail = &lines[lines.len().saturating_sub(CAPTURED_LINES)..];
    let shown = tail
        .iter()
        .map(|line| {
            let chars = line.chars().count();
            let cut = chars.saturating_sub(CAPTURED_LINE_CHARS);
            let line: String = line.chars().skip(cut).collect();
            let line = crate::util::sanitize::escape_control_chars(&line).into_owned();
            if cut > 0 {
                format!("({cut} earlier characters) … {line}")
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    Some((tail.len(), shown))
}

/// The captured stderr and stdout of a failed run, for an error or warning.
fn format_captured_output(captured: &crate::timer::CapturedOutput) -> String {
    let mut out = String::new();
    for (name, bytes) in [("stderr", &captured.stderr), ("stdout", &captured.stdout)] {
        if let Some((count, lines)) = tail_lines(bytes) {
            out.push_str(&format!("──── {name} (last {count} lines) ────\n{lines}\n"));
        }
    }
    if out.is_empty() {
        out.push_str("(the command produced no output)\n");
    }
    out
}

/// '-i' with '--show-output-on-failure': show the output of the first failed
/// runs as a warning and continue.
fn warn_failed_run_output(
    command_name: &str,
    iteration: BenchmarkIteration,
    captured: &crate::timer::CapturedOutput,
) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static SHOWN: AtomicUsize = AtomicUsize::new(0);

    match SHOWN.fetch_add(1, Ordering::Relaxed) {
        shown if shown < MAX_FAILED_RUN_OUTPUTS => {
            let run = match iteration {
                BenchmarkIteration::Benchmark(i) => format!("benchmark iteration {i}"),
                BenchmarkIteration::Warmup(i) => format!("warmup iteration {i}"),
                BenchmarkIteration::NonBenchmarkRun => "a non-benchmark run".to_string(),
            };
            eprintln!(
                "{} '{}' failed in {run} (ignored):\n{}",
                crate::output::colors::yellow("Warning:"),
                crate::util::sanitize::escape_control_chars(command_name),
                format_captured_output(captured)
            );
        }
        MAX_FAILED_RUN_OUTPUTS => eprintln!(
            "{} the output of further failed runs is not shown.",
            crate::output::colors::yellow("Warning:")
        ),
        _ => {}
    }
}

/// Number of instances of one execution: `--concurrency` for warmup and
/// benchmark runs, 1 for `--prepare`, `--conclude`, `--setup` and `--cleanup`.
fn batch_size(options: &Options, iteration: BenchmarkIteration) -> usize {
    batch_size_of(options.concurrency, iteration)
}

fn batch_size_of(concurrency: usize, iteration: BenchmarkIteration) -> usize {
    match iteration {
        BenchmarkIteration::NonBenchmarkRun => 1,
        BenchmarkIteration::Warmup(_) | BenchmarkIteration::Benchmark(_) => concurrency,
    }
}

/// One finished instance of a `--concurrency` batch.
pub struct BatchInstance {
    /// Start of the instance, relative to the start of the batch
    pub offset: Second,
    pub result: TimerResult,
}

/// Start `n` instances at the same time (one thread each, released together by
/// a barrier) and wait for all of them. Every instance is timed by its own
/// timer, so an early finisher is not charged the time of a later one.
fn run_batch(
    n: usize,
    run_one: impl Fn() -> Result<TimerResult> + Sync,
) -> Result<Vec<BatchInstance>> {
    let barrier = std::sync::Barrier::new(n);
    let base = std::time::Instant::now();
    let outcomes: Vec<Result<BatchInstance>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..n)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    let offset = base.elapsed().as_secs_f64();
                    run_one().map(|result| BatchInstance { offset, result })
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|_| Err(anyhow!("a benchmark instance thread panicked")))
            })
            .collect()
    });
    // Report an interruption first, otherwise the first failing instance
    let mut instances = Vec::with_capacity(n);
    let mut first_error = None;
    for outcome in outcomes {
        match outcome {
            Ok(instance) => instances.push(instance),
            Err(error) => {
                let interrupted = error.downcast_ref::<crate::error::Interrupted>().is_some();
                if first_error.is_none() || interrupted {
                    first_error = Some(error);
                }
            }
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(instances),
    }
}

/// Combine the instances of one run into the figures of the run (see
/// `docs/specs/concurrency.md`):
/// - `time_real`: makespan, from the earliest process start to the latest exit;
/// - `time_total`: the same, including the spawn overhead;
/// - `time_user`/`time_system`: sum over the instances;
/// - `memory_usage_byte`: maximum of any instance (the sum would count shared pages twice);
/// - `status`: that of the first failed instance, else of the first instance;
/// - `timed_out`: any instance timed out.
pub fn aggregate_batch(instances: Vec<BatchInstance>) -> (TimerResult, Vec<InstanceTiming>) {
    use crate::util::exit_code::extract_exit_code;
    let end = |i: &BatchInstance| i.offset + i.result.time_total;
    let start_total = instances.iter().map(|i| i.offset).fold(f64::MAX, f64::min);
    let start_real = instances
        .iter()
        .map(|i| i.offset + (i.result.time_total - i.result.time_real).max(0.0))
        .fold(f64::MAX, f64::min);
    let last_end = instances.iter().map(end).fold(0.0, f64::max);

    let timings = instances
        .iter()
        .map(|i| InstanceTiming {
            time_real: i.result.time_real,
            exit_code: extract_exit_code(i.result.status),
        })
        .collect();

    let counters = instances
        .iter()
        .filter_map(|i| i.result.counters)
        .reduce(|a, b| ResourceCounters {
            voluntary_ctx_switches: a.voluntary_ctx_switches + b.voluntary_ctx_switches,
            involuntary_ctx_switches: a.involuntary_ctx_switches + b.involuntary_ctx_switches,
            minor_faults: a.minor_faults + b.minor_faults,
            major_faults: a.major_faults + b.major_faults,
            block_input_ops: a.block_input_ops + b.block_input_ops,
            block_output_ops: a.block_output_ops + b.block_output_ops,
        });

    let status = instances
        .iter()
        .find(|i| !i.result.status.success())
        .unwrap_or(&instances[0])
        .result
        .status;
    let aggregate = TimerResult {
        time_real: (last_end - start_real).max(0.0),
        time_total: (last_end - start_total).max(0.0),
        time_user: instances.iter().map(|i| i.result.time_user).sum(),
        time_system: instances.iter().map(|i| i.result.time_system).sum(),
        memory_usage_byte: instances
            .iter()
            .map(|i| i.result.memory_usage_byte)
            .max()
            .unwrap_or(0),
        counters,
        status,
        captured: None,
        timed_out: instances.iter().any(|i| i.result.timed_out),
        until_matched: None,
        custom_metrics: std::collections::BTreeMap::new(),
    };
    (aggregate, timings)
}

pub struct RawExecutor<'a> {
    options: &'a Options,
}

impl<'a> RawExecutor<'a> {
    pub fn new(options: &'a Options) -> Self {
        RawExecutor { options }
    }
}

impl Executor for RawExecutor<'_> {
    fn run_command_and_measure(
        &self,
        command: &Command<'_>,
        iteration: BenchmarkIteration,
        command_failure_action: Option<CmdFailureAction>,
        output_policy: &CommandOutputPolicy,
    ) -> Result<(TimingResult, ExitStatus)> {
        let command = command.for_iteration(&iteration);
        let action =
            command_failure_action.unwrap_or_else(|| self.options.command_failure_action.clone());
        let run_one = || {
            run_command_and_measure_common(
                command.get_command()?,
                iteration,
                action.clone(),
                &self.options.command_input_policy,
                output_policy,
                &command.get_command_line(),
                ProcessSettings::from_options(self.options),
            )
        };
        let (result, instances) = if batch_size(self.options, iteration) > 1 {
            aggregate_batch(run_batch(self.options.concurrency, run_one)?)
        } else {
            (run_one()?, Vec::new())
        };
        let status = result.status;

        Ok((
            TimingResult {
                time_real: result.time_real,
                time_total: result.time_total,
                time_user: result.time_user,
                time_system: result.time_system,
                memory_usage_byte: result.memory_usage_byte,
                energy_joules: None,
                counters: result.counters,
                timed_out: result.timed_out,
                custom_metrics: result.custom_metrics,
                instances,
            },
            status,
        ))
    }

    fn calibrate(&mut self) -> Result<()> {
        Ok(())
    }

    fn time_overhead(&self) -> Second {
        0.0
    }
}

pub struct ShellExecutor<'a> {
    options: &'a Options,
    shell: &'a Shell,
    shell_spawning_time: Option<TimingResult>,
}

impl<'a> ShellExecutor<'a> {
    pub fn new(shell: &'a Shell, options: &'a Options) -> Self {
        ShellExecutor {
            shell,
            options,
            shell_spawning_time: None,
        }
    }
}

/// `cmd.exe /C ./app.exe` fails ("'.' is not recognized as an internal or
/// external command"): cmd only understands backslashes in the program path.
/// Rewrite the forward slashes of a relative program path (`./…`, `../…`) to
/// backslashes. The arguments are left untouched.
#[cfg(any(windows, test))]
fn normalize_relative_command_path_for_cmd(command_line: &str) -> String {
    if !(command_line.starts_with("./") || command_line.starts_with("../")) {
        return command_line.to_string();
    }
    let end = command_line
        .find(char::is_whitespace)
        .unwrap_or(command_line.len());
    format!(
        "{}{}",
        command_line[..end].replace('/', "\\"),
        &command_line[end..]
    )
}

impl Executor for ShellExecutor<'_> {
    fn run_command_and_measure(
        &self,
        command: &Command<'_>,
        iteration: BenchmarkIteration,
        command_failure_action: Option<CmdFailureAction>,
        output_policy: &CommandOutputPolicy,
    ) -> Result<(TimingResult, ExitStatus)> {
        let command = command.for_iteration(&iteration);
        let on_windows_cmd = cfg!(windows) && *self.shell == Shell::Default("cmd.exe");
        let build = || {
            let mut command_builder = self.shell.command();
            command_builder.arg(if on_windows_cmd { "/C" } else { "-c" });

            // Windows needs special treatment for its behavior on parsing cmd arguments
            if on_windows_cmd {
                #[cfg(windows)]
                command_builder.raw_arg(normalize_relative_command_path_for_cmd(
                    &command.get_command_line(),
                ));
            } else {
                command_builder.arg(command.get_command_line());
            }
            command_builder
        };
        let action =
            command_failure_action.unwrap_or_else(|| self.options.command_failure_action.clone());
        let run_one = || {
            run_command_and_measure_common(
                build(),
                iteration,
                action.clone(),
                &self.options.command_input_policy,
                output_policy,
                &command.get_command_line(),
                ProcessSettings::from_options(self.options),
            )
        };
        let batch = batch_size(self.options, iteration);
        let (mut result, mut instances) = if batch > 1 {
            aggregate_batch(run_batch(batch, run_one)?)
        } else {
            (run_one()?, Vec::new())
        };

        // Subtract shell spawning time. The spawns of a batch overlap, so the
        // makespan loses it once; CPU time and each instance lose it per instance.
        if let Some(ref spawning_time) = self.shell_spawning_time {
            let copies = batch.max(1) as f64;
            result.time_real = (result.time_real - spawning_time.time_real).max(0.0);
            result.time_total = (result.time_total - spawning_time.time_total).max(0.0);
            result.time_user = (result.time_user - spawning_time.time_user * copies).max(0.0);
            result.time_system = (result.time_system - spawning_time.time_system * copies).max(0.0);
            for instance in &mut instances {
                instance.time_real = (instance.time_real - spawning_time.time_real).max(0.0);
            }
        }
        let status = result.status;

        Ok((
            TimingResult {
                time_real: result.time_real,
                time_total: result.time_total,
                time_user: result.time_user,
                time_system: result.time_system,
                memory_usage_byte: result.memory_usage_byte,
                energy_joules: None,
                counters: result.counters,
                timed_out: result.timed_out,
                custom_metrics: result.custom_metrics,
                instances,
            },
            status,
        ))
    }

    /// Measure the average shell spawning time
    fn calibrate(&mut self) -> Result<()> {
        const COUNT: u64 = 50;
        let progress_bar = if self.options.output_style != OutputStyleOption::Disabled {
            Some(get_progress_bar(
                COUNT,
                "Measuring shell spawning time",
                self.options.output_style,
            ))
        } else {
            None
        };

        let mut times_real: Vec<Second> = vec![];
        let mut times_user: Vec<Second> = vec![];
        let mut times_system: Vec<Second> = vec![];

        for _ in 0..COUNT {
            // Just run the shell without any command
            let res = self.run_command_and_measure(
                &Command::new(None, ""),
                BenchmarkIteration::NonBenchmarkRun,
                None,
                &CommandOutputPolicy::Null,
            );

            match res {
                Err(error) => {
                    let shell_cmd = if cfg!(windows) {
                        format!("{} /C \"\"", self.shell)
                    } else {
                        format!("{} -c \"\"", self.shell)
                    };

                    // Keep the cause (e.g. a '--priority' permission error)
                    return Err(error.context(format!(
                        "Could not measure the shell execution time (make sure you can run '{shell_cmd}')"
                    )));
                }
                Ok((r, _)) => {
                    times_real.push(r.time_real);
                    times_user.push(r.time_user);
                    times_system.push(r.time_system);
                }
            }

            if let Some(bar) = progress_bar.as_ref() {
                bar.inc(1)
            }
        }

        if let Some(bar) = progress_bar.as_ref() {
            bar.finish_and_clear()
        }

        self.shell_spawning_time = Some(TimingResult {
            time_real: mean(&times_real),
            time_total: mean(&times_real),
            time_user: mean(&times_user),
            time_system: mean(&times_system),
            memory_usage_byte: 0,
            energy_joules: None,
            counters: None,
            timed_out: false,
            custom_metrics: std::collections::BTreeMap::new(),
            instances: Vec::new(),
        });

        Ok(())
    }

    fn uses_shell(&self) -> bool {
        true
    }

    fn time_overhead(&self) -> Second {
        // Zero before `calibrate()` has measured the shell spawning time
        self.shell_spawning_time
            .as_ref()
            .map_or(0.0, |t| t.time_real)
    }
}

#[derive(Clone)]
pub struct MockExecutor {
    shell: Option<String>,
    timeout: Option<std::time::Duration>,
    concurrency: usize,
}

impl MockExecutor {
    pub fn new(shell: Option<String>, timeout: Option<std::time::Duration>) -> Self {
        MockExecutor {
            shell,
            timeout,
            concurrency: 1,
        }
    }

    /// `--concurrency N`: simulate N instances per run. Instance `i` takes
    /// `(1 + 0.01 * i)` times the requested time, so the makespan is that of
    /// the last instance and the instance times are distinct but deterministic.
    pub fn with_concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency.max(1);
        self
    }

    /// `--debug-mode` doesn't run anything: it only understands commands of
    /// the form `sleep <seconds> [user] [system] [memory]` and reports those measurements.
    fn extract_measurements<S: AsRef<str>>(
        sleep_command: S,
    ) -> Result<(Second, Second, Second, u64)> {
        let command = sleep_command.as_ref();
        let parts: Vec<&str> = command.split_whitespace().collect();
        if parts.is_empty() || parts[0] != "sleep" {
            bail!(
                "'--debug-mode' only simulates commands of the form 'sleep <seconds>', \
                 got '{command}'"
            );
        }
        if parts.len() < 2 {
            bail!("'--debug-mode' requires sleep duration, got '{command}'");
        }
        let requested = parts[1]
            .parse::<Second>()
            .ok()
            .filter(|s| s.is_finite() && *s >= 0.0)
            .ok_or_else(|| {
                anyhow!(
                    "'--debug-mode' only simulates commands of the form 'sleep <seconds>', \
                     got '{command}'"
                )
            })?;

        let time_user = if parts.len() > 2 {
            parts[2]
                .parse::<Second>()
                .ok()
                .filter(|s| s.is_finite() && *s >= 0.0)
                .ok_or_else(|| anyhow!("Invalid user time in '{command}'"))?
        } else {
            0.0
        };

        let time_system = if parts.len() > 3 {
            parts[3]
                .parse::<Second>()
                .ok()
                .filter(|s| s.is_finite() && *s >= 0.0)
                .ok_or_else(|| anyhow!("Invalid system time in '{command}'"))?
        } else {
            0.0
        };

        let memory_usage_byte = if parts.len() > 4 {
            parts[4]
                .parse::<u64>()
                .map_err(|_| anyhow!("Invalid memory bytes in '{command}'"))?
        } else {
            0
        };

        Ok((requested, time_user, time_system, memory_usage_byte))
    }

    fn extract_time<S: AsRef<str>>(sleep_command: S) -> Result<Second> {
        Self::extract_measurements(sleep_command).map(|m| m.0)
    }
}

impl Executor for MockExecutor {
    fn run_command_and_measure(
        &self,
        command: &Command<'_>,
        iteration: BenchmarkIteration,
        _command_failure_action: Option<CmdFailureAction>,
        _output_policy: &CommandOutputPolicy,
    ) -> Result<(TimingResult, ExitStatus)> {
        let command = command.for_iteration(&iteration);
        #[cfg(unix)]
        let status = {
            use std::os::unix::process::ExitStatusExt;
            ExitStatus::from_raw(0)
        };

        #[cfg(windows)]
        let status = {
            use std::os::windows::process::ExitStatusExt;
            ExitStatus::from_raw(0)
        };

        let (requested, time_user, time_system, memory_usage_byte) =
            Self::extract_measurements(command.get_command_line())?;
        let timed_out = self.timeout.is_some_and(|t| requested > t.as_secs_f64());
        let time_real = if timed_out {
            self.timeout.unwrap().as_secs_f64()
        } else {
            requested
        };

        let n = batch_size_of(self.concurrency, iteration);
        let instances: Vec<InstanceTiming> = if n > 1 {
            (0..n)
                .map(|i| InstanceTiming {
                    time_real: time_real * (1.0 + 0.01 * i as f64),
                    exit_code: Some(0),
                })
                .collect()
        } else {
            Vec::new()
        };
        let copies = n as f64;
        let time_real = instances.last().map_or(time_real, |i| i.time_real);

        Ok((
            TimingResult {
                time_real,
                time_total: time_real,
                time_user: time_user * copies,
                time_system: time_system * copies,
                memory_usage_byte,
                energy_joules: None,
                counters: None,
                timed_out,
                custom_metrics: std::collections::BTreeMap::new(),
                instances,
            },
            status,
        ))
    }

    fn calibrate(&mut self) -> Result<()> {
        // Validate the simulated shell (`--debug-mode --shell 'sleep …'`), so
        // that `time_overhead` can't fail later
        if let Some(shell) = &self.shell {
            Self::extract_time(shell)?;
        }
        Ok(())
    }

    fn time_overhead(&self) -> Second {
        self.shell
            .as_ref()
            .and_then(|shell| Self::extract_time(shell).ok())
            .unwrap_or(0.0)
    }
}

#[test]
fn test_mock_executor_extract_time() {
    assert_eq!(MockExecutor::extract_time("sleep 0.1").unwrap(), 0.1);
    for invalid in ["echo hi", "sleep", "sleep abc", "sleep -1", "sleep inf"] {
        assert!(MockExecutor::extract_time(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn test_normalize_relative_command_path_for_cmd() {
    for (input, expected) in [
        ("./app.exe", ".\\app.exe"),
        (
            "./target/release/app.exe --x a/b",
            ".\\target\\release\\app.exe --x a/b",
        ),
        ("../bin/app.exe\t./x", "..\\bin\\app.exe\t./x"),
        ("app.exe ./x", "app.exe ./x"),
        ("C:/tools/app.exe", "C:/tools/app.exe"),
        (".hidden/app", ".hidden/app"),
        ("", ""),
    ] {
        assert_eq!(normalize_relative_command_path_for_cmd(input), expected);
    }
}

#[test]
fn captured_output_is_tailed_escaped_and_cut() {
    let many_lines: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    let (count, text) = tail_lines(many_lines.as_bytes()).unwrap();
    assert_eq!(count, CAPTURED_LINES);
    assert!(text.starts_with("line 11\n") && text.ends_with("line 30"));

    let (_, text) = tail_lines(b"\x1b[31mred\x07").unwrap();
    assert_eq!(text, "\\u{1b}[31mred\\u{7}");

    let (_, text) = tail_lines(&[b'x'; 1000]).unwrap();
    assert!(text.starts_with("(700 earlier characters) … x"), "{text}");
    let mut long_line = vec![0u8; 1000];
    long_line.extend_from_slice(b"the error");
    let (_, text) = tail_lines(&long_line).unwrap();
    assert!(text.ends_with("the error"), "{text}");

    assert!(tail_lines(b"").is_none());
    assert!(tail_lines(b"\n  \n").is_none());
    assert_eq!(
        format_captured_output(&crate::timer::CapturedOutput::default()),
        "(the command produced no output)\n"
    );
}

#[cfg(test)]
mod batch_tests {
    use super::*;

    fn status(code: i32) -> ExitStatus {
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

    fn instance(offset: f64, real: f64, user: f64, memory: u64, code: i32) -> BatchInstance {
        BatchInstance {
            offset,
            result: TimerResult {
                time_real: real,
                time_total: real,
                time_user: user,
                time_system: user / 2.0,
                memory_usage_byte: memory,
                counters: None,
                status: status(code),
                captured: None,
                timed_out: false,
                until_matched: None,
                custom_metrics: Default::default(),
            },
        }
    }

    #[test]
    fn early_finisher_is_not_charged_the_later_finishers_time() {
        // instance 0 starts at 0 and runs 0.1 s, instance 1 starts at 0.01 and runs 0.3 s
        let (aggregate, timings) = aggregate_batch(vec![
            instance(0.0, 0.1, 0.1, 10, 0),
            instance(0.01, 0.3, 0.2, 30, 0),
        ]);
        assert_eq!(timings[0].time_real, 0.1);
        assert_eq!(timings[1].time_real, 0.3);
        // makespan: from the first start (0) to the last exit (0.01 + 0.3)
        assert!((aggregate.time_real - 0.31).abs() < 1e-12);
        assert!((aggregate.time_user - 0.3).abs() < 1e-12);
        assert!((aggregate.time_system - 0.15).abs() < 1e-12);
        assert_eq!(aggregate.memory_usage_byte, 30);
        assert!(aggregate.status.success());
    }

    #[test]
    fn one_failing_instance_fails_the_run() {
        let (aggregate, timings) = aggregate_batch(vec![
            instance(0.0, 0.1, 0.0, 1, 0),
            instance(0.0, 0.1, 0.0, 1, 3),
        ]);
        assert!(!aggregate.status.success());
        assert_eq!(timings[0].exit_code, Some(0));
        assert_eq!(timings[1].exit_code, Some(3));
    }

    #[test]
    fn batch_runs_every_instance() {
        let counter = std::sync::atomic::AtomicUsize::new(0);
        let instances = run_batch(5, || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(instance(0.0, 0.0, 0.0, 0, 0).result)
        })
        .unwrap();
        assert_eq!(instances.len(), 5);
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 5);
        assert!(run_batch(3, || Err(anyhow!("boom"))).is_err());
    }
}
