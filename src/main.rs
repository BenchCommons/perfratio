#![cfg_attr(
    all(windows, feature = "windows_process_extensions_main_thread_handle"),
    feature(windows_process_extensions_main_thread_handle)
)]
#![warn(clippy::undocumented_unsafe_blocks)]

use std::env;

use clap_complete::Shell;
use perfratio::benchmark::scheduler::Scheduler;
use perfratio::cli::{build_command, get_cli_arguments};
use perfratio::command::Commands;
use perfratio::export::ExportManager;
use perfratio::options::Options;

use anyhow::{Context, Result};
use perfratio::output::colors;

use perfratio::{cli, compare, error, options, output, system_check, util};

fn parse_shell(name: &str) -> Result<Shell> {
    match name {
        "bash" => Ok(Shell::Bash),
        "zsh" => Ok(Shell::Zsh),
        "fish" => Ok(Shell::Fish),
        "powershell" => Ok(Shell::PowerShell),
        "elvish" => Ok(Shell::Elvish),
        other => Err(anyhow::anyhow!("unsupported shell: {other}")),
    }
}

fn run() -> Result<()> {
    perfratio::util::interrupt::install()?;

    // Enabled ANSI colors on Windows 10
    #[cfg(windows)]
    // Fails on consoles without ANSI support; the output is then uncolored
    let _ = colored::control::set_virtual_terminal(true);

    let cli_arguments = get_cli_arguments(env::args_os());

    if let Some(shell) = cli_arguments.get_one::<String>("generate-completions") {
        let shell = parse_shell(shell)?;
        let mut command = build_command();
        clap_complete::generate(shell, &mut command, "perfratio", &mut std::io::stdout());
        if shell == Shell::Fish {
            perfratio::out!("{}", cli::FISH_COMMAND_COMPLETION);
        }
        return Ok(());
    }

    let mut options = Options::from_cli_arguments(&cli_arguments)?;
    if let Some(precision) = cli_arguments.get_one::<String>("precision") {
        util::units::set_precision(match precision.as_str() {
            "auto" => util::units::Precision::Auto,
            // Validated by clap
            decimals => util::units::Precision::Fixed(decimals.parse().unwrap_or(3)),
        });
    }
    if options.raw_ratios {
        let to_stdout = [
            "export",
            "export-asciidoc",
            "export-json",
            "export-csv",
            "export-markdown",
            "export-orgmode",
            "export-markdown-runs",
            "export-orgmode-runs",
            "export-asciidoc-runs",
            "export-html",
        ]
        .iter()
        .filter_map(|flag| cli_arguments.get_many::<String>(flag))
        .flatten()
        .any(|target| target == "-");
        if to_stdout {
            anyhow::bail!("'--raw-ratios' prints only ratios on stdout: it cannot be combined with an export to '-'");
        }
    }
    let commands = Commands::from_cli_arguments(&cli_arguments)?;
    let export_manager = ExportManager::from_cli_arguments(
        &cli_arguments,
        options.time_unit,
        options.sort_order_exports,
        options.metric,
        options.counters.as_ref(),
        options.topdown.as_ref(),
    )?;

    // Load the baseline before benchmarking, so that a bad file fails fast
    let baseline = cli_arguments
        .get_one::<String>("compare")
        .map(|path| perfratio::import::import_json(path).map(|results| (path.as_str(), results)))
        .transpose()?;
    let regression_threshold = cli_arguments
        .get_one::<String>("fail-if-regressed")
        .map(|value| compare::parse_threshold(value).map_err(anyhow::Error::msg))
        .transpose()?;

    let mut imported_results = vec![];
    if let Some(files) = cli_arguments.get_many::<String>("import-json") {
        for file in files {
            imported_results.extend(perfratio::import::import_json_for_metric(
                file,
                Some(options.metric),
            )?);
        }
    }

    if commands.iter().count() == 0
        && options.reference_command.is_none()
        && imported_results.is_empty()
    {
        anyhow::bail!("No commands to benchmark and no JSON files imported.");
    }

    options.validate_against_command_list(&commands)?;

    if options.priority == util::priority::Priority::Realtime {
        eprintln!(
            "{} '--priority realtime': a benchmarked command that never blocks can starve the \
             rest of the system, including perfratio itself.",
            output::colors::yellow("Warning:")
        );
    }

    if let Some(mode) = cli_arguments.get_one::<String>("check-system") {
        let mut checks = system_check::run_checks();
        checks.extend(system_check::scheduler_check(options.counters.as_ref()));
        let passed = system_check::all_passed(&checks);
        if options.output_style != options::OutputStyleOption::Disabled {
            perfratio::outln!("{}", system_check::report(&checks));
        } else if mode == "strict" && !passed {
            // '--style none' hides the report, but a failure must be explained
            eprint!("{}", system_check::report(&checks));
        }
        if mode == "strict" && !passed {
            return Err(error::SystemCheckFailed.into());
        }
    }

    let mut scheduler = Scheduler::new(&commands, &options, &export_manager);
    scheduler.add_imported_results(imported_results);
    scheduler.run_benchmarks()?;
    scheduler.print_relative_speed_comparison();
    scheduler.print_below_floor_warning();
    scheduler.print_raw_ratios();
    if cli_arguments.get_flag("delta-table") {
        scheduler.print_delta_table();
    }
    scheduler.print_pareto();
    scheduler.final_export()?;

    if let Some((baseline_path, baseline)) = &baseline {
        let comparison = compare::compare(baseline, scheduler.results());
        if options.output_style != options::OutputStyleOption::Disabled {
            perfratio::outln!(
                "{}",
                compare::terminal_table(
                    &comparison,
                    baseline_path,
                    regression_threshold,
                    options.time_unit
                )
            );
        }
        if let Some(path) = cli_arguments.get_one::<String>("export-diff-markdown") {
            perfratio::export::write_to_file(
                path,
                compare::markdown_table(&comparison, regression_threshold, options.time_unit)
                    .as_bytes(),
            )
            .with_context(|| format!("Could not write '{path}'"))?;
        }
        if let Some(threshold) = regression_threshold {
            let regressions = comparison.regressions(threshold).count();
            if regressions > 0 && !perfratio::util::interrupt::interrupted() {
                return Err(error::RegressionDetected(regressions).into());
            }
        }
    }

    if let Some(gate) = options.fail_on_quality {
        let worse = scheduler
            .results()
            .iter()
            .filter_map(|r| r.quality.as_ref())
            .filter(|q| q.grade > gate)
            .count();
        if worse > 0 && !perfratio::util::interrupt::interrupted() {
            return Err(error::QualityGateFailed(worse, gate.name()).into());
        }
    }

    if options.below_floor == perfratio::calibration::BelowFloorAction::Fail {
        let below = scheduler
            .results()
            .iter()
            .filter_map(|r| r.quality.as_ref())
            .filter(|q| q.below_resolution == Some(true))
            .count();
        if below > 0 && !perfratio::util::interrupt::interrupted() {
            return Err(error::BelowFloor(below).into());
        }
    }

    if perfratio::util::interrupt::interrupted() {
        std::process::exit(130);
    }

    Ok(())
}

fn main() {
    match run() {
        Ok(_) => {}
        Err(e) => {
            if e.is::<perfratio::error::Interrupted>() {
                std::process::exit(130);
            }
            if e.is::<perfratio::error::RegressionDetected>() {
                eprintln!("{} {:#}", colors::red("Error:"), e);
                std::process::exit(3);
            }
            if e.is::<perfratio::error::QualityGateFailed>() {
                eprintln!("{} {:#}", colors::red("Error:"), e);
                std::process::exit(5);
            }
            if e.is::<perfratio::error::BelowFloor>() {
                eprintln!("{} {:#}", colors::red("Error:"), e);
                std::process::exit(6);
            }
            if e.is::<perfratio::error::SystemCheckFailed>() {
                eprintln!("{} {:#}", colors::red("Error:"), e);
                std::process::exit(4);
            }
            eprintln!("{} {:#}", colors::red("Error:"), e);
            if perfratio::util::interrupt::interrupted() {
                std::process::exit(130);
            }
            std::process::exit(1);
        }
    }
}
