//! `--jobs N`: a pool of workers that execute independent runs at the same
//! time (#306). Unlike `--concurrency`, which measures one batch of N
//! instances, every run is timed on its own.
//!
//! [`Dispatcher`] holds the scheduling policies and no process or thread, so
//! it is tested with simulated workers. [`run_pool`] drives real workers.

use std::cell::{Cell, RefCell};
use std::process::ExitStatus;
use std::sync::{mpsc, Arc};
use std::time::Instant;

use anyhow::{anyhow, Result};

use super::executor::{BenchmarkIteration, Executor};
use super::timing_result::TimingResult;
use super::{run_intermediate, BenchmarkRunner, CONCLUSION_ERROR, PREPARATION_ERROR};
use crate::command::Command;
use crate::options::{CommandOutputPolicy, Options};
use crate::util::cpu_partition::{pin_current_thread, Partition};

thread_local! {
    /// Slot of the worker running on this thread (`None` outside a pool)
    static WORKER: Cell<Option<usize>> = const { Cell::new(None) };
    /// CPUs of the worker running on this thread (`--pin-workers`)
    static WORKER_CPUS: RefCell<Option<Arc<[usize]>>> = const { RefCell::new(None) };
}

/// CPUs that processes started by this worker are pinned to, if any.
/// They replace `--affinity` for the benchmarked command and for
/// `--prepare` / `--conclude`.
pub fn current_cpus() -> Option<Arc<[usize]>> {
    WORKER_CPUS.with(|cpus| cpus.borrow().clone())
}

/// Worker slot (0-based) of the calling thread, if it belongs to a pool.
/// Exported to the benchmarked commands as `PERFRATIO_WORKER`.
pub fn current_worker() -> Option<usize> {
    WORKER.with(Cell::get)
}

/// How the pool picks the next run (`--schedule saturate|exclusive|fair`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Keep every worker busy: the next run is always started
    Saturate,
    /// A command never runs concurrently with itself
    Exclusive,
    /// Lockstep rounds: a command only starts its next run when no other
    /// command has fewer runs started
    Fair,
}

impl Policy {
    pub fn name(self) -> &'static str {
        match self {
            Policy::Saturate => "saturate",
            Policy::Exclusive => "exclusive",
            Policy::Fair => "fair",
        }
    }
}

/// Chooses which command receives the next run.
#[derive(Debug, Clone)]
pub struct Dispatcher {
    policy: Policy,
    /// Runs started so far, per command (including runs already finished)
    issued: Vec<u64>,
    /// Runs currently executing, per command
    in_flight: Vec<usize>,
}

impl Dispatcher {
    /// `issued`: runs already started per command (the initial measurement).
    pub fn new(policy: Policy, issued: Vec<u64>) -> Self {
        let in_flight = vec![0; issued.len()];
        Dispatcher {
            policy,
            issued,
            in_flight,
        }
    }

    pub fn issued(&self, command: usize) -> u64 {
        self.issued[command]
    }

    pub fn in_flight(&self, command: usize) -> usize {
        self.in_flight[command]
    }

    /// The command that gets the next run among those with `wants[c]`, or
    /// `None` if there is none right now (a finishing run can change that).
    /// The pick counts as started.
    pub fn next(&mut self, wants: &[bool]) -> Option<usize> {
        let eligible = |c: usize| {
            wants[c]
                && match self.policy {
                    Policy::Exclusive => self.in_flight[c] == 0,
                    Policy::Saturate | Policy::Fair => true,
                }
        };
        // Fair: only the commands with the fewest started runs are eligible.
        // Saturate and exclusive also prefer them, which interleaves the
        // commands instead of finishing one after the other.
        let fewest = (0..wants.len())
            .filter(|&c| wants[c])
            .map(|c| self.issued[c])
            .min()?;
        let pick = (0..wants.len())
            .filter(|&c| eligible(c) && (self.policy != Policy::Fair || self.issued[c] == fewest))
            .min_by_key(|&c| (self.issued[c], c))?;
        self.issued[pick] += 1;
        self.in_flight[pick] += 1;
        Some(pick)
    }

    /// A run of `command` finished (or was abandoned).
    pub fn finished(&mut self, command: usize) {
        self.in_flight[command] -= 1;
    }
}

/// Everything a worker needs to execute a run of one command. Owned copies,
/// so that the workers do not borrow the runners the main thread updates.
struct RunContext<'a> {
    executor: &'a dyn Executor,
    command: &'a Command<'a>,
    options: &'a Options,
    output_policy: &'a CommandOutputPolicy,
    preparation: Option<Command<'a>>,
    conclusion: Option<Command<'a>>,
}

enum Outcome {
    TimedOut,
    Run(Box<TimingResult>, ExitStatus),
}

struct Finished {
    command: usize,
    iteration: u64,
    slot: usize,
    outcome: Result<Outcome>,
}

/// One run on a worker: `--prepare`, the command, `--conclude`.
fn execute(ctx: &RunContext<'_>, iteration: u64) -> Result<Outcome> {
    let it = BenchmarkIteration::Benchmark(iteration);
    if let Some(prepare) = &ctx.preparation {
        run_intermediate(
            ctx.executor,
            ctx.output_policy,
            ctx.options,
            prepare,
            it,
            "preparation",
            PREPARATION_ERROR,
        )?;
    }
    let (res, status) =
        ctx.executor
            .run_command_and_measure(ctx.command, it, None, ctx.output_policy)?;
    if res.timed_out {
        return Ok(Outcome::TimedOut);
    }
    if let Some(conclude) = &ctx.conclusion {
        run_intermediate(
            ctx.executor,
            ctx.output_policy,
            ctx.options,
            conclude,
            it,
            "conclusion",
            CONCLUSION_ERROR,
        )?;
    }
    Ok(Outcome::Run(Box::new(res), status))
}

/// Execute the remaining timed runs of `runners` on `jobs` workers.
///
/// The initial measurement (run 0) of every runner must be done. With
/// `adaptive` (`--target-precision`, `--duration`) a command gets runs while
/// `needs_more_runs` says so, which can overshoot by up to `jobs - 1` runs
/// that were already started. Otherwise every command gets `count` runs in
/// total. `on_run` is called after each recorded run.
pub fn run_pool(
    runners: &mut [BenchmarkRunner<'_>],
    policy: Policy,
    jobs: usize,
    adaptive: bool,
    budget: f64,
    partition: Option<&Partition>,
    mut on_run: impl FnMut(&BenchmarkRunner<'_>),
) -> Result<()> {
    let started = Instant::now();
    // No more workers than runs: a huge `--jobs` must not start thousands of threads
    let workers = if adaptive {
        jobs
    } else {
        let planned: u64 = runners.iter().map(|r| r.count.saturating_sub(1)).sum();
        jobs.min(usize::try_from(planned).unwrap_or(usize::MAX))
            .max(1)
    };
    let contexts: Vec<RunContext<'_>> = runners
        .iter()
        .map(|r| RunContext {
            executor: r.executor,
            command: r.command,
            options: r.options,
            output_policy: r.output_policy,
            preparation: r.preparation_command.clone(),
            conclusion: r.conclusion_command.clone(),
        })
        .collect();
    let mut dispatcher = Dispatcher::new(policy, runners.iter().map(|_| 1).collect());
    let mut first_error: Option<anyhow::Error> = None;
    // perfratio's own threads stay on the reserved CPUs while the pool runs;
    // the workers (spawned below) inherit this on Linux and pin themselves
    let reserved: Option<Arc<[usize]>> = partition
        .map(|p| p.reserved.as_slice())
        .filter(|r| !r.is_empty())
        .map(Arc::from);
    let _main_pin = reserved.as_deref().and_then(pin_current_thread);

    std::thread::scope(|scope| {
        let (done_tx, done_rx) = mpsc::channel::<Finished>();
        let mut job_txs = Vec::with_capacity(workers);
        for slot in 0..workers {
            let (tx, rx) = mpsc::channel::<(usize, u64)>();
            let done_tx = done_tx.clone();
            let contexts = &contexts;
            let cpus: Option<Arc<[usize]>> =
                partition.map(|p| Arc::from(p.workers[slot].as_slice()));
            let reserved = reserved.clone();
            let spawned = std::thread::Builder::new().spawn_scoped(scope, move || {
                WORKER.with(|w| w.set(Some(slot)));
                WORKER_CPUS.with(|c| *c.borrow_mut() = cpus);
                // Processes started from this thread are pinned to the worker's
                // CPUs; the thread itself stays with perfratio
                let _pin = reserved.as_deref().and_then(pin_current_thread);
                while let Ok((command, iteration)) = rx.recv() {
                    let outcome = execute(&contexts[command], iteration);
                    let finished = Finished {
                        command,
                        iteration,
                        slot,
                        outcome,
                    };
                    if done_tx.send(finished).is_err() {
                        break;
                    }
                }
            });
            if let Err(e) = spawned {
                // Run with the workers that could be started
                if job_txs.is_empty() {
                    first_error = Some(anyhow!("could not start a benchmark worker: {e}"));
                }
                break;
            }
            job_txs.push(tx);
        }
        drop(done_tx);

        let mut idle: Vec<usize> = (0..job_txs.len()).rev().collect();
        let mut running = 0usize;
        loop {
            // Start as many runs as there are idle workers
            while first_error.is_none() && !crate::util::interrupt::interrupted() {
                let Some(&slot) = idle.last() else { break };
                let wants: Vec<bool> = runners
                    .iter()
                    .enumerate()
                    .map(|(c, runner)| wants_run(runner, &dispatcher, c, adaptive, started, budget))
                    .collect();
                let Some(command) = dispatcher.next(&wants) else {
                    break;
                };
                idle.pop();
                running += 1;
                let iteration = dispatcher.issued(command) - 1;
                if job_txs[slot].send((command, iteration)).is_err() {
                    first_error = Some(anyhow!("a benchmark worker stopped unexpectedly"));
                    break;
                }
            }
            if running == 0 {
                break;
            }
            let Ok(done) = done_rx.recv() else {
                first_error.get_or_insert_with(|| anyhow!("a benchmark worker panicked"));
                break;
            };
            running -= 1;
            idle.push(done.slot);
            dispatcher.finished(done.command);
            let runner = &mut runners[done.command];
            match done.outcome {
                Ok(Outcome::Run(res, status)) => {
                    runner.workers_seen.insert(done.slot);
                    runner.record_run(*res, status, None, done.iteration);
                    on_run(runner);
                }
                Ok(Outcome::TimedOut) => runner.timed_out = true,
                Err(e) if e.is::<crate::error::Interrupted>() => {}
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        // Closing the job channels ends the workers
        drop(job_txs);
    });

    if let Some(e) = first_error {
        return Err(e);
    }
    if policy == Policy::Fair && crate::util::interrupt::interrupted() {
        equalize_runs(runners);
    }
    Ok(())
}

/// Whether `runner` may start another run now.
fn wants_run(
    runner: &BenchmarkRunner<'_>,
    dispatcher: &Dispatcher,
    command: usize,
    adaptive: bool,
    started: Instant,
    budget: f64,
) -> bool {
    if runner.timed_out || runner.times_real.is_empty() {
        return false;
    }
    if adaptive {
        if let Some(max) = runner.options.run_bounds.max {
            let planned = (runner.times_real.len() + dispatcher.in_flight(command)) as u64;
            if planned >= max + runner.extra_runs() {
                return false;
            }
        }
        runner.needs_more_runs(started, budget)
    } else {
        dispatcher.issued(command) < runner.count
    }
}

/// `--schedule fair` after Ctrl-C: keep the same number of runs of every
/// command (runs that were in flight can leave some commands one ahead).
fn equalize_runs(runners: &mut [BenchmarkRunner<'_>]) {
    let Some(fewest) = runners
        .iter()
        .filter(|r| !r.timed_out && !r.times_real.is_empty())
        .map(|r| r.times_real.len())
        .min()
    else {
        return;
    };
    for runner in runners.iter_mut() {
        if !runner.timed_out && runner.times_real.len() > fewest {
            let keep: Vec<usize> = (0..fewest).collect();
            runner.retain_runs(&keep);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulate `jobs` workers; every started run finishes in start order.
    /// Returns the (command, in-flight-of-that-command-before-start) log.
    fn simulate(policy: Policy, jobs: usize, targets: &[u64]) -> Vec<(usize, usize, Vec<u64>)> {
        let mut d = Dispatcher::new(policy, vec![0; targets.len()]);
        let mut queue: std::collections::VecDeque<usize> = Default::default();
        let mut log = Vec::new();
        loop {
            while queue.len() < jobs {
                let wants: Vec<bool> = (0..targets.len())
                    .map(|c| d.issued(c) < targets[c])
                    .collect();
                let before: Vec<usize> = (0..targets.len()).map(|c| d.in_flight(c)).collect();
                let Some(c) = d.next(&wants) else { break };
                queue.push_back(c);
                log.push((
                    c,
                    before[c],
                    (0..targets.len()).map(|c| d.issued(c)).collect(),
                ));
            }
            match queue.pop_front() {
                Some(c) => d.finished(c),
                None => break,
            }
        }
        log
    }

    #[test]
    fn every_policy_starts_exactly_the_requested_runs() {
        for policy in [Policy::Saturate, Policy::Exclusive, Policy::Fair] {
            let log = simulate(policy, 3, &[5, 7, 2]);
            for (c, want) in [5usize, 7, 2].into_iter().enumerate() {
                assert_eq!(log.iter().filter(|e| e.0 == c).count(), want, "{policy:?}");
            }
        }
    }

    #[test]
    fn exclusive_never_overlaps_a_command_with_itself() {
        let log = simulate(Policy::Exclusive, 4, &[6, 6, 6]);
        assert!(log.iter().all(|e| e.1 == 0), "{log:?}");
        // One command alone is sequential
        let single = simulate(Policy::Exclusive, 4, &[5]);
        assert!(single.iter().all(|e| e.1 == 0));
    }

    #[test]
    fn saturate_runs_one_command_on_all_workers() {
        let log = simulate(Policy::Saturate, 4, &[8]);
        assert!(log.iter().any(|e| e.1 == 3), "{log:?}");
    }

    #[test]
    fn fair_keeps_started_counts_within_one() {
        let log = simulate(Policy::Fair, 3, &[9, 9, 9]);
        for (_, _, issued) in &log {
            let (lo, hi) = (issued.iter().min().unwrap(), issued.iter().max().unwrap());
            assert!(hi - lo <= 1, "{issued:?}");
        }
    }

    #[test]
    fn fair_with_unequal_targets_finishes_the_longer_one() {
        let log = simulate(Policy::Fair, 2, &[2, 5]);
        assert_eq!(log.iter().filter(|e| e.0 == 1).count(), 5);
    }

    #[test]
    fn next_is_none_when_nothing_is_wanted_or_all_are_busy() {
        let mut d = Dispatcher::new(Policy::Exclusive, vec![0, 0]);
        assert_eq!(d.next(&[false, false]), None);
        assert_eq!(d.next(&[true, false]), Some(0));
        assert_eq!(d.next(&[true, false]), None, "command 0 is busy");
        d.finished(0);
        assert_eq!(d.next(&[true, false]), Some(0));
    }

    #[test]
    fn worker_slot_is_only_set_inside_the_pool() {
        assert_eq!(current_worker(), None);
    }
}
