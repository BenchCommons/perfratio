use super::benchmark_result::BenchmarkResult;
use super::executor::{Executor, MockExecutor, RawExecutor, ShellExecutor};
use super::{relative_speed, Benchmark, BenchmarkRunner};
use colored::Colorize as _;
use std::cmp::Ordering;

use crate::command::{Command, Commands};
use crate::export::ExportManager;
use crate::metric::Metric;
use crate::optimize::{self, OptimizeReport, PointData};
use crate::options::{ExecutorKind, Options, OutputStyleOption, ScheduleMode, SortOrder};
use crate::output::colors;
use crate::output::format::{format_duration, format_duration_unit};
use crate::output::progress_bar::{
    format_progress_estimate, get_progress_bar, replace_message_template,
};
use crate::parameter::ParameterValue;
use crate::stats::significance::Significance;

use anyhow::Result;

pub struct Scheduler<'a> {
    commands: &'a Commands<'a>,
    options: &'a Options,
    export_manager: &'a ExportManager,
    results: Vec<BenchmarkResult>,
    /// Index into `results` of the `--reference` command's result, if any.
    /// Imported results (`--import-json`) come first, so this is not
    /// necessarily 0.
    reference_index: Option<usize>,
    imported_count: usize,
    /// One report per `--optimize` search
    optimize_reports: Vec<OptimizeReport>,
}

/// The `optimize` JSON value: an object for one search, an array for several.
fn optimize_json(reports: &[OptimizeReport]) -> Option<serde_json::Value> {
    match reports {
        [] => None,
        [one] => serde_json::to_value(one).ok(),
        many => serde_json::to_value(many).ok(),
    }
}

impl<'a> Scheduler<'a> {
    pub fn new(
        commands: &'a Commands,
        options: &'a Options,
        export_manager: &'a ExportManager,
    ) -> Self {
        Self {
            commands,
            options,
            export_manager,
            results: vec![],
            reference_index: None,
            imported_count: 0,
            optimize_reports: Vec::new(),
        }
    }

    /// All results so far (imported ones first)
    pub fn results(&self) -> &[BenchmarkResult] {
        &self.results
    }

    pub fn add_imported_results(&mut self, imported: Vec<BenchmarkResult>) {
        self.imported_count += imported.len();
        for res in imported {
            if self.options.output_style != OutputStyleOption::Disabled {
                crate::outln!(
                    "{}{}: {} (imported)",
                    "Benchmark ".bold(),
                    (self.results.len() + 1).to_string().bold(),
                    colors::cyan(&res.command_with_unused_parameters)
                );
            }
            self.results.push(res);
        }
    }

    pub fn run_benchmarks(&mut self) -> Result<()> {
        let reference = self
            .options
            .reference_command
            .as_ref()
            .map(|cmd| Command::new(self.options.reference_name.as_deref(), cmd));

        let total_live_commands = reference.iter().count() + self.commands.iter().count();
        if total_live_commands == 0 {
            return Ok(());
        }

        // One executor per distinct shell ('--shell' may be given per
        // command), each calibrated once
        let mut executors: Vec<(&ExecutorKind, Box<dyn Executor>)> = Vec::new();
        for kind in &self.options.executor_kinds {
            if executors.iter().any(|(k, _)| *k == kind) {
                continue;
            }
            let mut executor: Box<dyn Executor> = match kind {
                ExecutorKind::Raw => Box::new(RawExecutor::new(self.options)),
                ExecutorKind::Mock(shell) => Box::new(
                    MockExecutor::new(shell.clone(), self.options.timeout)
                        .with_counters(self.options.counters.clone())
                        .with_topdown(self.options.topdown.clone())
                        .with_regions(&self.options.regions)
                        .with_concurrency(self.options.concurrency)
                        .with_validation(self.options.validation.clone()),
                ),
                ExecutorKind::Shell(shell) => Box::new(ShellExecutor::new(shell, self.options)),
            };
            executor.calibrate()?;
            executors.push((kind, executor));
        }
        // --subtract: one baseline per executor (i.e. per shell)
        let baselines: Vec<Option<super::benchmark_result::Baseline>> =
            match &self.options.subtract_command {
                Some(command) => executors
                    .iter()
                    .map(|(_, executor)| {
                        super::measure_baseline(command, self.options, &**executor).map(Some)
                    })
                    .collect::<Result<_>>()?,
                None => vec![None; executors.len()],
            };
        let baseline_for = |number: usize| {
            let kind = self.options.executor_kind_for(number);
            executors
                .iter()
                .position(|(k, _)| *k == kind)
                .and_then(|index| baselines[index].clone())
        };
        let executor_for = |number: usize| -> &dyn Executor {
            let kind = self.options.executor_kind_for(number);
            executors
                .iter()
                .find(|(k, _)| *k == kind)
                .map(|(_, executor)| &**executor)
                .unwrap_or(&*executors[0].1)
        };

        let display_offset = self.results.len();
        // The reference command (if any) is benchmarked first, right after the
        // imported results.
        self.reference_index = reference.as_ref().map(|_| display_offset);
        // `--concurrency {param}`: the instance count of every parameter combination
        let resolved: Vec<Command> = reference
            .iter()
            .chain(self.commands.iter())
            .map(|command| match &self.options.concurrency_template {
                Some(template) => command.clone().with_concurrency_template(template),
                None => Ok(command.clone()),
            })
            .collect::<Result<_>>()?;
        let commands_to_run: Vec<(usize, &Command)> = resolved.iter().enumerate().collect();

        if (self.options.schedule == ScheduleMode::RoundRobin && commands_to_run.len() > 1)
            || self.options.schedule.pool_policy().is_some()
        {
            let mut runners: Vec<BenchmarkRunner> = commands_to_run
                .iter()
                .map(|&(number, cmd)| {
                    let mut runner = BenchmarkRunner::new(
                        number,
                        number + display_offset,
                        cmd,
                        self.options,
                        executor_for(number),
                    );
                    runner.baseline = baseline_for(number);
                    runner
                })
                .collect();

            // 1. Setup phase for all commands
            for runner in &runners {
                if crate::util::interrupt::interrupted() {
                    break;
                }
                runner.run_setup()?;
            }

            // 2. Warmup phase: with `--warmup auto`, each command warms up until
            // it is stable (before interleaving starts); otherwise interleaved
            if self.options.warmup_auto {
                let progress_bar = if self.options.output_style != OutputStyleOption::Disabled {
                    Some(get_progress_bar(
                        super::AUTO_WARMUP_MAX_RUNS * runners.len() as u64,
                        "Performing warmup runs (auto)",
                        self.options.output_style,
                    ))
                } else {
                    None
                };
                for runner in &mut runners {
                    if crate::util::interrupt::interrupted() {
                        break;
                    }
                    match runner.run_auto_warmup(|| {
                        if let Some(bar) = progress_bar.as_ref() {
                            bar.inc(1);
                        }
                    }) {
                        Ok(w) => runner.warmup = Some(w),
                        Err(e) if e.is::<crate::error::Interrupted>() => break,
                        Err(e) => {
                            if let Some(bar) = progress_bar.as_ref() {
                                bar.finish_and_clear();
                            }
                            return Err(e);
                        }
                    }
                }
                if let Some(bar) = progress_bar.as_ref() {
                    bar.finish_and_clear();
                }
            } else if self.options.warmup_count > 0 {
                let progress_bar = if self.options.output_style != OutputStyleOption::Disabled {
                    Some(get_progress_bar(
                        self.options.warmup_count * runners.len() as u64,
                        "Performing round-robin warmup runs",
                        self.options.output_style,
                    ))
                } else {
                    None
                };

                'warmup: for w in 0..self.options.warmup_count {
                    for runner in &mut runners {
                        if crate::util::interrupt::interrupted() {
                            break 'warmup;
                        }
                        if runner.timed_out {
                            continue;
                        }
                        match runner.run_warmup_iteration(w) {
                            Ok(_) => {
                                if let Some(bar) = progress_bar.as_ref() {
                                    bar.inc(1);
                                }
                            }
                            Err(e) if e.is::<crate::error::Interrupted>() => break 'warmup,
                            Err(e) => {
                                if let Some(bar) = progress_bar.as_ref() {
                                    bar.finish_and_clear();
                                }
                                return Err(e);
                            }
                        }
                    }
                }

                if let Some(bar) = progress_bar.as_ref() {
                    bar.finish_and_clear();
                }
            }

            // 3. Initial measurement phase
            let progress_bar = if self.options.output_style != OutputStyleOption::Disabled {
                let bar = get_progress_bar(
                    runners.len() as u64,
                    "Initial round-robin measurements",
                    self.options.output_style,
                );
                Some(replace_message_template(bar, "{msg} {elapsed_precise}"))
            } else {
                None
            };

            for runner in &mut runners {
                if crate::util::interrupt::interrupted() {
                    break;
                }
                if runner.timed_out {
                    continue;
                }
                match runner.run_initial_measurement() {
                    Ok(()) => {
                        if let Some(bar) = progress_bar.as_ref() {
                            bar.inc(1);
                        }
                    }
                    Err(e) if e.is::<crate::error::Interrupted>() => break,
                    Err(e) => {
                        if let Some(bar) = progress_bar.as_ref() {
                            bar.finish_and_clear();
                        }
                        return Err(e);
                    }
                }
            }

            if let Some(bar) = progress_bar.as_ref() {
                bar.finish_and_clear();
            }

            let common = match self.options.run_bounds {
                crate::options::RunBounds {
                    min,
                    max: Some(max),
                } if min == max => min,
                _ => {
                    let per_round: f64 = runners.iter().map(|r| r.initial_total_time).sum();
                    let n = if per_round > 0.0 {
                        (self
                            .options
                            .duration
                            .unwrap_or(self.options.min_benchmarking_time)
                            / per_round) as u64
                    } else {
                        self.options.run_bounds.min
                    };
                    let n = n.max(self.options.run_bounds.min);
                    self.options.run_bounds.max.map_or(n, |m| n.min(m)).max(1)
                }
            };
            for r in &mut runners {
                r.count = r.whole_cycles(common) + r.extra_runs();
            }

            let max_count = runners.iter().map(|r| r.count).max().unwrap_or(common);
            let total_remaining_runs: u64 = runners.iter().map(|r| r.count.saturating_sub(1)).sum();

            let progress_bar = if self.options.output_style != OutputStyleOption::Disabled
                && total_remaining_runs > 0
            {
                Some(get_progress_bar(
                    total_remaining_runs,
                    "Performing round-robin benchmark runs",
                    self.options.output_style,
                ))
            } else {
                None
            };

            // 4. Timing iterations: on the worker pool with `--jobs`, otherwise
            // interleaved
            if let Some(policy) = self.options.schedule.pool_policy() {
                let adaptive =
                    self.options.target_precision.is_some() || self.options.duration.is_some();
                let budget = self.options.max_benchmarking_time * runners.len() as f64;
                let multiple_runners = runners.len() > 1;
                let time_unit = self.options.time_unit;
                let measure_energy = self.options.measure_energy;
                let pooled = super::pool::run_pool(
                    &mut runners,
                    policy,
                    self.options.jobs,
                    adaptive,
                    budget,
                    self.options.cpu_partition.as_ref(),
                    |runner| {
                        let Some(bar) = progress_bar.as_ref() else {
                            return;
                        };
                        bar.inc(1);
                        let msg = format_progress_estimate(
                            &runner.times_real,
                            time_unit,
                            measure_energy.then_some(&runner.energy_measurements),
                        );
                        if multiple_runners {
                            bar.set_message(format!("{}: {}", runner.command.get_name(), msg));
                        } else {
                            bar.set_message(msg);
                        }
                    },
                );
                if let Err(e) = pooled {
                    if let Some(bar) = progress_bar.as_ref() {
                        bar.finish_and_clear();
                    }
                    return Err(e);
                }
            } else {
                // 4. Interleaved timing iterations. With --target-precision, each
                // command keeps running until its mean is precise enough; the
                // commands share the wall-clock time, and so the time budget.
                let adaptive =
                    self.options.target_precision.is_some() || self.options.duration.is_some();
                let started = std::time::Instant::now();
                let budget = self.options.max_benchmarking_time * runners.len() as f64;
                let multiple_runners = runners.len() > 1;
                let mut i = 1;
                'timing: loop {
                    if adaptive {
                        if !runners.iter().any(|r| r.needs_more_runs(started, budget)) {
                            break;
                        }
                    } else if i >= max_count {
                        break;
                    }
                    for runner in &mut runners {
                        if crate::util::interrupt::interrupted() {
                            break 'timing;
                        }
                        let run = if adaptive {
                            !runner.timed_out && runner.needs_more_runs(started, budget)
                        } else {
                            !runner.timed_out && i < runner.count
                        };
                        if run {
                            let msg = format_progress_estimate(
                                &runner.times_real,
                                self.options.time_unit,
                                if self.options.measure_energy {
                                    Some(&runner.energy_measurements)
                                } else {
                                    None
                                },
                            );
                            if let Some(bar) = progress_bar.as_ref() {
                                if multiple_runners {
                                    bar.set_message(format!(
                                        "{}: {}",
                                        runner.command.get_name(),
                                        msg
                                    ));
                                } else {
                                    bar.set_message(msg);
                                }
                            }

                            match runner.run_timed_iteration(i) {
                                Ok(()) => {
                                    if let Some(bar) = progress_bar.as_ref() {
                                        bar.inc(1);
                                    }
                                }
                                Err(e) if e.is::<crate::error::Interrupted>() => break 'timing,
                                Err(e) => {
                                    if let Some(bar) = progress_bar.as_ref() {
                                        bar.finish_and_clear();
                                    }
                                    return Err(e);
                                }
                            }
                        }
                    }
                    i += 1;
                }
            }

            if let Some(bar) = progress_bar.as_ref() {
                bar.finish_and_clear();
            }

            // 5. Cleanup phase
            for runner in &runners {
                if crate::util::interrupt::interrupted() {
                    let _ = runner.run_cleanup();
                } else {
                    runner.run_cleanup()?;
                }
            }

            // 6. Finish and collect results
            for runner in runners {
                if runner.times_real.is_empty() && !runner.timed_out {
                    if self.options.output_style != OutputStyleOption::Disabled {
                        crate::outln!(
                            "{}{}: {} (interrupted before completing any runs)",
                            "Benchmark ".bold(),
                            (runner.display_number + 1).to_string().bold(),
                            colors::cyan(runner.command.get_name_with_unused_parameters())
                        );
                    }
                    continue;
                }
                let res = runner.finish(true)?;
                self.results.push(res);

                let intermediate_results: Vec<_> = if self.options.filter_failed {
                    self.results
                        .iter()
                        .filter(|r| !r.has_failure())
                        .cloned()
                        .collect()
                } else {
                    self.results.clone()
                };
                let reference = self.get_reference_result(&intermediate_results);
                self.export_manager
                    .write_results(&intermediate_results, reference, true)?;
            }
        } else if let Some(spec) = self.options.optimize.as_ref() {
            // `--optimize`: for each command, search the parameter instead of
            // benchmarking a fixed list of values
            let options = self.options;
            let metric = options.metric;
            let results = &mut self.results;
            for &(number, base) in &commands_to_run {
                if crate::util::interrupt::interrupted() {
                    break;
                }
                let points: std::cell::RefCell<Vec<PointData>> = Default::default();
                // Confirmation runs feed the pooled statistics of the report; they are
                // not entries of the results (one per evaluated value)
                let confirming = std::cell::Cell::new(false);
                let mut evaluate = |x: i64| -> Result<Option<f64>> {
                    if crate::util::interrupt::interrupted() {
                        return Ok(None);
                    }
                    let cmd = base.with_parameter(&spec.var, ParameterValue::Text(spec.text(x)));
                    let Some(res) =
                        Benchmark::new(number, results.len(), &cmd, options, executor_for(number))
                            .with_baseline(baseline_for(number))
                            .run()?
                    else {
                        return Ok(None);
                    };
                    let value = res
                        .primary_mean(metric)
                        .filter(|v| v.is_finite() && *v != 0.0);
                    let Some(value) = value else {
                        anyhow::bail!(
                            "'--optimize': the {} of '{}' is {}, so the search cannot compare it. \
                                 Is the metric available (e.g. readable energy counters)?",
                            metric.name(),
                            cmd.get_command_line(),
                            res.primary_mean(metric)
                                .map_or("missing".to_string(), |v| v.to_string())
                        );
                    };
                    let samples = res.primary_samples(metric);
                    let se = match (&samples, res.primary_stddev(metric)) {
                        (Some(s), Some(sd)) if !s.is_empty() => sd / (s.len() as f64).sqrt(),
                        _ => 0.0,
                    };
                    points.borrow_mut().push(PointData {
                        x,
                        value,
                        se,
                        samples,
                        wall_mean: res.mean,
                        command: cmd.get_command_line(),
                    });
                    if confirming.get() {
                        return Ok(Some(value));
                    }
                    results.push(res);

                    // Export after each evaluation, as for ordinary benchmarks
                    let intermediate: Vec<_> = if options.filter_failed {
                        results
                            .iter()
                            .filter(|r| !r.has_failure())
                            .cloned()
                            .collect()
                    } else {
                        results.clone()
                    };
                    self.export_manager
                        .write_results(&intermediate, None, true)?;
                    Ok(Some(value))
                };
                let mut outcome =
                    optimize::golden_section_min(spec.min, spec.max, spec.budget, &mut evaluate)?;

                // Noise: run the best value and its close competitors once more
                // and pool the samples before declaring a best value
                let mut reevaluated = Vec::new();
                if !outcome.interrupted && spec.confirm > 0 {
                    let candidates =
                        optimize::confirmation_candidates(&points.borrow(), spec.confirm);
                    confirming.set(true);
                    for x in candidates {
                        if evaluate(x)?.is_none() {
                            outcome.interrupted = true;
                            break;
                        }
                        reevaluated.push(x);
                    }
                }
                let points = optimize::merge_repeated(points.into_inner());
                let fixed = base
                    .get_parameters()
                    .iter()
                    .map(|(name, value)| (name.to_string(), value.to_string()))
                    .collect();
                if let Some(report) =
                    optimize::analyze(spec, metric, &outcome, &points, &reevaluated, fixed)
                {
                    if options.output_style != OutputStyleOption::Disabled {
                        crate::outln!("\n{}", optimize::summary(spec, metric, &report, &points));
                    }
                    if report.unimodal_warning {
                        eprintln!("{}", optimize::unimodal_warning_text(&spec.var));
                    }
                    self.optimize_reports.push(report);
                }
            }
            self.export_manager
                .set_optimize(optimize_json(&self.optimize_reports));
        } else {
            for (number, cmd) in commands_to_run {
                if crate::util::interrupt::interrupted() {
                    break;
                }
                if let Some(res) = Benchmark::new(
                    number,
                    number + display_offset,
                    cmd,
                    self.options,
                    executor_for(number),
                )
                .with_baseline(baseline_for(number))
                .run()?
                {
                    self.results.push(res);

                    // We export results after each individual benchmark, because
                    // we would risk losing them if a later benchmark fails.
                    let intermediate_results: Vec<_> = if self.options.filter_failed {
                        self.results
                            .iter()
                            .filter(|r| !r.has_failure())
                            .cloned()
                            .collect()
                    } else {
                        self.results.clone()
                    };
                    let reference = self.get_reference_result(&intermediate_results);
                    self.export_manager
                        .write_results(&intermediate_results, reference, true)?;
                }
            }
        }

        Ok(())
    }

    fn get_reference_result<'b>(
        &self,
        results_slice: &'b [BenchmarkResult],
    ) -> Option<&'b BenchmarkResult> {
        self.reference_index
            .and_then(|idx| self.results.get(idx))
            .and_then(|ref_res| {
                results_slice
                    .iter()
                    .find(|r| r.command == ref_res.command && r.parameters == ref_res.parameters)
            })
    }

    /// `--delta-table`: compare every measured dimension with the reference.
    pub fn print_delta_table(&self) {
        if self.options.output_style == OutputStyleOption::Disabled {
            return;
        }
        let results: Vec<BenchmarkResult> = self
            .results
            .iter()
            .filter(|r| !(self.options.filter_failed && r.has_failure()))
            .cloned()
            .collect();
        let reference = self.get_reference_result(&results);
        let table = crate::delta_table::terminal(&results, reference);
        if !table.is_empty() {
            crate::out!("{table}");
        }
    }

    /// `--raw-ratios`: one relative value per line on stdout, nothing else.
    pub fn print_raw_ratios(&self) {
        if !self.options.raw_ratios {
            return;
        }
        let results: Vec<BenchmarkResult> = self
            .results
            .iter()
            .filter(|r| !(self.options.filter_failed && r.has_failure()))
            .cloned()
            .collect();
        let metric = self.options.metric;
        let lines = raw_ratio_lines(&results, self.get_reference_result(&results), metric);
        for line in lines {
            crate::outln!("{line}");
        }
    }

    /// `--pareto`, `--constraint`, `--objective`: the frontier of the measured commands.
    pub fn print_pareto(&self) {
        let Some(config) = &self.options.pareto else {
            return;
        };
        if self.options.output_style == OutputStyleOption::Disabled {
            return;
        }
        let results: Vec<BenchmarkResult> = self
            .results
            .iter()
            .filter(|r| !(self.options.filter_failed && r.has_failure()))
            .cloned()
            .collect();
        let analysis = crate::pareto::analyze(config, &results);
        let text = crate::pareto::terminal(config, &analysis, &results);
        if !text.is_empty() {
            crate::out!("{text}");
        }
    }

    pub fn print_relative_speed_comparison(&self) {
        if self.options.output_style == OutputStyleOption::Disabled {
            return;
        }

        // Keep track of each result's index in `self.results`, so that the
        // reference can be found again after filtering.
        let results: Vec<(usize, &BenchmarkResult)> = self
            .results
            .iter()
            .enumerate()
            .filter(|(_, r)| !(self.options.filter_failed && r.has_failure()))
            .collect();

        if results.len() < 2 {
            return;
        }

        let results_slice: Vec<_> = results.iter().map(|(_, r)| (*r).clone()).collect();

        let metric = self.options.metric;
        // Use the `--reference` command's result if it is still present,
        // otherwise (no reference, or it was filtered out) the best one according to the active metric.
        let reference = self
            .get_reference_result(&results_slice)
            .unwrap_or_else(|| relative_speed::best_of(&results_slice, metric));

        let interrupted = crate::util::interrupt::interrupted();
        let total_live_commands =
            self.options.reference_command.iter().count() + self.commands.iter().count();
        let completed_live_commands = self.results.len().saturating_sub(self.imported_count);
        let unbenchmarked = total_live_commands.saturating_sub(completed_live_commands);

        let summary_title = if interrupted {
            if unbenchmarked > 0 {
                let plural = if unbenchmarked == 1 {
                    "command"
                } else {
                    "commands"
                };
                format!("Summary (interrupted — {unbenchmarked} {plural} not benchmarked)")
            } else {
                "Summary (interrupted)".to_string()
            }
        } else {
            "Summary".to_string()
        };

        if let Some(annotated_results) = relative_speed::compute_with_check_from_reference(
            &results_slice,
            reference,
            self.options.sort_order_speed_comparison,
            metric,
        ) {
            match self.options.sort_order_speed_comparison {
                SortOrder::MeanTime => {
                    crate::outln!("{}", summary_title.bold());

                    // `compute_with_check_from_reference` marks exactly one entry
                    let Some(reference) = annotated_results.iter().find(|r| r.is_reference) else {
                        return;
                    };
                    let assessments = family_assessments(reference.result, &annotated_results);
                    let others = annotated_results.iter().filter(|r| !r.is_reference);

                    let verb = metric.verb();
                    crate::outln!(
                        "  {} {verb}",
                        colors::cyan(&reference.result.command_with_unused_parameters)
                    );

                    // All absolute numbers use one unit (the one of the largest mean,
                    // unless --time-unit is given), so the lines are directly comparable.
                    let largest_mean = annotated_results
                        .iter()
                        .map(|r| r.result.mean)
                        .fold(0.0, f64::max);
                    let (_, unit) = format_duration_unit(largest_mean, self.options.time_unit);

                    for item in others {
                        if item.result.timed_out {
                            let timeout_sec = item.result.timeout.unwrap_or(item.result.mean);
                            let timeout_str = format_duration(timeout_sec, self.options.time_unit);
                            crate::outln!(
                                "    {} {}",
                                colors::yellow(format!("> {timeout_str} (timeout)")),
                                colors::magenta(&item.result.command_with_unused_parameters)
                            );
                            continue;
                        }

                        let absolute = match metric {
                            Metric::Custom(_) | Metric::Instructions | Metric::Cycles => {
                                custom_difference(reference.result, item.result, metric)
                            }
                            _ => absolute_difference(reference.result, item.result, unit),
                        };
                        let stddev = if let Some(stddev) = item.relative_speed_stddev {
                            format!(" ± {}", colors::green(format!("{stddev:.2}")))
                        } else if let Some((lo, hi)) = item.relative_speed_ci {
                            format!(" [{lo:.2}, {hi:.2}]")
                        } else {
                            "".into()
                        };
                        let (better_word, worse_word) = metric.comparison_words();
                        let comparator = match item.relative_ordering {
                            Ordering::Less => format!(
                                "{}{} times {worse_word} than",
                                colors::green(format!("{:8.2}", item.relative_speed)).bold(),
                                stddev
                            ),
                            Ordering::Greater => format!(
                                "{}{} times {better_word} than",
                                colors::green(format!("{:8.2}", item.relative_speed)).bold(),
                                stddev
                            ),
                            Ordering::Equal => {
                                let ratio_str =
                                    colors::green(format!("{:.2}", item.relative_speed)).bold();
                                match metric {
                                    Metric::Wall | Metric::Cpu | Metric::User | Metric::System => {
                                        format!("    As fast ({ratio_str}{stddev}) as")
                                    }
                                    Metric::Energy => {
                                        format!("    the same energy ({ratio_str}{stddev}) as")
                                    }
                                    Metric::Memory => {
                                        format!("    the same memory ({ratio_str}{stddev}) as")
                                    }
                                    Metric::Instructions => {
                                        format!(
                                            "    the same instructions ({ratio_str}{stddev}) as"
                                        )
                                    }
                                    Metric::Cycles => {
                                        format!("    the same cycles ({ratio_str}{stddev}) as")
                                    }
                                    Metric::Custom(_) => {
                                        format!("    the same value ({ratio_str}{stddev}) as")
                                    }
                                }
                            }
                        };
                        crate::outln!(
                            "{} {} {}{}",
                            comparator,
                            colors::magenta(&item.result.command_with_unused_parameters),
                            absolute.dimmed(),
                            significance_suffix(&assessments, item.result)
                        );

                        if self.options.deep_stats {
                            if let (Some(ref_times), Some(item_times)) =
                                (&reference.result.times, &item.result.times)
                            {
                                let var_base_ref = reference
                                    .result
                                    .baseline
                                    .as_ref()
                                    .and_then(|sub| {
                                        sub.stddev.map(|sd| sd.powi(2) / sub.runs.max(1) as f64)
                                    })
                                    .unwrap_or(0.0);
                                let var_base_item = item
                                    .result
                                    .baseline
                                    .as_ref()
                                    .and_then(|sub| {
                                        sub.stddev.map(|sd| sd.powi(2) / sub.runs.max(1) as f64)
                                    })
                                    .unwrap_or(0.0);
                                match crate::stats::deep::compare_samples_with_baseline_variance(
                                    ref_times,
                                    item_times,
                                    var_base_ref,
                                    var_base_item,
                                ) {
                                    Some(cmp) => {
                                        let alpha =
                                            crate::stats::settings::current().significance_level;
                                        // the stricter of 1% and the chosen level is
                                        // reported when it is met
                                        let strict = alpha.min(0.01);
                                        let sig_str = if cmp.p_value < strict {
                                            colors::cyan(format!(
                                                "statistically significant (p < {strict})"
                                            ))
                                        } else if cmp.p_value < alpha {
                                            colors::cyan(format!(
                                                "statistically significant (p < {alpha})"
                                            ))
                                        } else {
                                            format!(
                                                "no statistically significant difference (p ≥ {alpha})"
                                            )
                                            .dimmed()
                                        };
                                        crate::outln!(
                                            "      [Bootstrap t-test: t = {:.2}, p = {:.4} -> {}]",
                                            cmp.t_statistic,
                                            cmp.p_value,
                                            sig_str
                                        );
                                    }
                                    None => {
                                        crate::outln!(
                                            "      {}",
                                            "[Bootstrap t-test: not applicable (zero variance or insufficient samples)]"
                                                .dimmed()
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                SortOrder::Command => {
                    let show_reference_annotations = self.options.reference_command.is_some();
                    let reference_command = if show_reference_annotations {
                        annotated_results
                            .iter()
                            .find(|r| r.is_reference)
                            .map(|r| r.result.command_with_unused_parameters.as_str())
                    } else {
                        None
                    };

                    let interrupted_suffix = if interrupted {
                        if unbenchmarked > 0 {
                            let plural = if unbenchmarked == 1 {
                                "command"
                            } else {
                                "commands"
                            };
                            format!(" (interrupted — {unbenchmarked} {plural} not benchmarked)")
                        } else {
                            " (interrupted)".to_string()
                        }
                    } else {
                        String::new()
                    };

                    if let Some(ref_cmd) = reference_command {
                        crate::outln!(
                            "{} (reference: {}){}",
                            "Relative speed comparison".bold(),
                            colors::cyan(ref_cmd),
                            interrupted_suffix
                        );
                    } else {
                        crate::outln!(
                            "{}{}",
                            "Relative speed comparison".bold(),
                            interrupted_suffix
                        );
                    }

                    let Some(reference_result) = annotated_results
                        .iter()
                        .find(|r| r.is_reference)
                        .map(|r| r.result)
                    else {
                        return;
                    };
                    let assessments = family_assessments(reference_result, &annotated_results);

                    let max_cmd_len = annotated_results
                        .iter()
                        .map(|r| r.result.command_with_unused_parameters.len())
                        .max()
                        .unwrap_or(0);

                    for item in annotated_results {
                        if item.result.timed_out {
                            let timeout_sec = item.result.timeout.unwrap_or(item.result.mean);
                            let timeout_str = format_duration(timeout_sec, self.options.time_unit);
                            crate::outln!(
                                "  {}  {:<width$}",
                                colors::yellow(format!(
                                    "{:>21}",
                                    format!("> {timeout_str} (timeout)")
                                )),
                                item.result.command_with_unused_parameters,
                                width = max_cmd_len,
                            );
                            continue;
                        }

                        let stddev_suffix = if item.is_reference {
                            "        ".into()
                        } else if let Some(stddev) = item.relative_speed_stddev {
                            format!(" ± {}", colors::green(format!("{stddev:5.2}")))
                        } else {
                            "        ".into()
                        };

                        let reference_annotation = if show_reference_annotations {
                            if item.is_reference {
                                format!("  {}", "(reference)".dimmed())
                            } else {
                                let ref_cmd = reference_command.unwrap();
                                let (better_word, worse_word) = metric.comparison_words();
                                let equal_word = metric.equal_word();
                                let desc = match item.relative_ordering {
                                    Ordering::Less => format!("{better_word} than {ref_cmd}"),
                                    Ordering::Greater => format!("{worse_word} than {ref_cmd}"),
                                    Ordering::Equal => format!("{equal_word} {ref_cmd}"),
                                };
                                format!("  {desc}")
                            }
                        } else {
                            String::new()
                        };

                        crate::outln!(
                            "  {}{}  {:<width$}{}{}",
                            colors::green(format!("{:10.2}", item.relative_speed)).bold(),
                            stddev_suffix,
                            item.result.command_with_unused_parameters,
                            reference_annotation,
                            if item.is_reference {
                                String::new()
                            } else {
                                significance_suffix(&assessments, item.result)
                            },
                            width = max_cmd_len,
                        );
                    }
                }
            }
        } else {
            eprintln!(
                "{}: The benchmark comparison could not be computed as some benchmark times are zero. \
                 This could be caused by background interference during the initial calibration phase \
                 of perfratio, in combination with very fast commands (faster than a few milliseconds). \
                 Try to re-run the benchmark on a quiet system. If you did not do so already, try the \
                 --shell=none/-N option. If it does not help either, you command is most likely too fast \
                 to be accurately benchmarked by perfratio.",
                 colors::red("Note").bold()
            );
        }
    }

    pub fn final_export(&self) -> Result<()> {
        let results: Vec<_> = if self.options.filter_failed {
            self.results
                .iter()
                .filter(|r| !r.has_failure())
                .cloned()
                .collect()
        } else {
            self.results.clone()
        };
        let reference = self.get_reference_result(&results);
        self.export_manager
            .write_results(&results, reference, false)
    }
}

/// The lines of `--raw-ratios`: mean of the metric divided by the mean of the reference
/// (or the best result), in result order, with six decimals and a `.` as decimal point.
pub fn raw_ratio_lines(
    results: &[BenchmarkResult],
    reference: Option<&BenchmarkResult>,
    metric: Metric,
) -> Vec<String> {
    if results.is_empty() {
        return Vec::new();
    }
    let reference = reference.unwrap_or_else(|| relative_speed::best_of(results, metric));
    let Some(reference_mean) = reference.primary_center(metric).filter(|m| *m != 0.0) else {
        return results.iter().map(|_| "nan".to_string()).collect();
    };
    results
        .iter()
        .map(|r| match r.primary_center(metric) {
            Some(mean) if !r.timed_out => format!("{:.6}", mean / reference_mean),
            _ => "nan".to_string(),
        })
        .collect()
}

#[cfg(feature = "cli")]
#[cfg(test)]
fn generate_results(args: &[&'static str]) -> Result<Vec<BenchmarkResult>> {
    use crate::cli::get_cli_arguments;

    let args = ["perfratio", "--debug-mode", "--style=none"]
        .iter()
        .chain(args);
    let cli_arguments = get_cli_arguments(args);
    let mut options = Options::from_cli_arguments(&cli_arguments)?;

    assert_eq!(options.executor_kind, ExecutorKind::Mock(None));

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

    options.validate_against_command_list(&commands)?;

    let mut scheduler = Scheduler::new(&commands, &options, &export_manager);

    scheduler.run_benchmarks()?;
    // Run ids come from a process-wide counter that parallel tests share
    for result in &mut scheduler.results {
        result.run_ids = None;
    }
    Ok(scheduler.results)
}

#[cfg(feature = "cli")]
#[test]
fn scheduler_basic() -> Result<()> {
    insta::assert_yaml_snapshot!(generate_results(&["--runs=2", "sleep 0.123", "sleep 0.456"])?, @r#"
    - command: sleep 0.123
      mean: 0.123
      stddev: 0
      median: 0.123
      percentiles:
        p05: 0.123
        p25: 0.123
        p75: 0.123
        p95: 0.123
      geometric_mean: 0.123
      user: 0
      system: 0
      cpu_percent: 0
      min: 0.123
      max: 0.123
      times:
        - 0.123
        - 0.123
      user_times:
        - 0
        - 0
      system_times:
        - 0
        - 0
      memory_usage_byte:
        - 0
        - 0
      exit_codes:
        - 0
        - 0
      quality:
        grade: unreliable
        checks:
          - id: sample_sufficiency
            metric: wall
            status: fail
            detail: 2 runs; at least 3 are needed to estimate the spread
          - id: statistical_precision
            metric: wall
            status: pass
            detail: ±0.0% (95% CI of the mean)
          - id: outlier_burden
            metric: time
            status: unavailable
            detail: fewer than 4 runs
          - id: trend
            metric: time
            status: unavailable
            detail: needs at least 8 runs
          - id: interference
            metric: time
            status: unavailable
            detail: needs at least 8 runs
          - id: counter_coverage
            metric: counters
            status: not_applicable
          - id: energy_coverage
            metric: energy
            status: not_applicable
          - id: workload_validation
            status: not_applicable
    - command: sleep 0.456
      mean: 0.456
      stddev: 0
      median: 0.456
      percentiles:
        p05: 0.456
        p25: 0.456
        p75: 0.456
        p95: 0.456
      geometric_mean: 0.456
      user: 0
      system: 0
      cpu_percent: 0
      min: 0.456
      max: 0.456
      times:
        - 0.456
        - 0.456
      user_times:
        - 0
        - 0
      system_times:
        - 0
        - 0
      memory_usage_byte:
        - 0
        - 0
      exit_codes:
        - 0
        - 0
      quality:
        grade: unreliable
        checks:
          - id: sample_sufficiency
            metric: wall
            status: fail
            detail: 2 runs; at least 3 are needed to estimate the spread
          - id: statistical_precision
            metric: wall
            status: pass
            detail: ±0.0% (95% CI of the mean)
          - id: outlier_burden
            metric: time
            status: unavailable
            detail: fewer than 4 runs
          - id: trend
            metric: time
            status: unavailable
            detail: needs at least 8 runs
          - id: interference
            metric: time
            status: unavailable
            detail: needs at least 8 runs
          - id: counter_coverage
            metric: counters
            status: not_applicable
          - id: energy_coverage
            metric: energy
            status: not_applicable
          - id: workload_validation
            status: not_applicable
    "#);

    Ok(())
}

#[cfg(feature = "cli")]
#[test]
fn scheduler_round_robin() -> Result<()> {
    insta::assert_yaml_snapshot!(generate_results(&["--schedule=round-robin", "--runs=2", "sleep 0.123", "sleep 0.456"])?, @r#"
    - command: sleep 0.123
      mean: 0.123
      stddev: 0
      median: 0.123
      percentiles:
        p05: 0.123
        p25: 0.123
        p75: 0.123
        p95: 0.123
      geometric_mean: 0.123
      user: 0
      system: 0
      cpu_percent: 0
      min: 0.123
      max: 0.123
      times:
        - 0.123
        - 0.123
      user_times:
        - 0
        - 0
      system_times:
        - 0
        - 0
      memory_usage_byte:
        - 0
        - 0
      exit_codes:
        - 0
        - 0
      quality:
        grade: unreliable
        checks:
          - id: sample_sufficiency
            metric: wall
            status: fail
            detail: 2 runs; at least 3 are needed to estimate the spread
          - id: statistical_precision
            metric: wall
            status: pass
            detail: ±0.0% (95% CI of the mean)
          - id: outlier_burden
            metric: time
            status: unavailable
            detail: fewer than 4 runs
          - id: trend
            metric: time
            status: unavailable
            detail: needs at least 8 runs
          - id: interference
            metric: time
            status: unavailable
            detail: needs at least 8 runs
          - id: counter_coverage
            metric: counters
            status: not_applicable
          - id: energy_coverage
            metric: energy
            status: not_applicable
          - id: workload_validation
            status: not_applicable
    - command: sleep 0.456
      mean: 0.456
      stddev: 0
      median: 0.456
      percentiles:
        p05: 0.456
        p25: 0.456
        p75: 0.456
        p95: 0.456
      geometric_mean: 0.456
      user: 0
      system: 0
      cpu_percent: 0
      min: 0.456
      max: 0.456
      times:
        - 0.456
        - 0.456
      user_times:
        - 0
        - 0
      system_times:
        - 0
        - 0
      memory_usage_byte:
        - 0
        - 0
      exit_codes:
        - 0
        - 0
      quality:
        grade: unreliable
        checks:
          - id: sample_sufficiency
            metric: wall
            status: fail
            detail: 2 runs; at least 3 are needed to estimate the spread
          - id: statistical_precision
            metric: wall
            status: pass
            detail: ±0.0% (95% CI of the mean)
          - id: outlier_burden
            metric: time
            status: unavailable
            detail: fewer than 4 runs
          - id: trend
            metric: time
            status: unavailable
            detail: needs at least 8 runs
          - id: interference
            metric: time
            status: unavailable
            detail: needs at least 8 runs
          - id: counter_coverage
            metric: counters
            status: not_applicable
          - id: energy_coverage
            metric: energy
            status: not_applicable
          - id: workload_validation
            status: not_applicable
    "#);

    Ok(())
}

/// The significance of every result against the reference, with `--correction`
/// applied over the family. A line names method, family and size when a
/// correction is active.
fn family_assessments(
    reference: &BenchmarkResult,
    annotated: &[relative_speed::BenchmarkResultWithRelativeSpeed],
) -> crate::stats::significance::FamilyAssessment {
    let settings = crate::stats::settings::current();
    let results: Vec<&BenchmarkResult> = annotated.iter().map(|r| r.result).collect();
    let assessments = crate::stats::significance::assess_family(reference, &results, &settings);
    if !settings.correction.is_none() {
        crate::outln!(
            "  {}",
            format!(
                "Multiple-comparison correction: {} over {} comparisons ({} family), alpha = {}",
                settings.correction.name(),
                assessments.family_size,
                settings.family.name(),
                settings.significance_level
            )
            .dimmed()
        );
    }
    assessments
}

/// "(12.3 ms, +4.5 ms)": the mean of `item` and its difference to the
/// reference, plus energy when both results have it. Positive differences mean
/// `item` is slower (or uses more energy) than the reference.
/// Significance marker of `item` against `reference` for the summary
/// (`  [+] significant speedup (p < 0.001)`), empty if it cannot be assessed.
/// Symbols are used only where colors are (not for `NO_COLOR`, pipes or dumb terminals).
fn significance_suffix(
    assessments: &crate::stats::significance::FamilyAssessment,
    item: &BenchmarkResult,
) -> String {
    let fancy = colored::control::SHOULD_COLORIZE.should_colorize();
    match assessments.get(item) {
        Some(assessment) => {
            let text = assessment.label(fancy);
            let text = match assessment.significance {
                Significance::SignificantSpeedup => colors::green(text),
                Significance::SignificantRegression => colors::red(text),
                Significance::Noise => text.dimmed(),
            };
            format!("  {text}")
        }
        None => String::new(),
    }
}

/// Value and difference of a `--metric custom:<name>` metric, which has no unit.
fn custom_difference(
    reference: &BenchmarkResult,
    item: &BenchmarkResult,
    metric: Metric,
) -> String {
    match (
        item.primary_center(metric),
        reference.primary_center(metric),
    ) {
        (Some(value), Some(base)) => {
            let diff = value - base;
            let sign = if diff >= 0.0 { '+' } else { '−' };
            if matches!(metric, Metric::Instructions | Metric::Cycles) {
                let (v, d) = (
                    crate::benchmark::format_count(value),
                    crate::benchmark::format_count(diff.abs()),
                );
                return format!("({v}, {sign}{d})");
            }
            format!("({value:.3}, {sign}{:.3})", diff.abs())
        }
        _ => String::new(),
    }
}

fn absolute_difference(
    reference: &BenchmarkResult,
    item: &BenchmarkResult,
    unit: crate::util::units::Unit,
) -> String {
    let signed = |value: f64, formatted: String| {
        if value >= 0.0 {
            format!("+{formatted}")
        } else {
            format!("−{formatted}")
        }
    };
    // the central value of `--estimator` (the mean unless set)
    let center = |r: &BenchmarkResult| r.primary_center(Metric::Wall).unwrap_or(r.mean);
    let (item_value, reference_value) = (center(item), center(reference));
    let time_diff = item_value - reference_value;
    let mut parts = vec![
        format_duration(item_value, Some(unit)),
        signed(time_diff, format_duration(time_diff.abs(), Some(unit))),
    ];
    if let (Some(e_ref), Some(e_item)) = (reference.mean_energy_joules, item.mean_energy_joules) {
        let energy_diff = e_item - e_ref;
        parts.push(format!(
            "{e_item:.3} J, {}",
            signed(energy_diff, format!("{:.3} J", energy_diff.abs()))
        ));
    }
    format!("({})", parts.join(", "))
}

#[cfg(test)]
mod absolute_difference_tests {
    use super::*;
    use crate::util::units::Unit;

    fn result(mean: f64, energy: Option<f64>) -> BenchmarkResult {
        BenchmarkResult {
            mean,
            mean_energy_joules: energy,
            ..Default::default()
        }
    }

    #[test]
    fn slower_item_has_positive_difference() {
        assert_eq!(
            absolute_difference(
                &result(0.0557, None),
                &result(0.1046, None),
                Unit::MilliSecond
            ),
            "(104.6 ms, +48.9 ms)"
        );
    }

    #[test]
    fn faster_item_has_negative_difference() {
        assert_eq!(
            absolute_difference(&result(2.0, None), &result(1.5, None), Unit::Second),
            "(1.500 s, −0.500 s)"
        );
    }

    #[test]
    fn energy_is_included_when_both_have_it() {
        assert_eq!(
            absolute_difference(
                &result(1.0, Some(0.66)),
                &result(2.0, Some(1.21)),
                Unit::Second
            ),
            "(2.000 s, +1.000 s, 1.210 J, +0.550 J)"
        );
        assert_eq!(
            absolute_difference(&result(1.0, Some(0.66)), &result(2.0, None), Unit::Second),
            "(2.000 s, +1.000 s)"
        );
    }
}
