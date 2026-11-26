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

use perfratio::{budget, cli, compare, error, options, output, system_check, util};

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
    perfratio::stats::settings::install(options.stat_settings.clone());
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
        options.execution_info(),
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

    // '--budget', '--budget-file' and the '--fail-if-regressed' shorthand (#325)
    let mut budget_texts: Vec<String> = cli_arguments
        .get_many::<String>("budget")
        .into_iter()
        .flatten()
        .cloned()
        .collect();
    for path in cli_arguments
        .get_many::<String>("budget-file")
        .into_iter()
        .flatten()
    {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Could not read the budget file '{path}'"))?;
        budget_texts.extend(budget::parse_budget_file(path, &content).map_err(anyhow::Error::msg)?);
    }
    let explicit_budgets = !budget_texts.is_empty();
    let mut budgets: Vec<budget::Budget> = regression_threshold
        .map(budget::Budget::wall_regression)
        .into_iter()
        .collect();
    for text in &budget_texts {
        budgets.push(budget::Budget::parse(text).map_err(anyhow::Error::msg)?);
    }
    if baseline.is_none() {
        if let Some(b) = budgets.iter().find(|b| b.kind == budget::Kind::Relative) {
            anyhow::bail!(
                "The relative budget '{}' needs a baseline: add '--compare FILE', or use an absolute limit such as 'memory<=256MiB'",
                b.text
            );
        }
    }
    let on_unavailable: budget::OnUnavailable = cli_arguments
        .get_one::<String>("on-unavailable")
        .map(|v| v.parse().map_err(anyhow::Error::msg))
        .transpose()?
        .unwrap_or_default();

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
        checks.extend(system_check::worker_cpus_check(
            options.cpu_partition.as_ref(),
        ));
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
    scheduler.print_raw_ratios();
    if cli_arguments.get_flag("delta-table") {
        scheduler.print_delta_table();
    }
    scheduler.print_pareto();

    // The comparison and the budgets go into the JSON export, so they come first
    let comparison = baseline
        .as_ref()
        .map(|(_, baseline)| compare::compare(baseline, scheduler.results()));
    let budget_report = (!budgets.is_empty()).then(|| {
        budget::evaluate(
            &budgets,
            comparison.as_ref(),
            scheduler.results(),
            baseline.as_ref().map(|(path, _)| *path),
            on_unavailable,
        )
    });
    export_manager.set_compare(
        budget_report
            .as_ref()
            .and_then(|report| serde_json::to_value(report).ok()),
    );
    scheduler.final_export()?;

    if let (Some((baseline_path, _)), Some(comparison)) = (&baseline, &comparison) {
        if options.output_style != options::OutputStyleOption::Disabled {
            perfratio::outln!(
                "{}",
                compare::terminal_table(
                    comparison,
                    baseline_path,
                    regression_threshold,
                    options.time_unit
                )
            );
        }
    }
    if let (true, Some(report)) = (explicit_budgets, &budget_report) {
        if options.output_style != options::OutputStyleOption::Disabled {
            let text = report.terminal();
            if !text.is_empty() {
                perfratio::outln!("{text}");
            }
        }
    }
    if let Some(path) = cli_arguments.get_one::<String>("export-diff-markdown") {
        if let Some(comparison) = &comparison {
            let mut markdown =
                compare::markdown_table(comparison, regression_threshold, options.time_unit);
            if let (true, Some(report)) = (explicit_budgets, &budget_report) {
                markdown.push_str(&report.markdown());
            }
            perfratio::export::write_to_file(path, markdown.as_bytes())
                .with_context(|| format!("Could not write '{path}'"))?;
        }
    }
    if let Some(report) = &budget_report {
        let failures = report.failures();
        if failures > 0 && !perfratio::util::interrupt::interrupted() {
            return Err(if explicit_budgets {
                error::BudgetFailed(failures).into()
            } else {
                error::RegressionDetected(failures).into()
            });
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
            if e.is::<perfratio::error::RegressionDetected>()
                || e.is::<perfratio::error::BudgetFailed>()
            {
                eprintln!("{} {:#}", colors::red("Error:"), e);
                std::process::exit(3);
            }
            if e.is::<perfratio::error::QualityGateFailed>() {
                eprintln!("{} {:#}", colors::red("Error:"), e);
                std::process::exit(5);
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
