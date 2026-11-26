use std::fs::File;
#[cfg(feature = "cli")]
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::{Command, Stdio};
#[cfg(feature = "cli")]
use std::{cmp, env};
use std::{fmt, io};

use anyhow::{bail, ensure, Result};
#[cfg(feature = "cli")]
use clap::ArgMatches;

use crate::command::Commands;
use crate::error::OptionsError;
use crate::util::units::{Second, Unit};

#[cfg(not(windows))]
pub const DEFAULT_SHELL: &str = "sh";

#[cfg(windows)]
pub const DEFAULT_SHELL: &str = "cmd.exe";

/// Shell to use for executing benchmarked commands
#[derive(Debug, Clone, PartialEq)]
pub enum Shell {
    /// Default shell command
    Default(&'static str),

    /// Custom shell command specified via --shell
    Custom(Vec<String>),
}

impl Default for Shell {
    fn default() -> Self {
        Shell::Default(DEFAULT_SHELL)
    }
}

impl fmt::Display for Shell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Shell::Default(cmd) => write!(f, "{cmd}"),
            Shell::Custom(cmdline) => write!(f, "{}", shell_words::join(cmdline)),
        }
    }
}

impl Shell {
    /// Parse given string as shell command line
    pub fn parse_from_str<'a>(s: &str) -> Result<Self, OptionsError<'a>> {
        let v = shell_words::split(s).map_err(OptionsError::ShellParseError)?;
        if v.is_empty() || v[0].is_empty() {
            return Err(OptionsError::EmptyShell);
        }
        Ok(Shell::Custom(v))
    }

    pub fn command(&self) -> Command {
        match self {
            Shell::Default(cmd) => Command::new(cmd),
            Shell::Custom(cmdline) => {
                let mut c = Command::new(&cmdline[0]);
                c.args(&cmdline[1..]);
                c
            }
        }
    }
}

/// Action to take when an executed command fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CmdFailureAction {
    /// Exit with an error message
    RaiseError,

    /// Ignore all non-zero exit codes
    IgnoreAllFailures,

    /// Ignore specific exit codes
    IgnoreSpecificFailures(Vec<i32>),
}

/// Output style type option
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStyleOption {
    /// Do not output with colors or any special formatting
    Basic,

    /// Output with full color and formatting
    Full,

    /// Keep elements such as progress bar, but use no coloring
    NoColor,

    /// Keep coloring, but use no progress bar
    Color,

    /// Disable all the output
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    Command,
    MeanTime,
}

/// Bounds for the number of benchmark runs
pub struct RunBounds {
    /// Minimum number of benchmark runs
    pub min: u64,

    /// Maximum number of benchmark runs
    pub max: Option<u64>,
}

impl Default for RunBounds {
    fn default() -> Self {
        RunBounds { min: 10, max: None }
    }
}

/// `--first-run`: how the first (cold) timing run of each command is treated
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FirstRunPolicy {
    /// Part of the statistics (default)
    #[default]
    Include,

    /// Reported on its own line (and in the JSON export) and excluded from
    /// the statistics
    Separate,

    /// Silently excluded from the statistics, like an extra warmup run
    Discard,
}

/// Benchmark execution schedule mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScheduleMode {
    /// Execute all iterations for one command before proceeding to the next
    #[default]
    Grouped,

    /// Interleave iterations across all commands in round-robin fashion
    RoundRobin,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub enum CommandInputPolicy {
    /// Read from the null device
    #[default]
    Null,

    /// Read input from a file
    File(PathBuf),
}

impl CommandInputPolicy {
    pub fn get_stdin(&self) -> io::Result<Stdio> {
        let stream: Stdio = match self {
            CommandInputPolicy::Null => Stdio::null(),

            CommandInputPolicy::File(path) => {
                let file: File = File::open(path)?;
                Stdio::from(file)
            }
        };

        Ok(stream)
    }
}

/// How to handle the output of benchmarked commands
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CommandOutputPolicy {
    /// Redirect output to the null device
    #[default]
    Null,

    /// Feed output through a pipe before discarding it
    Pipe,

    /// Redirect output to a file
    File(PathBuf),

    /// Show command output on the terminal
    Inherit,

    /// Capture the tail of stdout and stderr, and show it only if a run fails
    /// (`--show-output-on-failure`)
    CaptureTail,
}

impl CommandOutputPolicy {
    pub fn get_stdout_stderr(&self) -> io::Result<(Stdio, Stdio)> {
        let streams = match self {
            CommandOutputPolicy::Null => (Stdio::null(), Stdio::null()),

            // Typically only stdout is performance-relevant, so just pipe that
            CommandOutputPolicy::Pipe => (Stdio::piped(), Stdio::null()),

            CommandOutputPolicy::File(path) => {
                let file = File::create(path)?;
                (file.into(), Stdio::null())
            }

            CommandOutputPolicy::Inherit => (Stdio::inherit(), Stdio::inherit()),

            CommandOutputPolicy::CaptureTail => (Stdio::piped(), Stdio::piped()),
        };

        Ok(streams)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExecutorKind {
    Raw,
    Shell(Shell),
    Mock(Option<String>),
}

impl ExecutorKind {
    /// Shown per result when commands use different shells
    pub fn name(&self) -> String {
        match self {
            ExecutorKind::Raw => "none".to_string(),
            ExecutorKind::Shell(shell) => shell.to_string(),
            ExecutorKind::Mock(shell) => shell.clone().unwrap_or_else(|| "default".to_string()),
        }
    }
}

impl Options {
    /// The executor kind of command `number` (the reference, if any, is 0).
    pub fn executor_kind_for(&self, number: usize) -> &ExecutorKind {
        match self.executor_kinds.as_slice() {
            [single] => single,
            kinds => kinds.get(number).unwrap_or(&self.executor_kind),
        }
    }

    /// Whether commands use different shells (`--shell` given per command).
    pub fn has_per_command_shells(&self) -> bool {
        self.executor_kinds
            .iter()
            .any(|kind| kind != &self.executor_kinds[0])
    }
}

impl Default for ExecutorKind {
    fn default() -> Self {
        ExecutorKind::Shell(Shell::default())
    }
}

/// Settings for stopping a benchmark run upon readiness output (`--until`)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UntilSettings {
    pub pattern: Vec<u8>,
    pub match_stderr: bool,
}

/// The main settings for a perfratio benchmark session
pub struct Options {
    /// Upper and lower bound for the number of benchmark runs
    pub run_bounds: RunBounds,

    /// Number of warmup runs
    pub warmup_count: u64,

    /// `--warmup auto`: warm up until the timings are stable
    pub warmup_auto: bool,

    /// `--first-run`: how the first timing run is treated
    pub first_run: FirstRunPolicy,

    /// Minimum benchmarking time
    pub min_benchmarking_time: Second,

    /// `--target-precision`: run until the 95% CI of the mean is at most this
    /// fraction of the mean (0.01 = ±1%)
    pub target_precision: Option<f64>,

    /// `--duration`: sample each command until its summed run time (seconds)
    /// reaches this budget
    pub duration: Option<Second>,

    /// `--max-benchmarking-time`: time budget per command for
    /// `--target-precision`
    pub max_benchmarking_time: Second,

    /// Whether or not to ignore non-zero exit codes
    pub command_failure_action: CmdFailureAction,

    // Command to use as a reference for relative speed comparison
    pub reference_command: Option<String>,

    // Name of the reference command
    pub reference_name: Option<String>,

    /// Command(s) to run before each timing run
    pub preparation_command: Option<Vec<String>>,

    /// Command(s) to run after each timing run
    pub conclusion_command: Option<Vec<String>>,

    /// Command to run before each *batch* of timing runs, i.e. before each individual benchmark
    pub setup_command: Option<Vec<String>>,

    /// Command to run after each *batch* of timing runs, i.e. after each individual benchmark
    pub cleanup_command: Option<Vec<String>>,

    /// What color mode to use for the terminal output
    pub output_style: OutputStyleOption,

    /// How to order benchmarks in the relative speed comparison
    pub sort_order_speed_comparison: SortOrder,

    /// How to order benchmarks in the markup format exports
    pub sort_order_exports: SortOrder,

    /// Determines how we run commands
    pub executor_kind: ExecutorKind,

    /// The executor of each command (including a potential reference, which
    /// comes first): one entry for all commands, or one per command
    /// (`--shell` given N times)
    pub executor_kinds: Vec<ExecutorKind>,

    /// Where input to the benchmarked command comes from
    pub command_input_policy: CommandInputPolicy,

    /// What to do with the output of the benchmarked commands
    pub command_output_policies: Vec<CommandOutputPolicy>,

    /// Which time unit to use when displaying results
    pub time_unit: Option<Unit>,

    /// Primary metric used for statistics, outlier detection, comparison, and sorting
    pub metric: crate::metric::Metric,

    /// `--optimize`: search a parameter for the best value of the primary metric
    pub optimize: Option<crate::optimize::OptimizeSpec>,

    /// `--pareto`, `--constraint`, `--objective`
    pub pareto: Option<crate::pareto::ParetoConfig>,

    /// Measure energy consumption in Joules and display Performance per Watt
    pub measure_energy: bool,

    /// Idle-power measurement window for net energy (--energy-baseline)
    pub energy_baseline: Option<std::time::Duration>,
    /// Report energy per RAPL domain (`--energy-domains`)
    pub energy_domains: bool,
    /// Which energy backend to use (`--energy-backend`)
    pub energy_backend: crate::energy::BackendPreference,

    /// Calculate deep statistics (confidence intervals, bootstrapping)
    pub deep_stats: bool,

    /// Whether to exclude results with non-zero exit codes from comparisons and exports
    pub filter_failed: bool,

    /// Suppress statistical outlier warnings
    pub suppress_outlier_warnings: bool,

    /// Method for the outlier report and warning (--outlier-method)
    pub outlier_method: crate::outlier_detection::OutlierMethod,

    /// Suppress off-CPU time warnings
    pub suppress_off_cpu_warnings: bool,

    /// Execution schedule mode
    pub schedule: ScheduleMode,

    /// Whether to exclude failed runs from summary statistics
    pub omit_failed_runs: bool,

    /// `--expect-output-sha256` / `--validate`: check the result of each run
    pub validation: Option<crate::validation::ValidationConfig>,

    /// `--quality`: what the terminal shows of the measurement quality report
    pub quality_mode: crate::quality::QualityMode,

    /// `--fail-on-quality`: exit non-zero when a benchmark's grade is worse
    pub fail_on_quality: Option<crate::quality::Grade>,

    /// Modified Z-score above which runs are discarded (--discard-outliers)
    pub discard_outliers: Option<f64>,

    /// Show and export OS resource counters (--resource-usage)
    pub show_resource_usage: bool,

    /// Hardware performance counters (--counters), resolved against the machine
    pub counters: Option<crate::perf_counters::CounterConfig>,

    /// Top-down analysis (--topdown), resolved against the machine
    pub topdown: Option<crate::topdown::TopdownConfig>,

    /// CPUs to pin every benchmarked process to (--affinity)
    pub affinity: Option<Vec<usize>>,
    /// Core type to prefer (--affinity p-cores|e-cores, macOS)
    pub core_class: Option<crate::util::affinity::CoreClass>,

    /// `--subtract`: a baseline command whose mean is subtracted from every run
    pub subtract_command: Option<String>,

    /// Scheduling priority of every benchmarked process (--priority)
    pub priority: crate::util::priority::Priority,

    /// Allow combining parametrized '--setup' or '--cleanup' with round-robin scheduling
    pub allow_setup_with_round_robin: bool,

    /// Kill a benchmark run that exceeds this duration (--timeout)
    pub timeout: Option<std::time::Duration>,

    /// `--concurrency`: instances of the command started simultaneously per run
    pub concurrency: usize,

    /// `--concurrency` value containing `{param}`: resolved per benchmark
    /// (`Command::with_concurrency_template`); `concurrency` stays 1
    pub concurrency_template: Option<String>,

    /// `--until`: stop timer upon readiness output pattern
    pub until: Option<UntilSettings>,

    /// `--output-metric`: custom metrics to extract from command output
    pub output_metrics: Vec<crate::output_metric::OutputMetric>,

    /// Extract `--output-metric` values from stderr instead of stdout
    pub output_metrics_stderr: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            run_bounds: RunBounds::default(),
            warmup_count: 0,
            warmup_auto: false,
            first_run: FirstRunPolicy::default(),
            min_benchmarking_time: 3.0,
            target_precision: None,
            duration: None,
            max_benchmarking_time: 60.0,
            command_failure_action: CmdFailureAction::RaiseError,
            reference_command: None,
            reference_name: None,
            preparation_command: None,
            conclusion_command: None,
            setup_command: None,
            cleanup_command: None,
            output_style: OutputStyleOption::Full,
            sort_order_speed_comparison: SortOrder::MeanTime,
            sort_order_exports: SortOrder::Command,
            executor_kind: ExecutorKind::default(),
            executor_kinds: vec![ExecutorKind::default()],
            command_output_policies: vec![CommandOutputPolicy::Null],
            time_unit: None,
            command_input_policy: CommandInputPolicy::Null,
            metric: crate::metric::Metric::Wall,
            optimize: None,
            pareto: None,
            measure_energy: false,
            energy_baseline: None,
            energy_domains: false,
            energy_backend: crate::energy::BackendPreference::Auto,
            deep_stats: false,
            filter_failed: false,
            suppress_outlier_warnings: false,
            outlier_method: Default::default(),
            suppress_off_cpu_warnings: false,
            schedule: ScheduleMode::Grouped,
            omit_failed_runs: false,
            validation: None,
            quality_mode: Default::default(),
            fail_on_quality: None,
            discard_outliers: None,
            show_resource_usage: false,
            counters: None,
            topdown: None,
            affinity: None,
            core_class: None,
            priority: crate::util::priority::Priority::default(),
            subtract_command: None,
            allow_setup_with_round_robin: false,
            timeout: None,
            concurrency: 1,
            concurrency_template: None,
            until: None,
            output_metrics: Vec::new(),
            output_metrics_stderr: false,
        }
    }
}

impl Options {
    #[cfg(feature = "cli")]
    pub fn from_cli_arguments<'a>(matches: &ArgMatches) -> Result<Self, OptionsError<'a>> {
        let mut options = Self::default();
        let param_to_u64 = |param| {
            matches
                .get_one::<String>(param)
                .map(|n| {
                    n.parse::<u64>()
                        .map_err(|e| OptionsError::IntParsingError(param, e))
                })
                .transpose()
        };

        if matches.get_one::<String>("warmup").map(String::as_str) == Some("auto") {
            options.warmup_auto = true;
        } else {
            options.warmup_count = param_to_u64("warmup")?.unwrap_or(options.warmup_count);
        }

        options.first_run = match matches.get_one::<String>("first-run").map(String::as_str) {
            Some("separate") => FirstRunPolicy::Separate,
            Some("discard") => FirstRunPolicy::Discard,
            // clap only accepts the values above and "include"
            _ => FirstRunPolicy::Include,
        };

        let mut min_runs = param_to_u64("min-runs")?;
        let mut max_runs = param_to_u64("max-runs")?;

        if let Some(runs) = param_to_u64("runs")? {
            if runs == 0 {
                return Err(OptionsError::ZeroRuns("runs"));
            }
            min_runs = Some(runs);
            max_runs = Some(runs);
        } else if max_runs == Some(0) {
            return Err(OptionsError::ZeroRuns("max-runs"));
        }

        match (min_runs, max_runs) {
            (Some(min), None) => {
                options.run_bounds.min = min;
            }
            (None, Some(max)) => {
                // Since the minimum was not explicit we lower it if max is below the default min.
                options.run_bounds.min = cmp::min(options.run_bounds.min, max);
                options.run_bounds.max = Some(max);
            }
            (Some(min), Some(max)) if min > max => {
                return Err(OptionsError::EmptyRunsRange);
            }
            (Some(min), Some(max)) => {
                options.run_bounds.min = min;
                options.run_bounds.max = Some(max);
            }
            (None, None) => {}
        };

        if let Some(val) = matches.get_one::<String>("duration") {
            let budget = crate::util::units::parse_duration(val)
                .map_err(|e| OptionsError::InvalidDuration(val.clone(), e))?;
            options.duration = Some(budget.as_secs_f64());
            // 3 runs are enough for a mean and a standard deviation; the
            // default minimum of 10 would defeat short budgets.
            if min_runs.is_none() {
                options.run_bounds.min = options.run_bounds.max.map_or(3, |max| max.min(3));
            }
        }

        options.setup_command = matches
            .get_many::<String>("setup")
            .map(|values| values.map(String::from).collect::<Vec<String>>());

        options.reference_command = matches.get_one::<String>("reference").map(String::from);
        options.reference_name = matches
            .get_one::<String>("reference-name")
            .map(String::from);

        options.preparation_command = matches
            .get_many::<String>("prepare")
            .map(|values| values.map(String::from).collect::<Vec<String>>());

        options.conclusion_command = matches
            .get_many::<String>("conclude")
            .map(|values| values.map(String::from).collect::<Vec<String>>());

        options.cleanup_command = matches
            .get_many::<String>("cleanup")
            .map(|values| values.map(String::from).collect::<Vec<String>>());

        options.command_output_policies = if matches.get_flag("show-output") {
            vec![CommandOutputPolicy::Inherit]
        } else if matches.get_flag("show-output-on-failure") {
            vec![CommandOutputPolicy::CaptureTail]
        } else if let Some(output_values) = matches.get_many::<String>("output") {
            let mut policies = vec![];
            for value in output_values {
                let policy = match value.as_str() {
                    "null" => CommandOutputPolicy::Null,
                    "pipe" => CommandOutputPolicy::Pipe,
                    "inherit" => CommandOutputPolicy::Inherit,
                    arg => {
                        let path = PathBuf::from(arg);
                        if path.components().count() <= 1 {
                            return Err(OptionsError::UnknownOutputPolicy(arg.to_string()));
                        }
                        CommandOutputPolicy::File(path)
                    }
                };
                policies.push(policy);
            }
            policies
        } else {
            vec![CommandOutputPolicy::Null]
        };

        options.output_style = match matches.get_one::<String>("style").map(|s| s.as_str()) {
            Some("full") => OutputStyleOption::Full,
            Some("basic") => OutputStyleOption::Basic,
            Some("nocolor") => OutputStyleOption::NoColor,
            Some("color") => OutputStyleOption::Color,
            Some("none") => OutputStyleOption::Disabled,
            _ => {
                if options
                    .command_output_policies
                    .contains(&CommandOutputPolicy::Inherit)
                    || !io::stdout().is_terminal()
                {
                    OutputStyleOption::Basic
                } else if env::var_os("TERM")
                    .map(|t| t == "unknown" || t == "dumb")
                    .unwrap_or(!cfg!(target_os = "windows"))
                    || env::var_os("NO_COLOR")
                        .map(|t| !t.is_empty())
                        .unwrap_or(false)
                {
                    OutputStyleOption::NoColor
                } else {
                    OutputStyleOption::Full
                }
            }
        };

        match options.output_style {
            OutputStyleOption::Basic | OutputStyleOption::NoColor => {
                colored::control::set_override(false)
            }
            OutputStyleOption::Full | OutputStyleOption::Color => {
                colored::control::set_override(true)
            }
            OutputStyleOption::Disabled => {}
        };

        (
            options.sort_order_speed_comparison,
            options.sort_order_exports,
        ) = match matches.get_one::<String>("sort").map(|s| s.as_str()) {
            None | Some("auto") => (SortOrder::MeanTime, SortOrder::Command),
            Some("command") => (SortOrder::Command, SortOrder::Command),
            Some("mean-time") => (SortOrder::MeanTime, SortOrder::MeanTime),
            // clap only accepts the values above; treat anything else like "auto"
            Some(_) => (SortOrder::MeanTime, SortOrder::Command),
        };

        // A command given after `--` is an argument vector and always runs
        // without a shell (`--shell` conflicts with it at the CLI level).
        let executor_kind = |shell: Option<&String>| -> Result<ExecutorKind, OptionsError> {
            Ok(match (matches.get_flag("debug-mode"), shell) {
                (false, Some(shell)) if shell == "default" => ExecutorKind::Shell(Shell::default()),
                (false, Some(shell)) if shell == "none" => ExecutorKind::Raw,
                (false, Some(shell)) => ExecutorKind::Shell(Shell::parse_from_str(shell)?),
                (false, None) => ExecutorKind::Shell(Shell::default()),
                (true, Some(shell)) => ExecutorKind::Mock(Some(shell.into())),
                (true, None) => ExecutorKind::Mock(None),
            })
        };
        options.executor_kinds = if matches.get_flag("no-shell")
            || (matches.contains_id("argv") && !matches.get_flag("debug-mode"))
        {
            vec![ExecutorKind::Raw]
        } else {
            match matches.get_many::<String>("shell") {
                // '--shell' once for all commands, or once per command
                Some(shells) => shells
                    .map(|shell| executor_kind(Some(shell)))
                    .collect::<Result<_, _>>()?,
                None => vec![executor_kind(None)?],
            }
        };
        options.executor_kind = options.executor_kinds[0].clone();

        if let Some(mode) = matches.get_one::<String>("ignore-failure") {
            options.command_failure_action = match mode.as_str() {
                "all-non-zero" | "" => CmdFailureAction::IgnoreAllFailures,
                codes => {
                    let exit_codes: Result<Vec<i32>, _> = codes
                        .split(',')
                        .map(|s| {
                            s.trim()
                                .parse::<i32>()
                                .map_err(|e| OptionsError::IntParsingError("ignore-failure", e))
                        })
                        .collect();
                    CmdFailureAction::IgnoreSpecificFailures(exit_codes?)
                }
            };
        }

        options.time_unit = match matches.get_one::<String>("time-unit").map(|s| s.as_str()) {
            Some("microsecond") => Some(Unit::MicroSecond),
            Some("millisecond") => Some(Unit::MilliSecond),
            Some("second") => Some(Unit::Second),
            _ => None,
        };

        if let Some(time) = matches.get_one::<String>("min-benchmarking-time") {
            options.min_benchmarking_time = time
                .parse::<f64>()
                .map_err(|e| OptionsError::FloatParsingError("min-benchmarking-time", e))?;
        }
        if let Some(value) = matches.get_one::<String>("target-precision") {
            let pct = value
                .trim()
                .trim_end_matches('%')
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|pct| pct.is_finite() && *pct > 0.0)
                .ok_or_else(|| OptionsError::InvalidTargetPrecision(value.clone()))?;
            options.target_precision = Some(pct / 100.0);
        }
        if let Some(time) = matches.get_one::<String>("max-benchmarking-time") {
            options.max_benchmarking_time = time
                .parse::<f64>()
                .map_err(|e| OptionsError::FloatParsingError("max-benchmarking-time", e))?;
        }
        if let Some(val) = matches.get_one::<String>("timeout") {
            options.timeout = Some(
                crate::util::units::parse_duration(val)
                    .map_err(|e| OptionsError::InvalidTimeout(val.clone(), e))?,
            );
        }
        options.energy_domains = matches.get_flag("energy-domains");
        options.pareto = crate::pareto::ParetoConfig::parse(
            matches.get_one::<String>("pareto").map(String::as_str),
            matches
                .get_many::<String>("constraint")
                .into_iter()
                .flatten()
                .map(String::as_str),
            matches.get_one::<String>("objective").map(String::as_str),
            matches
                .get_one::<String>("noise-threshold")
                .map(String::as_str),
        )
        .map_err(OptionsError::InvalidPareto)?;
        if let Some(val) = matches.get_one::<String>("concurrency") {
            if val.contains('{') {
                // Resolved per parameter combination, once the commands are known
                options.concurrency_template = Some(val.clone());
            } else {
                options.concurrency = val
                    .parse::<usize>()
                    .ok()
                    .filter(|&n| n >= 1)
                    .ok_or_else(|| OptionsError::InvalidConcurrency(val.clone()))?;
            }
        }
        if let Some(val) = matches.get_one::<String>("energy-baseline") {
            options.energy_baseline = parse_energy_baseline(val)
                .map_err(|e| OptionsError::InvalidEnergyBaseline(val.clone(), e))?;
        }
        if let Some(val) = matches.get_one::<String>("until") {
            if val.is_empty() {
                return Err(OptionsError::EmptyUntilPattern);
            }
            options.until = Some(UntilSettings {
                pattern: val.as_bytes().to_vec(),
                match_stderr: matches.get_flag("until-stderr"),
            });
        }
        if let Some(metrics) = matches.get_many::<String>("output-metric") {
            for m in metrics {
                let parsed = m
                    .parse::<crate::output_metric::OutputMetric>()
                    .map_err(|e| OptionsError::InvalidOutputMetric(format!("{e:#}")))?;
                if options
                    .output_metrics
                    .iter()
                    .any(|existing| existing.name == parsed.name)
                {
                    return Err(OptionsError::InvalidOutputMetric(format!(
                        "duplicate metric name '{}'",
                        parsed.name
                    )));
                }
                options.output_metrics.push(parsed);
            }
        }

        options.output_metrics_stderr = matches.get_flag("output-metric-stderr");
        if !options.output_metrics.is_empty()
            && options.command_output_policies.iter().any(|p| {
                matches!(
                    p,
                    CommandOutputPolicy::File(_) | CommandOutputPolicy::Inherit
                )
            })
        {
            return Err(OptionsError::InvalidOutputMetric(
                "--output-metric cannot be combined with '--output=inherit' or an output file"
                    .into(),
            ));
        }

        options.command_input_policy = if let Some(path_str) = matches.get_one::<String>("input") {
            if path_str == "null" {
                CommandInputPolicy::Null
            } else {
                let path = PathBuf::from(path_str);
                if !path.exists() {
                    return Err(OptionsError::StdinDataFileDoesNotExist(
                        path_str.to_string(),
                    ));
                }
                CommandInputPolicy::File(path)
            }
        } else {
            CommandInputPolicy::Null
        };

        options.optimize = crate::optimize::OptimizeSpec::from_cli_arguments(matches)
            .map_err(|e| OptionsError::InvalidOptimize(format!("{e:#}")))?;
        options.measure_energy = matches.get_flag("energy");
        if let Some(metric_str) = matches.get_one::<String>("metric") {
            options.metric = metric_str
                .parse::<crate::metric::Metric>()
                .unwrap_or_default();
        }
        if options.metric == crate::metric::Metric::Energy {
            options.measure_energy = true;
        }
        // An imported file carries its own values, so nothing is measured then.
        let importing = matches.get_one::<String>("import-json").is_some();
        let metric_counter = options.metric.counter_kind();
        // An imported file carries its own values, so nothing is extracted then.
        if let Some(name) = options.metric.custom_name() {
            if matches.get_one::<String>("import-json").is_none()
                && !options.output_metrics.iter().any(|m| m.name == name)
            {
                return Err(OptionsError::InvalidOutputMetric(format!(
                    "'--metric custom:{name}' needs a matching '--output-metric {name}=<REGEX>'"
                )));
            }
        }
        // Only look at the backend choice (and its environment variable) when
        // energy is requested, so a bad value cannot break runs without --energy.
        if options.measure_energy {
            options.energy_backend = crate::energy::BackendPreference::resolve(
                matches
                    .get_one::<String>("energy-backend")
                    .map(String::as_str),
                std::env::var(crate::energy::ENERGY_BACKEND_ENV)
                    .ok()
                    .as_deref(),
            )
            .map_err(OptionsError::InvalidEnergyBackend)?;
        }
        options.deep_stats = matches.get_flag("deep-stats");
        options.filter_failed = matches.get_flag("filter-failed");
        options.suppress_outlier_warnings = matches.get_flag("suppress-outlier-warnings");
        options.suppress_off_cpu_warnings = matches.get_flag("no-off-cpu-warning");
        if let Some(method) = matches.get_one::<String>("outlier-method") {
            options.outlier_method = match method.as_str() {
                "iqr" => crate::outlier_detection::OutlierMethod::Iqr,
                _ => crate::outlier_detection::OutlierMethod::ModifiedZ,
            };
        }

        if let Some(suppress_list) = matches.get_many::<String>("suppress-warnings") {
            for kind in suppress_list {
                match kind.as_str() {
                    "off-cpu" => options.suppress_off_cpu_warnings = true,
                    "outliers" => options.suppress_outlier_warnings = true,
                    "all" => {
                        options.suppress_off_cpu_warnings = true;
                        options.suppress_outlier_warnings = true;
                    }
                    _ => {}
                }
            }
        }
        options.omit_failed_runs = matches.get_flag("omit-failed-runs");
        options.validation = parse_validation(matches)?;
        options.quality_mode = matches
            .get_one::<String>("quality")
            .and_then(|value| crate::quality::QualityMode::parse(value))
            .unwrap_or_default();
        options.fail_on_quality = matches
            .get_one::<String>("fail-on-quality")
            .and_then(|value| crate::quality::Grade::parse(value));
        options.show_resource_usage = matches.get_flag("resource-usage");
        let simd = matches.get_flag("simd");
        // `--metric instructions|cycles` collects its own counter
        let counters_list = matches.get_one::<String>("counters").cloned().or_else(|| {
            (!importing)
                .then_some(metric_counter)
                .flatten()
                .map(|kind| kind.name().to_string())
        });
        if counters_list.is_some() || simd {
            let mode = matches
                .get_one::<String>("scope")
                .and_then(|v| crate::perf_counters::ScopeMode::parse(v))
                .unwrap_or_default();
            if simd && mode == crate::perf_counters::ScopeMode::Split {
                return Err(OptionsError::InvalidCounters(
                    "'--simd' cannot be combined with '--scope split': its four events are one \
                     unit and count in a single scope"
                        .to_string(),
                ));
            }
            let mut requested = match counters_list.as_deref() {
                Some(list) => crate::perf_counters::parse_list(list, mode)
                    .map_err(OptionsError::InvalidCounters)?,
                None => Vec::new(),
            };
            if let Some(kind) = metric_counter.filter(|_| !importing) {
                let spec = crate::perf_counters::CounterSpec {
                    kind,
                    scope: mode.default_scope(),
                };
                if !requested.contains(&spec) {
                    requested.push(spec);
                }
            }
            let (config, warnings) = crate::perf_counters::resolve_with_simd(
                requested,
                mode.default_scope(),
                simd,
                matches.get_flag("debug-mode"),
            )
            .map_err(OptionsError::InvalidCounters)?;
            for warning in warnings {
                eprintln!("{} {warning}", crate::output::colors::yellow("Warning:"));
            }
            options.counters = Some(config);
        }
        if let Some(value) = matches.get_one::<String>("affinity") {
            if let Some(class) = crate::util::affinity::CoreClass::parse(value) {
                crate::util::affinity::validate_core_class(
                    class,
                    crate::util::affinity::perf_levels(),
                )
                .map_err(|e| OptionsError::InvalidAffinity(format!("{e:#}")))?;
                options.core_class = Some(class);
            } else {
                let cpus = crate::util::affinity::parse_cpu_list(value)
                    .and_then(|cpus| crate::util::affinity::validate(&cpus).map(|_| cpus))
                    .map_err(|e| OptionsError::InvalidAffinity(format!("{e:#}")))?;
                options.affinity = Some(cpus);
            }
        }
        if matches.get_flag("topdown") {
            let debug_mode = matches.get_flag("debug-mode");
            if !debug_mode && !crate::topdown::platform_supported() {
                return Err(OptionsError::InvalidTopdown(
                    "--topdown is only supported on Linux x86_64".to_string(),
                ));
            }
            // On hybrid CPUs the events only count on one core type
            let pinned = options.affinity.is_some() || options.core_class.is_some();
            let (config, warnings) = crate::topdown::resolve(debug_mode, pinned);
            for warning in warnings {
                eprintln!("{} {warning}", crate::output::colors::yellow("Warning:"));
            }
            options.topdown = Some(config);
        }
        options.subtract_command = matches.get_one::<String>("subtract").cloned();
        if let Some(priority) = matches.get_one::<String>("priority") {
            options.priority = crate::util::priority::Priority::parse(priority)
                .map_err(|e| OptionsError::InvalidPriority(format!("{e:#}")))?;
        }
        options.discard_outliers = matches
            .get_one::<String>("discard-outliers")
            .map(|value| match value.as_str() {
                "default" => Ok(crate::outlier_detection::OUTLIER_THRESHOLD),
                other => other
                    .parse::<f64>()
                    .ok()
                    .filter(|z| z.is_finite() && *z > 0.0)
                    .ok_or(OptionsError::InvalidOutlierThreshold(other.to_string())),
            })
            .transpose()?;

        if options.omit_failed_runs
            && options.command_failure_action == CmdFailureAction::RaiseError
        {
            return Err(OptionsError::OmitFailedRunsRequiresIgnoreFailure);
        }

        options.schedule = if matches.get_flag("round-robin") {
            ScheduleMode::RoundRobin
        } else {
            match matches.get_one::<String>("schedule").map(|s| s.as_str()) {
                Some("round-robin") | Some("interleaved") => ScheduleMode::RoundRobin,
                _ => ScheduleMode::Grouped,
            }
        };

        options.allow_setup_with_round_robin = matches.get_flag("allow-setup-with-round-robin");

        options.warn_concurrency();

        Ok(options)
    }

    #[cfg(feature = "cli")]
    /// Warnings for `--concurrency N` with N > 1 (printed once, to stderr).
    fn warn_concurrency(&self) {
        if self.concurrency <= 1 {
            return;
        }
        let warning = crate::output::colors::yellow("Warning:");
        let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
        if self.concurrency > cpus {
            eprintln!(
                "{warning} '--concurrency {}' exceeds the {cpus} logical CPUs: instances will \
                 queue on the CPU and the times include that waiting.",
                self.concurrency
            );
        }
        if self
            .command_output_policies
            .iter()
            .any(|p| matches!(p, CommandOutputPolicy::Inherit))
        {
            eprintln!(
                "{warning} with '--concurrency {}' the output of the instances interleaves.",
                self.concurrency
            );
        }
    }

    pub fn validate_against_command_list(&mut self, commands: &Commands) -> Result<()> {
        let has_reference_command = self.reference_command.is_some();
        let num_commands = commands.num_commands(has_reference_command);

        if num_commands == 0 {
            return Ok(());
        }

        if let Some(template) = &self.concurrency_template {
            // Fail before anything runs when a value is not an integer >= 1
            let mut largest = 1;
            for command in commands.iter() {
                let resolved = command.clone().with_concurrency_template(template)?;
                largest = largest.max(resolved.concurrency_or(1));
            }
            let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
            if largest > cpus {
                eprintln!(
                    "{} '--concurrency {template}' reaches {largest} instances, more than the \
                     {cpus} logical CPUs: those instances will queue on the CPU.",
                    crate::output::colors::yellow("Warning:")
                );
            }
            if self.reference_command.is_some() {
                bail!(
                    "'--concurrency {template}' cannot be combined with '--reference': the \
                     reference command has no parameters to substitute"
                );
            }
        }

        if let Some(spec) = &self.optimize {
            if let Some(command) = commands.iter().find(|c| !c.uses_parameter(&spec.var)) {
                bail!(
                    "'--optimize {var}': the command '{command}' does not contain '{{{var}}}'",
                    var = spec.var,
                    command = command.get_command_line()
                );
            }
        }

        if self.schedule == ScheduleMode::RoundRobin && !self.allow_setup_with_round_robin {
            // Setup/cleanup differ between benchmarks if they contain a parameter
            // placeholder, or if they are given per command with different values.
            let has_param = |values: &Option<Vec<String>>| {
                values.iter().flatten().any(|value| {
                    commands.iter().any(|c| {
                        c.get_parameters()
                            .iter()
                            .any(|(n, _)| value.contains(&format!("{{{n}}}")))
                    })
                })
            };
            let per_command_values_differ = |values: &Option<Vec<String>>| {
                values
                    .as_ref()
                    .is_some_and(|values| values.iter().any(|v| v != &values[0]))
            };
            let reason = if has_param(&self.setup_command) || has_param(&self.cleanup_command) {
                Some("due to parameter substitution")
            } else if per_command_values_differ(&self.setup_command)
                || per_command_values_differ(&self.cleanup_command)
            {
                Some("different values were given per command")
            } else {
                None
            };
            if let Some(reason) = reason {
                bail!(
                    "The '--setup' and/or '--cleanup' options differ between benchmarks ({reason}) \
                     and cannot be combined with '--schedule round-robin'. \
                     Setup runs once per benchmark batch, but round-robin interleaves runs across benchmarks. \
                     Use '--prepare' for per-run state, or use the default grouped schedule."
                );
            }
        }

        ensure!(
            self.executor_kinds.len() <= 1 || self.executor_kinds.len() == num_commands,
            "The '--shell' option has to be provided just once or N times, where N={num_commands} is \
             the number of benchmark commands (including a potential reference)."
        );

        if let Some(setup_command) = &self.setup_command {
            ensure!(
                setup_command.len() <= 1 || num_commands == setup_command.len(),
                "The '--setup' option has to be provided just once or N times, where N={num_commands} is the \
                 number of benchmark commands (including a potential reference)."
            );
        }

        if let Some(cleanup_command) = &self.cleanup_command {
            ensure!(
                cleanup_command.len() <= 1 || num_commands == cleanup_command.len(),
                "The '--cleanup' option has to be provided just once or N times, where N={num_commands} is the \
                 number of benchmark commands (including a potential reference)."
            );
        }

        if let Some(preparation_command) = &self.preparation_command {
            ensure!(
                preparation_command.len() <= 1 || num_commands == preparation_command.len(),
                "The '--prepare' option has to be provided just once or N times, where N={num_commands} is the \
                 number of benchmark commands (including a potential reference)."
            );
        }

        if let Some(conclusion_command) = &self.conclusion_command {
            ensure!(
                conclusion_command.len() <= 1 || num_commands == conclusion_command.len(),
                "The '--conclude' option has to be provided just once or N times, where N={num_commands} is the \
                 number of benchmark commands (including a potential reference)."
            );
        }

        if self.command_output_policies.len() == 1 {
            self.command_output_policies =
                vec![self.command_output_policies[0].clone(); num_commands];
        } else {
            ensure!(
                self.command_output_policies.len() == num_commands,
                "The '--output' option has to be provided just once or N times, where N={num_commands} is the \
                 number of benchmark commands (including a potential reference)."
            );
        }

        if self.metric == crate::metric::Metric::Energy
            && !matches!(self.executor_kind, ExecutorKind::Mock(_))
            && !crate::energy::get_energy_sampler(self.energy_backend).is_available()
        {
            bail!(
                "Energy measurement is unavailable or unprivileged on this platform. Cannot use '--metric energy'."
            );
        }

        Ok(())
    }
}

#[test]
fn test_default_shell() {
    let shell = Shell::default();

    let s = format!("{shell}");
    assert_eq!(&s, DEFAULT_SHELL);

    let cmd = shell.command();
    assert_eq!(cmd.get_program(), DEFAULT_SHELL);
}

#[test]
fn test_can_parse_shell_command_line_from_str() {
    let shell = Shell::parse_from_str("shell -x 'aaa bbb'").unwrap();

    let s = format!("{shell}");
    assert_eq!(&s, "shell -x 'aaa bbb'");

    let cmd = shell.command();
    assert_eq!(cmd.get_program().to_string_lossy(), "shell");
    assert_eq!(
        cmd.get_args()
            .map(|a| a.to_string_lossy())
            .collect::<Vec<_>>(),
        vec!["-x", "aaa bbb"]
    );

    // Error cases
    assert!(matches!(
        Shell::parse_from_str("shell 'foo").unwrap_err(),
        OptionsError::ShellParseError(_)
    ));

    assert!(matches!(
        Shell::parse_from_str("").unwrap_err(),
        OptionsError::EmptyShell
    ));

    assert!(matches!(
        Shell::parse_from_str("''").unwrap_err(),
        OptionsError::EmptyShell
    ));
}

#[cfg(feature = "cli")]
#[test]
fn test_suppress_outlier_warnings_option() {
    let matches = crate::cli::build_command().get_matches_from(vec![
        "perfratio",
        "--suppress-outlier-warnings",
        "echo test",
    ]);
    let options = Options::from_cli_arguments(&matches).unwrap();
    assert!(options.suppress_outlier_warnings);

    let matches_default =
        crate::cli::build_command().get_matches_from(vec!["perfratio", "echo test"]);
    let options_default = Options::from_cli_arguments(&matches_default).unwrap();
    assert!(!options_default.suppress_outlier_warnings);
}

#[cfg(feature = "cli")]
#[test]
fn test_schedule_options() {
    let matches = crate::cli::build_command().get_matches_from(vec![
        "perfratio",
        "--schedule=round-robin",
        "echo test",
    ]);
    let options = Options::from_cli_arguments(&matches).unwrap();
    assert_eq!(options.schedule, ScheduleMode::RoundRobin);

    let matches_interleaved = crate::cli::build_command().get_matches_from(vec![
        "perfratio",
        "--schedule=interleaved",
        "echo test",
    ]);
    let options_interleaved = Options::from_cli_arguments(&matches_interleaved).unwrap();
    assert_eq!(options_interleaved.schedule, ScheduleMode::RoundRobin);

    assert!(crate::cli::build_command()
        .try_get_matches_from(vec!["perfratio", "--schedule=sequential", "echo test"])
        .is_err());

    let matches_flag = crate::cli::build_command().get_matches_from(vec![
        "perfratio",
        "--round-robin",
        "echo test",
    ]);
    let options_flag = Options::from_cli_arguments(&matches_flag).unwrap();
    assert_eq!(options_flag.schedule, ScheduleMode::RoundRobin);

    let matches_default =
        crate::cli::build_command().get_matches_from(vec!["perfratio", "echo test"]);
    let options_default = Options::from_cli_arguments(&matches_default).unwrap();
    assert_eq!(options_default.schedule, ScheduleMode::Grouped);
}

#[cfg(feature = "cli")]
#[test]
fn test_warning_suppression_options() {
    let matches_no_off_cpu = crate::cli::build_command().get_matches_from(vec![
        "perfratio",
        "--no-off-cpu-warning",
        "echo test",
    ]);
    let opts = Options::from_cli_arguments(&matches_no_off_cpu).unwrap();
    assert!(opts.suppress_off_cpu_warnings);
    assert!(!opts.suppress_outlier_warnings);

    let matches_outliers = crate::cli::build_command().get_matches_from(vec![
        "perfratio",
        "--suppress-outlier-warnings",
        "echo test",
    ]);
    let opts = Options::from_cli_arguments(&matches_outliers).unwrap();
    assert!(!opts.suppress_off_cpu_warnings);
    assert!(opts.suppress_outlier_warnings);

    let matches_suppress_both = crate::cli::build_command().get_matches_from(vec![
        "perfratio",
        "--suppress-warnings=off-cpu,outliers",
        "echo test",
    ]);
    let opts = Options::from_cli_arguments(&matches_suppress_both).unwrap();
    assert!(opts.suppress_off_cpu_warnings);
    assert!(opts.suppress_outlier_warnings);

    let matches_suppress_all = crate::cli::build_command().get_matches_from(vec![
        "perfratio",
        "--no-warning=all",
        "echo test",
    ]);
    let opts = Options::from_cli_arguments(&matches_suppress_all).unwrap();
    assert!(opts.suppress_off_cpu_warnings);
    assert!(opts.suppress_outlier_warnings);
}

#[cfg(feature = "cli")]
/// `--energy-baseline` value: `None` for an explicit zero ("0", "0s", "0ms"), which disables
/// the baseline; otherwise a positive duration. `parse_duration` rejects zero, so zero is
/// recognised here, after checking that the unit is valid.
fn parse_energy_baseline(val: &str) -> Result<Option<std::time::Duration>, String> {
    let t = val.trim();
    let split = t
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    if num.parse::<f64>().is_ok_and(|n| n == 0.0) {
        crate::util::units::parse_duration(&format!("1{unit}"))?;
        return Ok(None);
    }
    crate::util::units::parse_duration(t).map(Some)
}

#[cfg(all(test, feature = "cli"))]
mod energy_baseline_tests {
    use super::parse_energy_baseline;
    use std::time::Duration;

    #[test]
    fn zero_disables_the_baseline() {
        for v in ["0", "0s", "0.0", "0ms", " 0 "] {
            assert_eq!(parse_energy_baseline(v), Ok(None), "{v}");
        }
    }

    #[test]
    fn positive_durations_parse() {
        assert_eq!(
            parse_energy_baseline("2s"),
            Ok(Some(Duration::from_secs(2)))
        );
        assert_eq!(
            parse_energy_baseline("500ms"),
            Ok(Some(Duration::from_millis(500)))
        );
    }

    #[test]
    fn invalid_values_are_rejected() {
        for v in ["-1s", "", "abc", "0parsecs", "2parsecs"] {
            assert!(parse_energy_baseline(v).is_err(), "{v}");
        }
    }
}

/// `--expect-output-sha256`, `--validate`, `--validate-first-only`, `--on-invalid`
#[cfg(feature = "cli")]
fn parse_validation<'a>(
    matches: &ArgMatches,
) -> Result<Option<crate::validation::ValidationConfig>, OptionsError<'a>> {
    use crate::validation::{parse_sha256, OnInvalid, ValidationConfig};
    let expected_sha256 = matches
        .get_one::<String>("expect-output-sha256")
        .map(|value| {
            parse_sha256(value).ok_or_else(|| {
                OptionsError::InvalidValidation(format!(
                "'--expect-output-sha256 {value}' is not a SHA-256: expected 64 hexadecimal digits"
            ))
            })
        })
        .transpose()?;
    let validator = matches.get_one::<String>("validate").cloned();
    let first_only = matches.get_flag("validate-first-only");
    let on_invalid = matches
        .get_one::<String>("on-invalid")
        .and_then(|value| OnInvalid::parse(value));
    if expected_sha256.is_none() && validator.is_none() {
        if first_only || on_invalid.is_some() {
            return Err(OptionsError::InvalidValidation(
                "'--validate-first-only' and '--on-invalid' need '--expect-output-sha256' or \
                 '--validate'"
                    .into(),
            ));
        }
        return Ok(None);
    }
    Ok(Some(ValidationConfig {
        expected_sha256,
        validator,
        first_only,
        on_invalid: on_invalid.unwrap_or_default(),
    }))
}
