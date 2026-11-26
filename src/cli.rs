use std::ffi::OsString;

use clap::{
    builder::{
        styling::{AnsiColor, Effects},
        NonEmptyStringValueParser, PossibleValuesParser, Styles,
    },
    crate_version, Arg, ArgAction, ArgGroup, ArgMatches, Command, ValueHint,
};

pub const SHELLS: [&str; 5] = ["bash", "zsh", "fish", "powershell", "elvish"];

/// Appended to the clap-generated fish completions: complete executables from
/// `$PATH` for the benchmarked command (the first positional argument), which
/// clap cannot express (port of hyperfine#896, see hyperfine#872).
pub const FISH_COMMAND_COMPLETION: &str = "
# Complete executables from $PATH for the benchmarked command (hyperfine#872)
complete -c perfratio -n \"__fish_use_subcommand\" -x -a \"(__fish_complete_command)\"
";

pub fn get_cli_arguments<'a, I, T>(args: I) -> ArgMatches
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone + 'a,
{
    let command = build_command();
    command.get_matches_from(args)
}

/// Colors used by `--help` and usage errors. clap disables them automatically
/// when stdout is not a terminal or when `NO_COLOR` is set.
const STYLES: Styles = Styles::styled()
    .header(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .usage(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .literal(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
    .placeholder(AnsiColor::Cyan.on_default())
    .error(AnsiColor::Red.on_default().effects(Effects::BOLD))
    .valid(AnsiColor::Green.on_default())
    .invalid(AnsiColor::Yellow.on_default());

/// Build the clap command for parsing command line arguments
pub fn build_command() -> Command {
    Command::new("perfratio")
        .version(crate_version!())
        .styles(STYLES)
        .next_line_help(true)
        .hide_possible_values(true)
        // `--scope` applies to both of these
        .group(ArgGroup::new("pmu-request").args(["counters", "simd"]).multiple(true))
        .about("A multi-dimensional command-line benchmarking tool with Performance per Watt, Hardware Counter Ratios, and Deep Statistics.")
        .help_expected(true)
        .max_term_width(80)
        .arg(
            Arg::new("command")
                .help("The command to benchmark. This can be the name of an executable, a command \
                       line like \"grep -i todo\" or a shell command like \"sleep 0.5 && echo test\". \
                       The latter is only available if the shell is not explicitly disabled via \
                       '--shell=none'. If multiple commands are given, perfratio will show a \
                       comparison of the respective runtimes. A single '-' reads one command per line from \
                       stdin (blank lines and lines starting with '#' are ignored).")
                .required_unless_present_any(["generate-completions", "import-json", "argv"])
                .action(ArgAction::Append)
                .value_hint(ValueHint::CommandString)
                .value_parser(NonEmptyStringValueParser::new()),
        )
        .arg(
            Arg::new("argv")
                .last(true)
                .num_args(1..)
                .allow_hyphen_values(true)
                .action(ArgAction::Set)
                .value_name("PROGRAM [ARGS]")
                .value_hint(ValueHint::CommandWithArguments)
                .conflicts_with_all(["command", "shell"])
                .help("A single command to benchmark, given as an argument vector after '--'. \
                       It is executed exactly as given: without a shell (like '--shell=none') \
                       and without re-splitting or unquoting, so no extra quoting is needed. \
                       Parameters ('{name}') are substituted in each argument separately.\n\n\
                       Example:  perfratio -w 3 -- grep -E 'a|b' \"my file.txt\"\n"),
        )
        .arg(
            Arg::new("import-json")
                .long("import-json")
                .action(ArgAction::Append)
                .value_name("FILE")
                .help("Import timing results from a JSON file previously exported by perfratio (or hyperfine, for the shared result format). \
                       Multiple files can be specified. Can be combined with commands to benchmark or used \
                       on its own to re-export existing results to another format.\n\n\
                       Example:  perfratio --import-json baseline.json 'sleep 1'\n"),
        )
        .arg(
            Arg::new("parameter-sample")
                .long("parameter-sample")
                .action(ArgAction::Append)
                .num_args(2)
                .value_names(["VAR", "VALUES"])
                .help("Draw a value of the parameter VAR at random for every run, from the \
                       comma-separated VALUES (like '-L'), instead of creating one benchmark per \
                       value. Every command sees the same values in the same order (a paired \
                       comparison); '--seed' changes the sequence. Can be combined with '-L'/'-P' \
                       (which still create separate benchmarks).\n\n\
                       Example:  perfratio --parameter-sample file a.txt,b.txt,c.txt 'wc -l {file}' \
                       'grep -c . {file}'\n"),
        )
        .arg(
            Arg::new("aggregate-parameter-runs")
                .long("aggregate-parameter-runs")
                .action(ArgAction::SetTrue)
                .help("Pool all values of the '-L'/'-P'/'--parameter-file' parameters into one \
                       benchmark per command template: its runs go through all parameter \
                       combinations in turn, and the number of runs is rounded up to whole \
                       cycles, so every value is used equally often. Useful when each value is \
                       one sample of the same workload (e.g. 20 different input files)."),
        )
        .arg(
            Arg::new("seed")
                .long("seed")
                .visible_alias("bootstrap-seed")
                .action(ArgAction::Set)
                .value_name("NUM")
                .value_parser(clap::value_parser!(u64))
                .help("Seed for '--parameter-sample' (default: 0) and for the bootstrap \
                       resampling of intervals and tests (default: a fixed seed). Defaults make \
                       runs reproducible; the same seed gives the same output."),
        )
        .arg(
            Arg::new("compare")
                .long("compare")
                .action(ArgAction::Set)
                .value_name("FILE")
                .value_hint(ValueHint::FilePath)
                .help("Compare each benchmark with the result of the same command (name) in a \
                       JSON file exported earlier, e.g. on the main branch, and print a table \
                       with the relative change and its statistical significance.\n\n\
                       Example:  perfratio --compare baseline.json --fail-if-regressed 5% 'make'\n"),
        )
        .arg(
            Arg::new("fail-if-regressed")
                .long("fail-if-regressed")
                .action(ArgAction::Set)
                .value_name("PCT")
                .requires("compare")
                .help("With '--compare': exit with code 3 if any benchmark is slower than its \
                       baseline by more than PCT (e.g. '5%') and the difference is statistically \
                       significant (p < 0.05, from the per-run times). Noise alone doesn't fail."),
        )
        .arg(
            Arg::new("export-diff-markdown")
                .long("export-diff-markdown")
                .action(ArgAction::Set)
                .value_name("FILE")
                .value_hint(ValueHint::FilePath)
                .requires("compare")
                .help("With '--compare': write the comparison as a Markdown table to FILE, \
                       e.g. for a pull request comment or a CI job summary."),
        )
        .arg(
            Arg::new("warmup")
                .long("warmup")
                .short('w')
                .value_name("NUM|auto")
                .action(ArgAction::Set)
                .help(
                    "Perform NUM warmup runs before the actual benchmark. This can be used \
                     to fill (disk) caches for I/O-heavy programs. With 'auto', warmup runs \
                     are performed until the last 5 timings differ by at most 1% \
                     ((max - min) / median), at most 100 runs.",
                ),
        )
        .arg(
            Arg::new("min-runs")
                .long("min-runs")
                .short('m')
                .action(ArgAction::Set)
                .value_name("NUM")
                .help("Perform at least NUM runs for each command (default: 10)."),
        )
        .arg(
            Arg::new("max-runs")
                .long("max-runs")
                .short('M')
                .action(ArgAction::Set)
                .value_name("NUM")
                .help("Perform at most NUM runs for each command. By default, there is no limit."),
        )
        .arg(
            Arg::new("runs")
                .long("runs")
                .conflicts_with_all(["max-runs", "min-runs"])
                .short('r')
                .action(ArgAction::Set)
                .value_name("NUM")
                .help("Perform exactly NUM runs for each command. If this option is not specified, \
                       perfratio automatically determines the number of runs."),
        )
        .arg(
            Arg::new("setup")
                .long("setup")
                .short('s')
                .action(ArgAction::Append)
                .num_args(1)
                .value_name("CMD")
                .value_hint(ValueHint::CommandString)
                .help(
                    "Execute CMD before each set of timing runs. This is useful for \
                     compiling your software with the provided parameters, or to do any \
                     other work that should happen once before a series of benchmark runs, \
                     not every time as would happen with the --prepare option.\n\
                     The --setup option can be specified once for all commands or multiple times, \
                     once for each command. In the latter case, each setup command will be \
                     run before the corresponding benchmark command."
                ),
        )
        .arg(
            Arg::new("reference")
                .long("reference")
                .action(ArgAction::Set)
                .value_name("CMD")
                .help(
                    "The reference command for the relative comparison of results. \
                    If this is unset, results are compared with the fastest command as reference."
                )
        )
        .arg(
            Arg::new("reference-name")
                .long("reference-name")
                .action(ArgAction::Set)
                .value_name("CMD")
                .help("Give a meaningful name to the reference command.")
                .requires("reference")
        )
        .arg(
            Arg::new("prepare")
                .long("prepare")
                .visible_alias("before")
                .short('p')
                .action(ArgAction::Append)
                .num_args(1)
                .value_name("CMD")
                .value_hint(ValueHint::CommandString)
                .help(
                    "Execute CMD before each timing run. This is useful for \
                     clearing disk caches, for example.\nThe --prepare option can \
                     be specified once for all commands or multiple times, once for \
                     each command. In the latter case, each preparation command will \
                     be run prior to the corresponding benchmark command.",
                ),
        )
        .arg(
            Arg::new("conclude")
                .long("conclude")
                .visible_alias("after")
                .short('C')
                .action(ArgAction::Append)
                .num_args(1)
                .value_name("CMD")
                .value_hint(ValueHint::CommandString)
                .help(
                    "Execute CMD after each timing run. This is useful for killing \
                     long-running processes started (e.g. a web server started in --prepare), \
                     for example.\nThe --conclude option can be specified once for all \
                     commands or multiple times, once for each command. In the latter case, \
                     each conclude command will be run after the corresponding benchmark \
                     command.",
                ),
        )
        .arg(
            Arg::new("cleanup")
                .long("cleanup")
                .short('c')
                .action(ArgAction::Append)
                .num_args(1)
                .value_name("CMD")
                .value_hint(ValueHint::CommandString)
                .help(
                    "Execute CMD after the completion of all benchmarking \
                     runs for each individual command to be benchmarked. \
                     This is useful if the commands to be benchmarked produce \
                     artifacts that need to be cleaned up.\n\
                     The --cleanup option can be specified once for all commands or multiple \
                     times, once for each command."
                ),
        )
        .arg(
            Arg::new("parameter-scan")
                .long("parameter-scan")
                .short('P')
                .action(ArgAction::Append)
                .allow_hyphen_values(true)
                .num_args(3)
                .value_names(["VAR", "MIN", "MAX"])
                .help(
                    "Perform benchmark runs for each value in the range MIN..MAX. Replaces the \
                     string '{VAR}' in each command by the current parameter value.\n\n  \
                     Example:  perfratio -P threads 1 8 'make -j {threads}'\n\n\
                     This performs benchmarks for 'make -j 1', 'make -j 2', …, 'make -j 8'.\n\n\
                     The option can be specified multiple times and combined with --parameter-list \
                     or --parameter-file to run benchmarks for all possible parameter combinations.\n\n\
                     To have the value increase following different patterns, use shell arithmetic.\n\n  \
                     Example: perfratio -P size 0 3 'sleep $((2**{size}))'\n\n\
                     This performs benchmarks with power of 2 increases: 'sleep 1', 'sleep 2', 'sleep 4', …\n\
                     The exact syntax may vary depending on your shell and OS."
                ),
        )
        .arg(
            Arg::new("optimize")
                .long("optimize")
                .action(ArgAction::Set)
                .allow_hyphen_values(true)
                .num_args(3)
                .value_names(["VAR", "MIN", "MAX"])
                .conflicts_with_all(["import-json", "reference"])
                .help(
                    "Search the parameter VAR in MIN..MAX (integers, or fractional values in steps of \
                     '-D DELTA') for the value that minimizes the \
                     primary metric (see '--metric'; default: mean wall time) instead of benchmarking \
                     every value. Replaces '{VAR}' in the command by the current value.\n\n  \
                     Example:  perfratio --optimize j 1 64 --metric energy 'make -j {j}'\n\n\
                     Uses a golden-section search and assumes that the metric is unimodal in VAR; \
                     perfratio warns if the evaluated points suggest otherwise. With very noisy \
                     metrics the search can miss the true minimum. Other parameters ('-P', '-L') \
                     are combined with the search: one search runs per combination.",
                ),
        )
        .arg(
            Arg::new("optimize-budget")
                .long("optimize-budget")
                .action(ArgAction::Set)
                .value_name("N")
                .value_parser(clap::value_parser!(usize))
                .default_value("12")
                .help(
                    "Maximum number of distinct parameter values '--optimize' evaluates (at least 3). \
                     A range that fits into the budget is evaluated completely.",
                ),
        )
        .arg(
            Arg::new("optimize-confirm")
                .long("optimize-confirm")
                .action(ArgAction::Set)
                .value_name("N")
                .value_parser(clap::value_parser!(usize))
                .default_value("2")
                .help(
                    "After the search, '--optimize' runs the best value and up to N other values \
                     whose difference from it is not significant a second time, and pools both \
                     runs before declaring the best value. 0 disables this. The extra runs are not \
                     separate results entries.",
                ),
        )
        .arg(
            Arg::new("parameter-step-size")
                .long("parameter-step-size")
                .short('D')
                .action(ArgAction::Set)
                .value_names(["DELTA"])
                .help(
                    "This argument requires --parameter-scan (or --optimize) to be specified as \
                     well. Traverse the range MIN..MAX in steps of DELTA. With '--optimize', the \
                     search runs over these values, which allows fractional parameters.\n\n  \
                     Example:  perfratio -P delay 0.3 0.7 -D 0.2 'sleep {delay}'\n\n\
                     This performs benchmarks for 'sleep 0.3', 'sleep 0.5' and 'sleep 0.7'.",
                ),
        )
        .arg(
            Arg::new("parameter-step-n")
                .long("parameter-step-n")
                .action(ArgAction::Set)
                .value_name("N")
                .requires("parameter-scan")
                .conflicts_with("parameter-step-size")
                .help(
                    "Scan every '--parameter-scan' range with N+1 evenly spaced values, \
                     including MIN and MAX, instead of a step size. Values are rounded to \
                     integers if MIN and MAX are integers (duplicates are removed, with a \
                     note).\n\n  \
                     Example:  perfratio -P n 0 100 --parameter-step-n 4 'sort {n}'\n\n\
                     This performs benchmarks for 0, 25, 50, 75 and 100.",
                ),
        )
        .arg(
            Arg::new("parameter-step-log")
                .long("parameter-step-log")
                .action(ArgAction::SetTrue)
                .requires("parameter-step-n")
                .help(
                    "With '--parameter-step-n': space the values geometrically (constant \
                     ratio). MIN must be greater than 0.\n\n  \
                     Example:  perfratio -P n 10 1000000 --parameter-step-n 5 \
                     --parameter-step-log 'sort {n}'\n\n\
                     This performs benchmarks for 10, 100, 1000, 10000, 100000 and 1000000.",
                ),
        )
        .arg(
            Arg::new("parameter-list")
                .long("parameter-list")
                .short('L')
                .action(ArgAction::Append)
                .allow_hyphen_values(true)
                .num_args(2)
                .value_names(["VAR", "VALUES"])
                .help(
                    "Perform benchmark runs for each value in the comma-separated list VALUES. \
                     Replaces the string '{VAR}' in each command by the current parameter value\
                     .\n\nExample:  perfratio -L compiler gcc,clang '{compiler} -O2 main.cpp'\n\n\
                     This performs benchmarks for 'gcc -O2 main.cpp' and 'clang -O2 main.cpp'.\n\n\
                     The option can be specified multiple times and combined with --parameter-scan \
                     or --parameter-file to run benchmarks for all possible parameter combinations.\n\n\
                     Note: values are inserted verbatim and are not shell-escaped. Use \
                     '--shell=none' for values from untrusted sources.\n"
                ),
        )
        .arg(
            Arg::new("parameter-file")
                .long("parameter-file")
                .short('F')
                .action(ArgAction::Append)
                .num_args(2)
                .value_names(["VAR", "FILE"])
                .help(
                    "Perform benchmark runs for each line in FILE. \
                     Replaces the string '{VAR}' in each command by the current parameter value.\n\n\
                     Example:  perfratio -F url urls.txt 'curl {url}'\n\n\
                     The option can be specified multiple times and combined with --parameter-scan \
                     or --parameter-list to run benchmarks for all possible parameter combinations.\n\n\
                     Note: lines are inserted verbatim and are not shell-escaped. Use \
                     '--shell=none' for files from untrusted sources.\n"
                ),
        )
        .arg(
            Arg::new("max-benchmarks")
                .long("max-benchmarks")
                .action(ArgAction::Set)
                .value_name("NUM")
                .value_parser(clap::value_parser!(usize))
                .help("Maximum allowed total number of benchmark combinations when using parameters (default: 100000)."),
        )
        .arg(
            Arg::new("expand-used-parameters")
                .long("expand-used-parameters")
                .action(ArgAction::SetTrue)
                .help("Only combine each command with the parameters it actually uses, instead of \
                       with all parameters. A parameter is used if '{NAME}' appears in the command, \
                       its --command-name, or any --prepare/--conclude/--setup/--cleanup command. \
                       Example: with '-L a 1,2 -L b x,y', the command 'foo' is benchmarked once \
                       instead of 4 times, and 'bar {a}' twice."),
        )
        .arg(
            Arg::new("shell")
                .long("shell")
                .short('S')
                .action(ArgAction::Append)
                .value_name("SHELL")
                .value_hint(ValueHint::CommandString)
                .help("Set the shell to use for executing benchmarked commands. This can be the \
                       name or the path to the shell executable, or a full command line \
                       like \"bash --norc\". It can also be set to \"default\" to explicitly select \
                       the default shell on this platform. Finally, this can also be set to \
                       \"none\" to disable the shell. In this case, commands will be executed \
                       directly. They can still have arguments, but more complex things like \
                       \"sleep 0.1; sleep 0.2\" are not possible without a shell. \
                       Arguments are split with POSIX quoting rules, except on Windows, where \
                       the C runtime rules apply (double quotes only; backslashes in paths \
                       like C:\\tools\\app.exe are kept). \
                       Give '--shell' once for all commands, or once per command (in order, \
                       including a '--reference' first) to compare shells or to mix shell and \
                       \"none\", e.g. '-S zsh CMD1 -S bash CMD2 -S none CMD3' (the order of \
                       the options is what counts).")
        )
        .arg(
            Arg::new("no-shell")
                .short('N')
                .action(ArgAction::SetTrue)
                .conflicts_with_all(["shell", "debug-mode"])
                .help("An alias for '--shell=none'.")
        )
        .arg(
            Arg::new("ignore-failure")
                .long("ignore-failure")
                .action(ArgAction::Set)
                .value_name("MODE")
                .num_args(0..=1)
                .default_missing_value("all-non-zero")
                .require_equals(true)
                .short('i')
                .visible_alias("ignore-exit-code")
                .help("Ignore non-zero exit codes of the benchmarked programs. Without a value or with \
                       'all-non-zero', all non-zero exit codes are ignored. You can also provide \
                       a comma-separated list of exit codes to ignore (e.g., --ignore-exit-code=1,2). \
                       '--ignore-exit-code' is an alias for '--ignore-failure'."),
        )
        .arg(
            Arg::new("filter-failed")
                .long("filter-failed")
                .action(ArgAction::SetTrue)
                .help("Exclude results with non-zero exit codes from the relative speed \
                       comparison and from exported results. This is useful when using \
                       '--ignore-failure' with parameter scans where some combinations fail. \
                       Note: when combined with '--omit-failed-runs', benchmarks with partially \
                       failed runs are retained because failed runs were already omitted."),
        )
        .arg(
            Arg::new("omit-failed-runs")
                .long("omit-failed-runs")
                .action(ArgAction::SetTrue)
                .requires("ignore-failure")
                .help("Exclude failed runs from summary statistics while still reporting how many failed.")
                .long_help(
                    "Exclude failed runs from summary statistics while still reporting how many failed.\n\n\
                     Requires '--ignore-failure' so that benchmark runs with non-zero exit codes \
                     can complete. Failed runs are omitted from mean, median, standard deviation, \
                     and exported timing data (and recorded in 'omitted_failed_runs').\n\n\
                     When combined with '--filter-failed', the benchmark is kept as long as at least \
                     one run succeeded.\n\n\
                     Example:\n\n  \
                       perfratio --ignore-failure --omit-failed-runs --runs 20 './flaky-test.sh'",
                ),
        )
        .arg(
            Arg::new("quality")
                .long("quality")
                .action(ArgAction::Set)
                .value_name("MODE")
                .value_parser(["off", "summary", "full"])
                .help("Print the measurement quality report: off, summary or full.")
                .long_help(
                    "Print the measurement quality report of each benchmark.\n\n  \
                     off      Print nothing (the JSON export always has the report)\n  \
                     summary  The grade and the checks that are not 'pass'\n  \
                     full     Every check\n\n\
                     Without the option, the summary is printed only when some check is 'warn', \
                     'fail' or 'unavailable'. The grade is 'unreliable' if any check fails, \
                     'with_caveats' if any warns, else 'comparable'.",
                ),
        )
        .arg(
            Arg::new("calibrate")
                .long("calibrate")
                .action(ArgAction::Set)
                .value_name("MODE")
                .num_args(0..=1)
                .require_equals(true)
                .default_missing_value("auto")
                .value_parser(["auto", "off"])
                .help("Measure the runner floor with a trivial command: auto (default) or off.")
                .long_help(
                    "Measure the runner floor before benchmarking: the wall time and peak memory \
                     of a trivial command ('true', or 'exit 0' through a shell) started the same \
                     way as the benchmarks (shell, affinity, priority, output policy). About 20 \
                     runs, at most about 0.2 s, once per session and shell.\n\n  \
                     auto  Measure the floor (default)\n  \
                     off   Do not measure it\n\n\
                     A command is 'below resolution' when its mean is less than 3 floors, or the \
                     lower end of its 95% CI reaches the floor. It is flagged in the quality \
                     report ('measurement_floor') and in the summary; the numbers are still \
                     shown. The floor is exported as 'perfratio.calibration'.",
                ),
        )
        .arg(
            Arg::new("below-floor")
                .long("below-floor")
                .action(ArgAction::Set)
                .value_name("ACTION")
                .value_parser(["warn", "fail"])
                .help("Action for a result within the runner floor: warn (default) or fail.")
                .long_help(
                    "What to do when a result is within the runner floor (see '--calibrate').\n\n  \
                     warn  The quality check warns (default)\n  \
                     fail  The quality check fails and perfratio exits with code 6 after \
                     printing and exporting the statistics",
                ),
        )
        .arg(
            Arg::new("fail-on-quality")
                .long("fail-on-quality")
                .action(ArgAction::Set)
                .value_name("GRADE")
                .value_parser(["comparable", "with_caveats", "unreliable"])
                .help("Exit with code 5 if a benchmark's quality grade is worse than GRADE.")
                .long_help(
                    "Exit with code 5 if the measurement quality grade of any benchmark is worse \
                     than GRADE (comparable > with_caveats > unreliable).\n\n  \
                     comparable    fail on with_caveats and unreliable\n  \
                     with_caveats  fail on unreliable\n\n\
                     A CI gate on the trustworthiness of a measurement; use it next to \
                     '--fail-if-regressed' (exit code 3), which gates on the size of a change.\n\n\
                     Example:\n\n  \
                       perfratio --compare base.json --fail-if-regressed 5% \
                       --fail-on-quality with_caveats --runs 30 './my-program'",
                ),
        )
        .arg(
            Arg::new("expect-output-sha256")
                .long("expect-output-sha256")
                .visible_alias("expect-output-hash")
                .action(ArgAction::Set)
                .value_name("HEX")
                .conflicts_with_all(["show-output", "show-output-on-failure", "until", "concurrency", "output"])
                .help("Fail when the SHA-256 of a run's stdout differs from HEX.")
                .long_help(
                    "Fail when the SHA-256 of a run's stdout differs from HEX (64 hex digits).\n\n\
                     Checks that the benchmarked command produced the right result, so that an \
                     implementation cannot look fast because it did less work. A run with a wrong \
                     output is invalid, which is different from a non-zero exit code. Stdout is \
                     captured (at most 16 MiB) and hashed after the timed window of the run, so \
                     validation is never part of the measured time, memory or energy. Failed \
                     runs (non-zero exit code) are not validated. Use '--on-invalid' to choose \
                     what happens to invalid runs.\n\n\
                     Example:\n\n  \
                       perfratio --expect-output-sha256 \
                       ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad 'printf abc'",
                ),
        )
        .arg(
            Arg::new("validate")
                .long("validate")
                .action(ArgAction::Set)
                .value_name("CMD")
                .conflicts_with_all(["show-output", "show-output-on-failure", "until", "concurrency", "output"])
                .help("Run CMD after each run with the run's stdout on stdin; non-zero marks the run invalid.")
                .long_help(
                    "Run CMD (through 'sh -c', or 'cmd.exe /C' on Windows) after each run. It \
                     receives the stdout of the run on stdin and the run number in the \
                     environment variable PERFRATIO_RUN_ID (the same number as PERFRATIO_ITERATION). \
                     A non-zero exit code marks the run invalid, which is different from a \
                     failure of the benchmarked command. The validator runs outside the timed \
                     window. Numeric tolerance and structure checks belong in CMD. Combine with \
                     '--expect-output-sha256' to check both.",
                ),
        )
        .arg(
            Arg::new("validate-first-only")
                .long("validate-first-only")
                .action(ArgAction::SetTrue)
                .help("Check only the first timed run (for long deterministic workloads).")
                .long_help(
                    "Check only the first timed run. Needs '--expect-output-sha256' or \
                     '--validate'. For long deterministic workloads whose output does not change.",
                ),
        )
        .arg(
            Arg::new("on-invalid")
                .long("on-invalid")
                .action(ArgAction::Set)
                .value_name("MODE")
                .value_parser(["fail", "omit", "keep"])
                .help("What to do with a run that fails validation: fail (default), omit or keep.")
                .long_help(
                    "What to do with a run that fails '--expect-output-sha256' or '--validate'.\n\n  \
                     fail  Abort the benchmark, naming the run (default)\n  \
                     omit  Drop the run from the statistics. Omitting can hide a failing \
                     implementation: the number of omitted runs is always printed and exported\n  \
                     keep  Keep the run and flag it in the output and the export\n\n\
                     The exit code policy ('--ignore-failure', '--omit-failed-runs') is separate \
                     and unchanged.",
                ),
        )
        .arg(
            Arg::new("style")
                .long("style")
                .short('l')
                .action(ArgAction::Set)
                .value_name("TYPE")
                .value_parser(["auto", "basic", "full", "nocolor", "color", "none"])
                .help(
                    "Set output style type (default: auto). Set this to 'basic' to disable output \
                     coloring and interactive elements. Set it to 'full' to enable all effects \
                     even if no interactive terminal was detected. Set this to 'nocolor' to \
                     keep the interactive output without any colors. Set this to 'color' to keep \
                     the colors without any interactive output. Set this to 'none' to disable all \
                     the output of the tool.",
                ),
        )
        .arg(
            Arg::new("energy")
                .long("energy")
                .short('E')
                .action(ArgAction::SetTrue)
                .help("Measure energy consumption (Joules) and average power draw (Watts) using hardware sensors."),
        )
        .arg(
            Arg::new("energy-baseline")
                .long("energy-baseline")
                .action(ArgAction::Set)
                .value_name("DURATION")
                .requires("energy")
                .allow_hyphen_values(true)
                .help("Measure idle power for DURATION (e.g. 2s) before benchmarking and also \
                       report net energy = measured - idle power x run time. Off by default."),
        )
        .arg(
            Arg::new("energy-domains")
                .long("energy-domains")
                .action(ArgAction::SetTrue)
                .requires("energy")
                .help("Also report energy per RAPL domain (package, core, uncore, dram, psys) \
                       where the hardware exposes them. The domains overlap: the total energy \
                       is the package energy and the other domains must not be added to it."),
        )
        .arg(
            Arg::new("energy-backend")
                .long("energy-backend")
                .value_name("BACKEND")
                .value_parser(["auto", "sysfs", "perf", "ioreport", "emi"])
                .help(
                    "Energy backend for '--energy': 'auto' (default) picks the native source: \
                     on Linux sysfs powercap, falling back to the perf power PMU when energy_uj \
                     is unreadable; on macOS (Apple Silicon) IOReport; on Windows the Energy \
                     Meter Interface. 'sysfs', 'perf', 'ioreport' and 'emi' force one (only on \
                     its own OS). Can also be set with PERFRATIO_ENERGY_BACKEND.",
                ),
        )
        .arg(
            Arg::new("deep-stats")
                .long("deep-stats")
                .action(ArgAction::SetTrue)
                .help("Perform deep statistical analysis (bootstrapped confidence intervals for mean, median, and stddev, plus hypothesis testing)."),
        )
        .arg(
            Arg::new("estimator")
                .long("estimator")
                .action(ArgAction::Set)
                .value_name("ESTIMATOR")
                .help("Central value used for ranking, relative speed, significance, the delta \
                       table, '--compare' and '--target-precision': 'mean' (default), \
                       'median' or a percentile such as 'p95'. Prefer the median or a \
                       percentile for skewed timings (GC pauses, page-cache effects); with few \
                       runs a percentile is noisy. Other than the mean, intervals are bootstrap \
                       intervals and the ratio carries a bootstrap interval instead of ± σ."),
        )
        .arg(
            Arg::new("confidence-level")
                .long("confidence-level")
                .action(ArgAction::Set)
                .value_name("LEVEL")
                .help("Confidence level of intervals, strictly between 0 and 1 (default: 0.95)."),
        )
        .arg(
            Arg::new("significance-level")
                .long("significance-level")
                .action(ArgAction::Set)
                .value_name("ALPHA")
                .help("Level of hypothesis tests and significance markers, strictly between 0 \
                       and 1 (default: 0.05)."),
        )
        .arg(
            Arg::new("bootstrap-resamples")
                .long("bootstrap-resamples")
                .action(ArgAction::Set)
                .value_name("N")
                .help("Number of bootstrap resamples, at least 100 (default: 5000)."),
        )
        .arg(
            Arg::new("metric")
                .long("metric")
                .action(ArgAction::Set)
                .value_name("METRIC")
                .value_parser(parse_metric_arg)
                .default_value("wall")
                .hide_default_value(true)
                .help(
                    "Primary metric used for statistics, outlier detection, comparison, and sorting:\n  \
                       * 'wall' (default, aliases: 'wall-clock', 'time'): wall-clock time\n  \
                       * 'cpu' (alias: 'total-cpu'): total CPU time (user + system)\n  \
                       * 'user': user CPU time\n  \
                       * 'system': system / kernel CPU time\n  \
                       * 'energy': energy consumed in Joules (auto-enables '--energy')\n  \
                       * 'memory' (aliases: 'rss', 'peak-memory'): peak resident set size (RSS)\n  \
                       * 'instructions': retired instructions (auto-enables '--counters', Linux)\n  \
                       * 'cycles': CPU cycles (auto-enables '--counters', Linux)\n  \
                       * 'custom:<name>': a value extracted with '--output-metric <name>=<regex>' \
                         (lower is better; every run needs a value)\n  \
                       * 'region:<name>': the time inside the region <name> that the program \
                         brackets with the region SDK (implies '--region=<name>'; every run \
                         needs the region)\n"
                ),
        )
        .arg(
            Arg::new("sort")
            .long("sort")
            .short('t')
            .action(ArgAction::Set)
            .value_name("METHOD")
            .value_parser(["auto", "command", "mean-time"])
            .default_value("auto")
            .hide_default_value(true)
            .help(
                "Specify the sort order of the speed comparison summary and the exported tables for \
                 markup formats (Markdown, AsciiDoc, org-mode):\n  \
                   * 'auto' (default): the speed comparison will be ordered by time and\n    \
                     the markup tables will be ordered by command (input order).\n  \
                   * 'command': order benchmarks in the way they were specified\n  \
                   * 'mean-time': order benchmarks by mean runtime\n"
            ),
        )
        .arg(
            Arg::new("subtract")
                .long("subtract")
                .action(ArgAction::Set)
                .value_name("CMD")
                .value_hint(ValueHint::CommandString)
                .help("Measure CMD first (with the same warmup and number of runs) and subtract \
                       its mean wall-clock, user, system time (and energy when '--energy' is used) \
                       from every run of the benchmarked commands, to measure a delta: e.g. \
                       --subtract \"python -c ''\" 'python -c \"import numpy\"'. Runs faster \
                       than the baseline count as 0, with a warning."),
        )
        .arg(
            Arg::new("target-precision")
                .long("target-precision")
                .action(ArgAction::Set)
                .value_name("PCT")
                .conflicts_with("runs")
                .help("Instead of a fixed number of runs, keep running each command until the \
                       95% confidence interval of its mean is at most PCT of the mean (e.g. \
                       '1%'), within '--min-runs' / '--max-runs' and '--max-benchmarking-time'. \
                       Low-noise commands then finish early and noisy ones get more runs."),
        )
        .arg(
            Arg::new("target-precision-method")
                .long("target-precision-method")
                .action(ArgAction::Set)
                .value_name("METHOD")
                .value_parser(["fixed", "sequential"])
                .requires("target-precision")
                .help("How '--target-precision' decides that the interval is narrow enough: \
                       'fixed' (default) re-checks the Student-t interval of the mean after \
                       every run, which stops too early more often than the confidence level \
                       says; 'sequential' uses an anytime-valid confidence sequence of the \
                       mean, valid however the run count was chosen, at the cost of about 2.5x \
                       the runs. Assumes independent runs with finite variance."),
        )
        .arg(
            Arg::new("max-benchmarking-time")
                .long("max-benchmarking-time")
                .action(ArgAction::Set)
                .value_name("SECONDS")
                .requires("target-precision")
                .help("Time budget per command for '--target-precision' (default: 60). If \
                       the target is not reached within it, a warning shows the precision \
                       that was reached."),
        )
        .arg(
            Arg::new("duration")
                .long("duration")
                .action(ArgAction::Set)
                .value_name("DURATION")
                .conflicts_with_all(["runs", "target-precision"])
                .help("Sample each command repeatedly until the summed run time reaches \
                       DURATION (e.g. 5s, 5000ms, 1m), with at least 3 runs ('--min-runs' \
                       and '--max-runs' override the floor and cap). Warmup, '--prepare' \
                       and '--conclude' time is not counted. Conflicts with '--runs' and \
                       '--target-precision'."),
        )
        .arg(
            Arg::new("timeout")
                .long("timeout")
                .action(ArgAction::Set)
                .value_name("DURATION")
                .allow_hyphen_values(true)
                .help("Kill a benchmark run that exceeds DURATION (e.g. 500ms, 2s, 1m). \
                       The benchmark is marked as \"timed out\" and later runs of that \
                       command are skipped."),
        )
        .arg(
            Arg::new("concurrency")
                .conflicts_with("subtract")
                .long("concurrency")
                .action(ArgAction::Set)
                .value_name("N")
                .conflicts_with_all(["until", "show-output-on-failure", "output-metric"])
                .help("Start N instances of the command simultaneously for every run. The \
                       reported time is the wall time until all N instances finished. \
                       User/system time is summed over the instances, memory is the \
                       maximum of any instance. The JSON export gains per-instance times \
                       and a throughput. N may contain a parameter, e.g. '-L c 1,2,4,8 \
                       --concurrency {c}', for one benchmark per value. Default: 1."),
        )
        .arg(
            Arg::new("region")
                .long("region")
                .action(ArgAction::Append)
                .value_name("NAME")
                .num_args(0..=1)
                .require_equals(true)
                .default_missing_value("default")
                .conflicts_with_all(["concurrency", "subtract"])
                .help("Measure only the region(s) the program brackets with the region SDK \
                       (perfratio_region_start/stop, see sdk/ and docs/specs/region-protocol.md). \
                       Without NAME the region 'default' is measured. Can be given several \
                       times. The JSON export lists the region times next to the whole-process \
                       time; use '--metric region:NAME' to rank on a region. CPU time, memory, \
                       counters and energy still cover the whole process. A command that \
                       reports no region fails."),
        )
        .arg(
            Arg::new("jobs")
                .short('j')
                .long("jobs")
                .action(ArgAction::Set)
                .value_name("N|auto")
                .conflicts_with_all(["concurrency", "energy", "optimize", "subtract"])
                .help("Run up to N independent invocations at the same time to collect \
                       samples faster. Every run is still timed on its own, unlike with \
                       '--concurrency'. Parallel runs compete for caches, memory bandwidth \
                       and CPUs, so the results are marked 'contended' and are not \
                       equivalent to sequential samples. 'auto' uses the number of logical \
                       CPUs. See '--schedule'. Default: 1."),
        )
        .arg(
            Arg::new("artifact-dir")
                .long("artifact-dir")
                .action(ArgAction::Set)
                .value_name("DIR")
                .value_hint(ValueHint::DirPath)
                .help("Create DIR/run-<id> for every run and export its path as \
                       PERFRATIO_RUN_DIR to --prepare, the command and --conclude of \
                       that run (and to --setup/--cleanup), so that parallel runs \
                       never share artifact files. The directories are kept."),
        )
        .arg(
            Arg::new("until")
                .long("until")
                .visible_alias("ready-when")
                .action(ArgAction::Set)
                .value_name("TEXT")
                .conflicts_with_all(["show-output", "show-output-on-failure", "output", "output-metric"])
                .help("Stop the timer as soon as the command writes TEXT to stdout (or stderr \
                       with --until-stderr), then terminate the process. A run that exits \
                       without printing TEXT fails."),
        )
        .arg(
            Arg::new("until-stderr")
                .long("until-stderr")
                .action(ArgAction::SetTrue)
                .requires("until")
                .help("Match the pattern specified by --until against stderr instead of stdout."),
        )
        .arg(
            Arg::new("output-metric")
                .long("output-metric")
                .action(ArgAction::Append)
                .value_name("NAME=REGEX")
                .conflicts_with_all(["show-output", "until"])
                .help("Extract a custom numeric metric from the command's standard output using \
                       the regular expression REGEX, which must contain at least one capture group. \
                       The value is extracted from the first capture group of the last match in the output. \
                       Can be specified multiple times for different metrics. Works with the default \
                       '--output=null' and with '--output=pipe'. A run where an ignored \
                       failure ('-i') left the metric out is exported as null."),
        )
        .arg(
            Arg::new("output-metric-stderr")
                .long("output-metric-stderr")
                .action(ArgAction::SetTrue)
                .requires("output-metric")
                .help("Extract the --output-metric values from stderr instead of stdout."),
        )
        .arg(
            Arg::new("precision")
                .long("precision")
                .action(ArgAction::Set)
                .value_name("N|auto")
                .value_parser(|value: &str| -> Result<String, String> {
                    match value {
                        "auto" => Ok(value.to_string()),
                        _ => match value.parse::<usize>() {
                            Ok(n) if n <= 9 => Ok(value.to_string()),
                            _ => Err("expected a number of decimals (0-9) or 'auto'".into()),
                        },
                    }
                })
                .help("Number of decimals of times in human-readable output (terminal, \
                       Markdown, AsciiDoc, Org, HTML). 'auto' rounds mean and σ to two \
                       significant digits of σ, e.g. '120.2 s ± 1.5 s' instead of \
                       '120.164 s ± 1.488 s'. CSV and JSON always keep full precision. \
                       Default: 3 decimals for seconds, 1 for milliseconds and microseconds."),
        )
        .arg(
            Arg::new("time-unit")
                .long("time-unit")
                .short('u')
                .action(ArgAction::Set)
                .value_name("UNIT")
                .value_parser(["microsecond", "millisecond", "second"])
                .help("Set the time unit to be used. Possible values: microsecond, millisecond, second. \
                       If the option is not given, the time unit is determined automatically. \
                       This option affects the standard output as well as all export formats except for CSV and JSON."),
        )
        .arg(
            Arg::new("export")
                .long("export")
                .short('e')
                .action(ArgAction::Append)
                .value_name("FILE")
                .value_hint(ValueHint::FilePath)
                .help(
                    "Export timing results to the given FILE.
                     \n\
                     Supported formats:\n\
                     .json     Includes summary and individual run timings (in seconds).\n\
                     .csv      Summary only (in seconds).\n\
                     .md       Markdown table.\n\
                     .adoc     AsciiDoc table.\n\
                     .org      Org-mode table.\n\
                     .html     Self-contained HTML report with plots.\n\
                     \n\
                     Notes:\n\
                     - Repeat --export to generate multiple formats.\n\
                     - Except for JSON and CSV, the output time unit can be changed using '--time-unit' option.\n\
                     - Defaults to JSON for unsupported or missing file extensions."
                ),
        )
        .arg(
            Arg::new("export-asciidoc")
                .long("export-asciidoc")
                .action(ArgAction::Set)
                .value_name("FILE")
                .value_hint(ValueHint::FilePath)
                .help("Export the timing summary statistics as an AsciiDoc table to the given FILE. \
                       The output time unit can be changed using the --time-unit option."),
        )
        .arg(
            Arg::new("export-csv")
                .long("export-csv")
                .action(ArgAction::Set)
                .value_name("FILE")
                .value_hint(ValueHint::FilePath)
                .help("Export the timing summary statistics as CSV to the given FILE. If you need \
                       the timing results for each individual run, use the JSON export format. \
                       The output time unit is always seconds."),
        )
        .arg(
            Arg::new("label")
                .long("label")
                .action(ArgAction::Append)
                .value_name("KEY=VALUE")
                .help("Attach a label to the JSON and CSV exports (can be given multiple times), \
                       e.g. --label commit=$GITHUB_SHA --label runner=graviton3. Labels appear \
                       under 'perfratio.labels' in JSON and as 'label_KEY' columns in CSV."),
        )
        .arg(
            Arg::new("export-markdown-runs")
                .long("export-markdown-runs")
                .action(ArgAction::Set)
                .value_name("FILE")
                .value_hint(ValueHint::FilePath)
                .help("Export one Markdown table per benchmark with every timed run (iteration, \
                       wall/user/system time, peak memory, energy, exit code) to the given FILE. \
                       Runs removed by --omit-failed-runs or --discard-outliers are not listed; \
                       the iteration numbers show the gaps."),
        )
        .arg(
            Arg::new("export-orgmode-runs")
                .long("export-orgmode-runs")
                .action(ArgAction::Set)
                .value_name("FILE")
                .value_hint(ValueHint::FilePath)
                .help("Like --export-markdown-runs, but as Emacs org-mode tables."),
        )
        .arg(
            Arg::new("export-asciidoc-runs")
                .long("export-asciidoc-runs")
                .action(ArgAction::Set)
                .value_name("FILE")
                .value_hint(ValueHint::FilePath)
                .help("Like --export-markdown-runs, but as AsciiDoc tables."),
        )
        .arg(
            Arg::new("export-html")
                .long("export-html")
                .action(ArgAction::Set)
                .value_name("FILE")
                .value_hint(ValueHint::FilePath)
                .help("Export a self-contained HTML report to the given FILE: summary table, \
                       histogram with kernel density estimate and run-order plot per command, \
                       and a box plot comparing all commands. No JavaScript, no external \
                       assets; light and dark themes. Also chosen by '--export FILE.html'."),
        )
        .arg(
            Arg::new("export-json")
                .long("export-json")
                .action(ArgAction::Set)
                .value_name("FILE")
                .value_hint(ValueHint::FilePath)
                .help("Export the timing summary statistics and timings of individual runs as JSON to the given FILE. \
                       The output time unit is always seconds"),
        )
        .arg(
            Arg::new("export-markdown")
                .long("export-markdown")
                .action(ArgAction::Set)
                .value_name("FILE")
                .value_hint(ValueHint::FilePath)
                .help("Export the timing summary statistics as a Markdown table to the given FILE. \
                       The output time unit can be changed using the --time-unit option."),
        )
        .arg(
            Arg::new("export-orgmode")
                .long("export-orgmode")
                .action(ArgAction::Set)
                .value_name("FILE")
                .value_hint(ValueHint::FilePath)
                .help("Export the timing summary statistics as an Emacs org-mode table to the given FILE. \
                       The output time unit can be changed using the --time-unit option."),
        )
        .arg(
            Arg::new("show-output")
                .long("show-output")
                .short('d')
                .action(ArgAction::SetTrue)
                .conflicts_with("style")
                .help(
                    "Print the stdout and stderr of the benchmark instead of suppressing it. \
                     This will increase the time it takes for benchmarks to run, \
                     so it should only be used for debugging purposes or \
                     when trying to benchmark output speed.",
                ),
        )
        .arg(
            Arg::new("delta-table")
                .long("delta-table")
                .action(ArgAction::SetTrue)
                .help(
                    "Print a table comparing every measured dimension (wall time, peak memory, \
                     energy, power, CPU time) of each command with the reference command: mean ± σ, \
                     the relative delta with its 95% confidence interval, and the p-value. \
                     Dimensions that were not measured are left out. With '--export-markdown', \
                     the tables are appended to the exported file.",
                ),
        )
        .arg(
            Arg::new("raw-ratios")
                .long("raw-ratios")
                .action(ArgAction::SetTrue)
                .conflicts_with("show-output")
                .help(
                    "Print only the relative values of the primary metric on stdout: one line \
                     per result in command order (a '--reference' command is the first), the reference (or the best command) being \
                     1.000000, others mean divided by the reference mean with six decimals and \
                     a '.' decimal point ('nan' if it cannot be computed). Nothing else is \
                     printed on stdout (implies '--style none'); warnings go to stderr. \
                     Exports to stdout ('-') are rejected.",
                ),
        )
        .arg(
            Arg::new("pareto")
                .long("pareto")
                .value_name("METRICS")
                .help(
                    "Mark the commands on the Pareto frontier of the comma-separated metrics, \
                     e.g. 'wall,energy,memory': the commands that no other command beats in one \
                     metric without being worse in another. A metric takes an optional statistic \
                     ('wall.p95', 'memory.max'). Commands without data for a metric are excluded, \
                     never counted as 0. Printed after the summary, and added to the JSON, \
                     Markdown and HTML exports.",
                ),
        )
        .arg(
            Arg::new("constraint")
                .long("constraint")
                .value_name("EXPR")
                .action(ArgAction::Append)
                .help(
                    "Only commands that satisfy the constraint are candidates for '--pareto' \
                     and '--objective'. EXPR is '<METRIC> <op> <VALUE>' with <=, <, >= or >, \
                     e.g. 'memory <= 256MiB' or 'wall.p95 <= 20ms'. Repeatable.",
                ),
        )
        .arg(
            Arg::new("objective")
                .long("objective")
                .value_name("DIRECTION:METRIC")
                .help(
                    "Pick the single best candidate under the constraints, e.g. \
                     'minimize:energy' or 'maximize:custom:ops'. Without '--pareto' the \
                     frontier is taken over this metric alone.",
                ),
        )
        .arg(
            Arg::new("noise-threshold")
                .long("noise-threshold")
                .value_name("PERCENT")
                .requires("pareto")
                .help(
                    "With '--pareto': a command counts as dominated only if another is better \
                     by more than PERCENT of the value on some metric and not worse beyond it \
                     on any. Otherwise it stays on the frontier as 'not dominated (within \
                     noise)'. Default: strict comparison.",
                ),
        )
        .arg(
            Arg::new("show-output-on-failure")
                .long("show-output-on-failure")
                .action(ArgAction::SetTrue)
                .conflicts_with_all(["show-output", "output"])
                .help(
                    "Capture the stdout and stderr of every run (the last 64 KiB of each) and \
                     print them only if the run fails: in the error message, or as a warning \
                     with '-i'/'--ignore-failure'. Unlike '--show-output', this works with \
                     '--style' and doesn't flood the terminal. Reading the output through pipes \
                     can affect the measurement slightly, like '--output=pipe'.",
                ),
        )
        .arg(
            Arg::new("output")
                .long("output")
                .short('O')
                .conflicts_with("show-output")
                .action(ArgAction::Append)
                .value_name("WHERE")
                .help(
                    "Control where the output of the benchmark is redirected. Note \
                     that some programs like 'grep' detect when standard output is \
                     /dev/null and apply certain optimizations. To avoid that, consider \
                     using '--output=pipe'.\n\
                     \n\
                     <WHERE> can be:\n\
                     \n  \
                       null:     Redirect output to /dev/null (the default).\n\
                     \n  \
                       pipe:     Feed the output through a pipe before discarding it.\n\
                     \n  \
                       inherit:  Don't redirect the output at all (same as '--show-output').\n\
                     \n  \
                       <FILE>:   Write the output to the given file.\n\n\
                    This option can be specified once for all commands or multiple times, once for \
                    each command. Note: If you want to log the output of each and every iteration, \
                    you can use a shell redirection and the '$PERFRATIO_ITERATION' environment variable:\n    \
                    perfratio 'my-command > output-${PERFRATIO_ITERATION}.log'\n\n",
                ),
        )
        .arg(
            Arg::new("input")
                .long("input")
                .short('I')
                .action(ArgAction::Set)
                .num_args(1)
                .value_name("WHERE")
                .help("Control where the input of the benchmark comes from.\n\
                       \n\
                       <WHERE> can be:\n\
                       \n  \
                         null:     Read from /dev/null (the default).\n\
                       \n  \
                         <FILE>:   Read the input from the given file."),
        )
        .arg(
            Arg::new("command-name")
                .long("command-name")
                .short('n')
                .action(ArgAction::Append)
                .num_args(0..=1)
                .default_missing_value("")
                .value_name("NAME")
                .help("Give a meaningful name to a command. This can be specified multiple times \
                       if several commands are benchmarked.\n\
                       Without a value, the commands are named by the words in which they differ \
                       (the common prefix and suffix are stripped; '#1', '#2', ... if nothing is \
                       shared). Because the next argument would be taken as the name, write \
                       '--command-name=' or put the option after the commands."),
        )
        // This option is hidden for now, as it is not yet clear yet if we want to 'stabilize' this,
        // see discussion in https://github.com/sharkdp/hyperfine/issues/527
        .arg(
            Arg::new("min-benchmarking-time")
            .long("min-benchmarking-time")
            .action(ArgAction::Set)
            .hide(true)
            .help("Set the minimum time (in seconds) to run benchmarks. Note that the number of \
                   benchmark runs is additionally influenced by the `--min-runs`, `--max-runs`, and \
                   `--runs` option.")
        )
        .arg(
            Arg::new("debug-mode")
            .long("debug-mode")
            .action(ArgAction::SetTrue)
            .hide(true)
            .help("Enable debug mode which does not actually run commands, but returns fake times when the command is 'sleep <time>'.")
        )
        .arg(
            Arg::new("generate-completions")
                .long("generate-completions")
                .action(ArgAction::Set)
                .value_name("SHELL")
                .value_parser(PossibleValuesParser::new(SHELLS))
                .help("Generate a shell completions script for the given shell and print it to stdout.")
        )
        .arg(
            Arg::new("affinity")
                .long("affinity")
                .action(ArgAction::Set)
                .value_name("CPUS")
                .help("Pin every benchmarked command (and --prepare/--conclude/--setup/--cleanup) \
                       to the given CPUs, e.g. '2', '0-3' or '0,2,4-5'. Reduces noise from \
                       migrations between cores, especially on CPUs with performance and \
                       efficiency cores. Linux and Windows. macOS cannot pin to CPUs: use \
                       'p-cores' or 'e-cores' (Apple Silicon) to prefer a core type."),
        )
        .arg(
            Arg::new("pin-workers")
                .long("pin-workers")
                .action(ArgAction::Set)
                .value_name("MODE")
                .num_args(0..=1)
                .require_equals(true)
                .default_missing_value("auto")
                .value_parser(["auto", "none"])
                .help("With '--jobs' > 1: give every worker its own CPUs, one physical core \
                       (with its SMT siblings) per worker, within '--affinity'. Default with \
                       '--jobs' > 1: auto; 'none' leaves the workers unpinned. Fails when \
                       there are fewer usable CPUs than jobs. Linux and Windows; elsewhere \
                       an explicit flag warns and the workers are not pinned. The partition \
                       is recorded in the JSON export."),
        )
        .arg(
            Arg::new("reserve-cpus")
                .long("reserve-cpus")
                .action(ArgAction::Set)
                .value_name("N|LIST")
                .help("With '--jobs' > 1: keep N CPUs (the highest ones, with their SMT \
                       siblings) or the CPUs of an explicit list such as '0' written as \
                       '0-0' or '0,1' for perfratio's own threads. Workers never run there. \
                       Linux and Windows."),
        )
        .arg(
            Arg::new("check-system")
                .long("check-system")
                .action(ArgAction::Set)
                .value_name("MODE")
                .num_args(0..=1)
                .require_equals(true)
                .default_missing_value("warn")
                .value_parser(["warn", "strict"])
                .help("Before benchmarking, report system conditions that add noise (CPU \
                       governor, turbo boost, load, battery, thermal throttling), with a hint \
                       for each problem. '--check-system=strict' aborts with exit code 4 if \
                       any check does not pass (useful on CI benchmark runners).")
                .long_help("Before benchmarking, report system conditions that make results \
                       noisy or irreproducible, with a hint for each problem:\n\
                       Linux: CPU frequency governor, turbo boost, load average, power source, \
                       temperature.\n\
                       macOS: load average, power source, Low Power Mode, thermal throttling.\n\n\
                       '--check-system' (or '=warn') only reports. '--check-system=strict' \
                       aborts with exit code 4 if any check does not pass, which is useful on \
                       dedicated benchmark machines and CI runners.\n\n\
                       Example:  perfratio --check-system=strict 'make'"),
        )
        .arg(
            Arg::new("priority")
                .long("priority")
                .action(ArgAction::Set)
                .value_name("POLICY")
                .value_parser(["normal", "high", "idle", "realtime"])
                .help("Scheduling priority of every benchmarked command (and \
                       --prepare/--conclude/--setup/--cleanup): 'high' and 'realtime' reduce \
                       preemption by other processes, 'idle' benchmarks background workloads. \
                       On macOS 'realtime' is nice -20 plus the foreground task role, not a \
                       real-time policy. Default: normal (inherited).")
                .long_help("Scheduling priority of every benchmarked command (and \
                       --prepare/--conclude/--setup/--cleanup).\n\n\
                       'normal' (default): inherit perfratio's priority.\n\
                       'high': nice -20 (Linux, macOS), HIGH_PRIORITY_CLASS (Windows).\n\
                       'idle': SCHED_IDLE (Linux), nice 19 (macOS), IDLE_PRIORITY_CLASS (Windows).\n\
                       'realtime': SCHED_FIFO at the maximum priority (Linux), \
                       REALTIME_PRIORITY_CLASS (Windows). Not available on macOS.\n\n\
                       'high' and 'realtime' need privileges: root or \
                       'sudo setcap cap_sys_nice+ep \"$(command -v perfratio)\"' on Linux, root on \
                       macOS, administrator rights on Windows (otherwise 'realtime' silently \
                       becomes 'high').\n\n\
                       Warning: a 'realtime' command that never blocks can starve the rest of \
                       the system, including perfratio itself.\n\n\
                       Example:  sudo perfratio --priority=realtime './compute'"),
        )
        .arg(
            Arg::new("resource-usage")
                .long("resource-usage")
                .visible_alias("rusage")
                .action(ArgAction::SetTrue)
                .help("Show the mean voluntary/involuntary context switches, minor/major page \
                       faults and block I/O operations per run, and export them per run in \
                       JSON ('resources'). Counters the OS cannot report (e.g. context switches \
                       on Windows) are shown as n/a and omitted from the JSON. The counters \
                       include the intermediate shell unless '--shell=none' is used."),
        )
        .arg(
            Arg::new("counters")
                .long("counters")
                .value_name("LIST")
                .num_args(0..=1)
                .require_equals(true)
                .default_missing_value("instructions,cycles,cache-references,cache-misses,branches,branch-misses")
                .help("Collect performance counters per run (Linux). LIST is \
                       comma-separated: instructions, cycles, cache-references, cache-misses, \
                       branches, branch-misses (default: all), plus the software events \
                       cpu-migrations, context-switches, page-faults and task-clock. Use \
                       '--counters=cycles' to give a list. Shorthands: 'default' (hardware \
                       list), 'sw' (cpu-migrations, context-switches), 'frequency' (cycles). \
                       An event takes a ':u' (user) or ':k' (kernel) modifier, e.g. \
                       'instructions:u,instructions:k' (see '--scope'); software events cannot \
                       be scoped. With cycles, the effective CPU frequency (cycles / \
                       task-clock) is shown and a warning is printed when it varies or drops. \
                       Counts user and kernel space by default, or user space only when \
                       perf_event_paranoid > 1; the scheduler events need kernel scope \
                       (perf_event_paranoid <= 1) and are otherwise reported as unavailable. \
                       Unavailable counters give one warning, never an error. Not available on \
                       macOS and Windows."),
        )
        .arg(
            Arg::new("scope")
                .long("scope")
                .value_name("SCOPE")
                .value_parser(["user", "kernel", "all", "split"])
                .requires("pmu-request")
                .help("Privilege levels that '--counters' counts: user, kernel, all (default, \
                       like 'perf stat') or split (every event as ':u' plus ':k'). Counting \
                       kernel space needs perf_event_paranoid <= 1 or CAP_PERFMON: with the \
                       default it falls back to user space with a warning ('counters_scope' \
                       in JSON), with 'kernel', 'split' or a ':k' modifier it is an error."),
        )
        .arg(
            Arg::new("topdown")
                .long("topdown")
                .action(ArgAction::SetTrue)
                .help("Collect Top-down Microarchitecture Analysis level 1 per run (Linux \
                       x86_64): the share of pipeline slots that retire work, are lost to \
                       bad speculation, or are stalled in the frontend or backend. The \
                       largest stall class is a hypothesis about the bottleneck, not a \
                       diagnosis. Needs Intel Ice Lake or later, or AMD Zen 4 or later; \
                       elsewhere it warns once and continues. On AMD it uses all six \
                       core counters, so '--counters' events are multiplexed."),
        )
        .arg(
            Arg::new("simd")
                .long("simd")
                .action(ArgAction::SetTrue)
                .help("Collect a floating-point scalar/vector breakdown per run (Linux, \
                       x86_64): retired scalar, 128-, 256- and 512-bit packed floating-point \
                       arithmetic and the vectorization ratio. Intel counts instructions, \
                       AMD Zen 4/5 counts uops, so compare ratios only within one vendor. \
                       Integer SIMD is not counted. Works with or without '--counters' \
                       and honors '--scope' (except 'split'). \
                       Unsupported CPUs, hybrid Intel CPUs, VMs without PMU and denied \
                       permissions give one warning, never an error."),
        )
        .arg(
            Arg::new("first-run")
                .long("first-run")
                .action(ArgAction::Set)
                .value_name("MODE")
                .value_parser(["include", "separate", "discard"])
                .default_value("include")
                .help("How to treat the first (cold) timing run of each command: 'include' it in \
                       the statistics (default), report it 'separate'ly and exclude it, or \
                       'discard' it silently. With 'separate' and 'discard', one extra run is \
                       performed so that the requested number of runs is still measured.")
                .long_help("How to treat the first (cold) timing run of each command. The first run \
                       often includes one-time costs (page cache, lazy loading, JIT compilation), \
                       which is exactly what you want to measure for a CLI's cold start, but \
                       distorts the warm statistics.\n\n\
                       'include' (default): the first run is part of the statistics.\n\
                       'separate': the first run is shown on its own line ('Cold (1st run)') and \
                       exported as 'first_run' in the JSON, and excluded from all statistics.\n\
                       'discard': the first run is excluded silently, like an extra warmup run.\n\n\
                       With 'separate' and 'discard', one extra run is performed so that the \
                       requested number of runs is still measured. If '--warmup' is used with \
                       'separate', the first warmup run is the cold one: it is reported \
                       (wall-clock time only) and no timing run is excluded.\n\n\
                       Example:  perfratio --first-run=separate 'node app.js'"),
        )
        .arg(
            Arg::new("discard-outliers")
                .long("discard-outliers")
                .action(ArgAction::Set)
                .value_name("Z")
                .num_args(0..=1)
                .require_equals(true)
                .default_missing_value("default")
                .help("Exclude outlier runs from all statistics. A run is an outlier if its \
                       modified Z-score exceeds Z (default: the threshold of the outlier warning). \
                       At most 5% of the runs (but at least one) are discarded; if more runs look \
                       like outliers, nothing is discarded and a warning is shown instead.")
                .long_help("Exclude outlier runs from all statistics (mean, σ, median, min/max, \
                       deep stats, relative speed and exports). A run is an outlier if its modified \
                       Z-score (distance from the median in units of the median absolute \
                       deviation, scaled by 1.4826) exceeds Z. Without a value, the threshold of \
                       the outlier warning is used.\n\n\
                       At most 5% of the runs (but always at least one) can be discarded. If more \
                       runs look like outliers, \
                       the run time distribution is probably multimodal: then nothing is \
                       discarded and a warning is shown instead.\n\n\
                       The indices of discarded runs are exported as 'discarded_outliers'; the \
                       per-run arrays in the JSON export only contain the kept runs.\n\n\
                       Example:  perfratio --discard-outliers=20 'make'"),
        )
        .arg(
            Arg::new("outlier-method")
                .long("outlier-method")
                .action(ArgAction::Set)
                .value_name("METHOD")
                .value_parser(["iqr", "modified-z"])
                .default_value("modified-z")
                .help("How outliers are counted for the outlier warning and the 'outliers' export \
                       field: 'modified-z' (modified Z-score, default) or 'iqr' (Tukey's fences, \
                       1.5 × interquartile range). Does not affect --discard-outliers."),
        )
        .arg(
            Arg::new("suppress-outlier-warnings")
                .long("suppress-outlier-warnings")
                .action(ArgAction::SetTrue)
                .help("Suppress statistical outlier warnings (useful for automated scripts and CI).")
        )
        .arg(
            Arg::new("no-off-cpu-warning")
                .long("no-off-cpu-warning")
                .action(ArgAction::SetTrue)
                .hide(true)
                .help("Suppress off-CPU time warnings (for I/O- or sleep-bound commands).")
        )
        .arg(
            Arg::new("suppress-warnings")
                .long("suppress-warnings")
                .alias("no-warning")
                .value_name("KIND")
                .value_delimiter(',')
                .value_parser(["off-cpu", "outliers", "all"])
                .help("Suppress specific warning types ('off-cpu', 'outliers', 'all').")
        )
        .arg(
            Arg::new("schedule")
                .long("schedule")
                .value_name("MODE")
                .value_parser(["grouped", "round-robin", "interleaved", "saturate", "exclusive", "fair", "randomized", "balanced"])
                .help("Set benchmark execution schedule: 'grouped' (run all iterations per command; default) or 'round-robin' / 'interleaved' (interleave iterations across commands to mitigate temporal bias), 'randomized' (interleaved, with a seeded shuffle of the command order in every round; '--seed' sets the seed) or 'balanced' (interleaved, order rotated every round so each command takes each position equally often). With '--jobs' > 1: 'saturate' (keep every worker busy; default), 'exclusive' (a command never runs concurrently with itself) or 'fair' (every command keeps the same number of runs).")
        )
        .arg(
            Arg::new("round-robin")
                .long("round-robin")
                .action(ArgAction::SetTrue)
                .help("Shortcut for '--schedule round-robin' (interleave benchmark runs across commands).")
        )
        .arg(
            Arg::new("allow-setup-with-round-robin")
                .long("allow-setup-with-round-robin")
                .action(ArgAction::SetTrue)
                .help("Allow combining parametrized '--setup' or '--cleanup' with round-robin scheduling. \
                       Note: setup/cleanup runs only once per batch of runs for a command, not before every \
                       single iteration of interleaved commands.")
        )
}

#[test]
fn verify_app() {
    build_command().debug_assert();
}

/// Validates `--metric`. Mirrors `Metric::from_str` (`src/metric.rs`), which this
/// file cannot use because `build.rs` compiles it standalone.
fn parse_metric_arg(s: &str) -> Result<String, String> {
    const BUILTIN: [&str; 13] = [
        "wall",
        "wall-clock",
        "time",
        "cpu",
        "total-cpu",
        "user",
        "system",
        "energy",
        "memory",
        "rss",
        "peak-memory",
        "instructions",
        "cycles",
    ];
    if BUILTIN.contains(&s.to_ascii_lowercase().as_str()) {
        return Ok(s.to_string());
    }
    if let Some(name) = s
        .get(..7)
        .filter(|p| p.eq_ignore_ascii_case("custom:"))
        .map(|_| &s[7..])
    {
        if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Ok(s.to_string());
        }
        return Err("expected 'custom:<name>' with a name of letters, digits and '_'".into());
    }
    if let Some(name) = s
        .get(..7)
        .filter(|p| p.eq_ignore_ascii_case("region:"))
        .map(|_| &s[7..])
    {
        if !name.is_empty()
            && name.len() <= 64
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return Ok(s.to_string());
        }
        return Err("expected 'region:<name>' with a name of letters, digits and '_'".into());
    }
    Err(format!(
        "possible values are {}, custom:<name>, region:<name>",
        BUILTIN.join(", ")
    ))
}
