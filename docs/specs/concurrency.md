# Spec: `--concurrency <N>` concurrent load mode (#53)

**Status:** Shipped in 1.x (first iteration, schemaVersion 1.5; follow-ups under "As shipped"). See "As shipped" at the end for the differences from this proposal.

## Problem / Motivation
References: #53, `sharkdp/hyperfine#58`, `megafine`, #46 (per-run rusage), #240 (JSON contract).

Every run is sequential. The question "how does this CLI behave when 8 copies run at once?" is
answered today with `yes | head -n8 | parallel -j8 ./prog`, which measures `parallel` and
loses per-invocation statistics.

## Current State (audit of `master`)
Nothing exists: no `concurrency` identifier anywhere in `src`.
Relevant code to build on:
- `timer::execute_and_measure` (`src/timer/mod.rs`) runs and times one child: spawn, optional
  watchdog (`--timeout`, process group), `--until`, output capture, `TimerResult`.
- `timer::unix_timer::wait_with_rusage` uses `wait4(pid, ...)` for one pid, per-child CPU time and
  peak RSS (#46). Needs a `wait4(-1)`-style batch variant.
- `src/benchmark/executor.rs` and `BenchmarkRunner` (`src/benchmark/mod.rs`) turn `TimerResult`
  into series (`times_real`, `memory_usage`, energy, `resource_counters`).
- `Scheduler` (`src/benchmark/scheduler.rs`) supports grouped and round-robin schedules.
- Parameters: `Command::new_parametrized`; option values are not parameter-expanded today.

## Contract

### CLI
```
--concurrency <N>   Start N instances of the command simultaneously for every benchmark run.
                    Reported time = wall time until ALL N instances finished (makespan).
```
- `N` is an integer >= 1. `0` or non-numeric: error. `1` is the default and identical to today.
- Value may contain `{param}` (e.g. `-L c 1,2,4,8 --concurrency {c}`) to get a scaling curve.
  This needs a one-time option-expansion step per parameter combination; if that is too big for
  the first iteration, ship plain integers first and track `{param}` as a follow-up task.
- A warning is printed when `N` exceeds the logical CPU count (instances will queue on CPU).
- `--show-output` / `--output=inherit` with `N > 1`: warning that output interleaves; allowed.
  `--output-metric`, `--until`, `--show-output-on-failure` with `N > 1`: error in the first
  iteration (per-instance capture is a follow-up).
- Combines with `--warmup`, `--runs`, `--schedule`, `-i`, `--timeout`, `--prepare`, `--conclude`.

### Semantics
- One *run* = spawn N instances back to back (no waiting between spawns), wait for all.
- `--prepare`, `--conclude`, `--setup`, `--cleanup` run once per run / benchmark, not per instance.
  Instances must not rely on exclusive resources unless the user parameterizes them (documented).
- Spawn skew: instances start sequentially, so the Nth starts a little later. The run's
  makespan starts at the first spawn. Per-instance time starts at that instance's own spawn.
- Reaping: loop `wait4(-1, ...)` (restricted to the batch's pids) and map pid to spawn instant,
  so early finishers are timed exactly. Spawn-order waiting is not acceptable (overstates early finishers).
  Windows: one wait per handle via `WaitForMultipleObjects`, or a thread per instance.
- Primary series: `times` holds the makespan per run, so mean, stddev, percentiles, outlier
  detection, `--target-precision`, relative speed and comparisons work unchanged.
- `user`/`system`: sum over the instances of the run. `memory_usage_byte`: the maximum RSS of any
  instance, plus a documented note that the sum is not reported (shared pages).
- Shell spawn overhead is subtracted once per batch, not N times (spawns overlap).
- Exit codes: `exit_codes` holds N values per run, flattened row-major (run 0 instances 0..N-1,
  ...). Any failing instance fails the run, subject to `-i/--ignore-failure`.
- `--timeout` applies to the batch: when the deadline passes, all instances are killed (process
  group per instance on Unix, job object on Windows) and the run is marked timed out.
- Ctrl-C kills all instances of the current run, as today for one.
- Energy (`--energy`): RAPL measures the whole machine package, so the value is the batch energy
  over the makespan window. Also report `energy_per_instance_joules = batch / N`.
- Hardware counters (#49): batch totals.

### Terminal output
```
Benchmark 1: curl -s localhost:8080/api
  Time (mean ± σ):      84.2 ms ±   3.1 ms    [concurrency: 8]
  Per-instance (mean):  61.0 ms   p50 58.2 ms   p95 79.9 ms   max 83.7 ms
  Throughput:           95.0 runs/s
  Range (min … max):    79.3 ms …  92.8 ms    20 runs
```
Throughput = `N / mean makespan` (instances per second). The label is `instances/s`;
the example above in #53 says `runs/s`, which would be ambiguous with benchmark runs, so the name
used in the code and JSON is `throughput` with unit documented as instances per second.
Percentiles come from the flattened instance times (`runs x N` samples).

### JSON (additive, MINOR bump of `schemaVersion`; see `output-schema-contract.md`)
Per result, only when `N > 1`:
```jsonc
"concurrency": 8,
"instance_times": [[0.058, 0.061, ...8...], ...],   // seconds; one row per run, aligned with "times"
"throughput": 95.0,                                  // instances / s, mean over runs
"instance_percentiles": { "p50": 0.0582, "p95": 0.0799, "max": 0.0837 },
"energy_per_instance_joules": 0.22                   // only with measured energy
```
- When `N = 1` none of these keys is emitted (output identical to today).
- `times` meaning is the makespan; consumers detect that via `concurrency` being present.
  Because the meaning of `times` is conditional, this is documented in the contract table and
  `concurrency` is the discriminator. Ask `.product` / tools-benchmark owners before release.
- `--import-json` round-trips the new fields.
- Other exports: Markdown/CSV tables show the makespan columns and add a `concurrency` column
  only when any result has `N > 1`.

## Edge cases
- `N` large (e.g. 1000): spawn failure (`EAGAIN`, fd limits). Report which instance failed, kill
  the already started instances of that run, abort the benchmark with an actionable message.
- Instances that finish before later ones spawn (very short commands): makespan still correct;
  concurrency degenerates. Warn if mean makespan < 5 ms x N ("spawn cost dominates").
- Fast commands with `-N`: no shell, so no shell overhead to subtract.
- Stdout/stderr default to null; inherited streams are shared.
- Parameter lists multiply benchmarks as usual; `{param}` concurrency creates one benchmark per value.
- Round-robin schedule: one run (N instances) per scheduler step.
- `--omit-failed-runs`: the whole run (all N instances) is omitted.
- Memory: N captured outputs held at once if capture is later enabled; cap accordingly.

## Test plan
- Integration: `--concurrency 4 -N -r 3 'sleep 0.2'`: mean about 0.2 s (not 0.8 s), `instance_times`
  has 3 rows of 4.
- Integration: one failing instance fails the run; `-i` ignores it; `exit_codes` has 3x4 entries.
- Integration: `--concurrency 0` and `abc` give errors; `--concurrency 1` output equals baseline.
- Integration: `--timeout 100ms --concurrency 3 'sleep 5'` kills all three, no orphans.
- Unit: percentile helper (p50, p95, max) and throughput; per-instance timing from a mocked
  wait order (early finisher is not charged the later finisher's time).
- Unit: JSON round trip with and without concurrency fields.
- Both schedules (`grouped`, `round-robin`) in `scheduler_basic`-style tests with mocked executor.
- Linux and macOS and Windows CI for the integration cases; timing assertions with wide margins.

## Implementation breakdown
1. Option parsing and validation (`src/cli.rs`, `src/options.rs`): `concurrency: usize`, errors and warnings.
2. `timer::execute_batch_and_measure` returning `BatchResult { makespan, instances }`;
   Unix reaping with `wait4(-1)` mapping pid to start; Windows variant.
3. Batch-aware timeout and Ctrl-C (process groups / job objects for all instances).
4. Executor: call the batch function when `N > 1`, one shell-overhead subtraction, user/system sum, RSS max.
5. `BenchmarkRunner`: `instance_times`, flattened exit codes, `--omit-failed-runs` handling.
6. Percentile and throughput helpers (`src/stats/`).
7. Terminal output lines.
8. JSON fields, `output-schema-contract.md` table, MINOR bump, import round-trip.
9. Energy per instance.
10. Tests above; README, man page; CHANGELOG.
11. Follow-ups: `{param}` in the option value, per-instance capture with `--until`/`--output-metric`, per-instance rusage in JSON.

## Acceptance criteria
- Makespan statistics, per-instance percentiles and throughput.
- Works with grouped and round-robin schedules.
- Documented with caveats (CPU count, I/O contention, shared resources).

## As shipped (first iteration, #53)
- **Reaping.** Each instance runs on its own thread (one `execute_and_measure` per instance, released
  together by a barrier), so every instance is timed by its own timer and waited with its own
  `wait4(pid)` (Unix) or process handle (Windows). An early finisher is never charged the time of a
  later one, with the same code on Linux, macOS and Windows. The `wait4(-1)` batch variant and
  `WaitForMultipleObjects` of the proposal were not needed.
- **Makespan.** `time_real` = latest process exit minus earliest process start over the instances
  (`aggregate_batch` in `src/benchmark/executor.rs`, unit tested with synthetic start offsets).
- **Timeout.** Each instance has its own watchdog armed at its own spawn, so the batch deadline is
  `--timeout` after the last spawn (spawn skew is microseconds to a few milliseconds). Every instance
  is killed (process group on Unix, job object on Windows) and the run is marked timed out.
- **Failures.** Without `-i`, a failing instance fails the benchmark (the other instances of that run
  finish first, they are not killed). With `-i`, `exit_codes` has N entries per run; for
  `--omit-failed-runs` the run counts as failed if any instance failed and all N are omitted.
- **Shell overhead.** The calibrated shell spawn time is subtracted once from the makespan, once from
  each instance time, and N times from the summed user/system time (CPU time does not overlap).
- **Rejected combinations (clap errors).** `--until`, `--show-output-on-failure`, `--output-metric`
  (as proposed) and `--subtract` (a baseline of N instances has no useful meaning).
  `--show-output` / `--output=inherit` warn that output interleaves.
- **Warnings.** N above the logical CPU count; mean makespan below 5 ms x N.
- **Energy.** The sampler brackets the whole batch (`energy_joules` per run is the batch energy);
  `energy_per_instance_joules` is the mean divided by N. Not run on real RAPL hardware.
- **Mock executor (`--debug-mode`).** Instance `i` takes `(1 + 0.01 i)` x the requested time, so tests
  are deterministic.
- **`{param}` in the value (follow-up, shipped).** `--concurrency '{c}'` with `-L c 1,2,4,8`
  gives one benchmark per value. The value is parameter-expanded once per benchmark
  (`Command::with_concurrency_template`) and stored on the command, so executors and the runner read
  `command.concurrency_or(options.concurrency)`. Every combination is resolved before anything runs;
  a value that is not an integer >= 1 (or an unknown parameter) is an error, and a warning is printed
  once when a value exceeds the logical CPU count. A benchmark that resolves to 1 has no concurrency
  fields in the JSON, as without the option. Combining a `{param}` value with `--reference` is
  rejected: the reference command has no parameters to substitute.
- **`--until`, `--output-metric`, `--show-output-on-failure`: decision.** They stay rejected with
  `--concurrency` (clap conflict). The semantics are ambiguous (the pattern of which instance stops
  the timer? one metric value per instance or one per run?), and no consumer asks for them. Reopen
  with a concrete use case.
- **Spawn failure.** When an instance fails to spawn (descriptor or process limit), the instances of
  the run that already started are killed (`src/timer/batch.rs`: a per-batch registry of started pids,
  reached through a thread-local), instances that did not start yet are not started, and the error
  says how many were killed. Process group (Unix) or the process (Windows) is killed; descendants of a
  process started without a timeout are not. A registered pid is released only after the process was
  reaped.
- **CSV / Markdown.** A `concurrency` (CSV, appended after the existing columns) / `Concurrency`
  (Markdown, last column) column appears only when a benchmark used more than one instance; rows
  without it show 1. Other exports are unchanged. This does not change the JSON, so `schemaVersion`
  is unchanged.
- **Not done.** Per-instance rusage in JSON; killing descendants of instances on spawn failure when no
  process group exists; Windows run of the spawn-failure path (unit-tested on Unix only).
