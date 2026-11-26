# Spec: `--jobs <N|auto>` independent parallel worker pool (#306)

**Status:** Shipped in 1.1 (schemaVersion 1.16). Parent tracker: #53. CPU partitioning of the workers: #307 (`worker-cpu-partition.md`).

## Problem / Motivation
References: #306, #53, #304 (trust tracker), `megafine` (`-j/--jobs`, `--schedule`), hyperfine#58.

`--concurrency N` (`concurrency.md`) answers "how does the command behave when N copies run at
once": the N instances form one measured batch. It does not speed up data collection: 1000
samples of a 50 ms command still take 50 s on an idle 16-core machine. `--jobs` is the other
goal, **sample throughput**: a pool of workers executes independent runs at the same time and
every run is timed on its own.

Parallel sampling is faster but not equivalent to sequential sampling. Concurrent runs of the
same command contend for caches, memory bandwidth, turbo headroom and the scheduler, and bias
each other. The results are therefore marked, and a warning is printed.

## Contract

### CLI
```
-j, --jobs <N|auto>   Run up to N independent invocations at the same time (default: 1).
                      auto = number of logical CPUs.
--schedule <POLICY>   grouped, round-robin (existing, only with --jobs 1) and
                      saturate (default with --jobs > 1), exclusive, fair (need --jobs > 1)
```
- `N` is an integer >= 1; `0`, `abc` and an empty value are errors. `--jobs 1` is the default
  and identical to today (the sequential code path is unchanged).
- Errors (clap or option validation): `--jobs` with `--concurrency` (different goals: a batch
  of N measured together vs. N runs measured separately), `--energy` / `--metric energy` (RAPL
  measures the whole package, so the energy of overlapping runs cannot be attributed),
  `--optimize` and `--subtract`. `saturate|exclusive|fair` without `--jobs` > 1 and
  `grouped|round-robin|--round-robin` with `--jobs` > 1 are errors that name the valid values.
- Warnings (stderr, once): `--jobs` above the usable CPUs (the `--affinity` list size, or the
  logical CPU count), and always that parallel sampling perturbs timing.

### Policies
All policies run on the same pool; they only decide which command receives the next run when a
worker is free. Commands are served least-started-first, so they interleave.
- `saturate`: a free worker always takes a run. With one command all workers run it.
- `exclusive`: a command is never started while one of its runs is executing. With one command
  the runs are sequential (use `saturate`); with several commands up to `min(jobs, commands)`
  workers are busy.
- `fair`: lockstep rounds. A command only gets its next run when no other command with runs
  left has fewer started runs, so started counts never differ by more than one. After Ctrl-C the
  runs of every command are cut to the smallest completed count, so the recorded run counts are
  equal.

### Semantics
- The warmup runs and the initial measurement (run 0, which also sizes `--runs`/`--duration`)
  are sequential, as with `--schedule round-robin`; the pool executes the remaining runs.
  Setup and cleanup run once per command, before and after the pool. Setup/cleanup that differs
  between commands is rejected as for round-robin (`--allow-setup-with-round-robin`).
- `--prepare` and `--conclude` run per run, on the worker that executes it, around that run.
  Parallel runs of one command share anything the commands write to, so give each worker its
  own state with `PERFRATIO_WORKER` (the 0-based worker slot, set for the benchmarked command
  and for `--prepare`/`--conclude`).
- The number of recorded runs equals `--runs` exactly. With `--target-precision` or
  `--duration` the stop condition is checked on finished runs, so up to `jobs - 1` runs that
  were already started can be recorded beyond it (`--max-runs` is still never exceeded).
- Statistics (mean, stddev, outliers, confidence intervals) are computed per run, as today.
  Runs are recorded in completion order; `per_run_values` stays aligned with `times`.
- `--timeout`: a run that times out marks the command as timed out, later runs of it are not
  started, runs in flight are discarded. A failing run (without `-i`) aborts the benchmark after
  the runs in flight have finished.
- Ctrl-C stops dispatching, the runs in flight are discarded, and the statistics of the runs
  collected so far are printed and exported.
- Hardware counters (`--counters` etc.) are opened per run on the worker thread and work as
  before. Energy is rejected (see above).

### JSON (additive, MINOR bump to 1.16; see `output-schema-contract.md`)
Only with `--jobs` > 1:
```jsonc
"perfratio": { "execution": { "jobs": 4, "schedule_policy": "saturate", "contended": true } }
"results": [ { "workers_used": 4, "contended": true, ... } ]
```
Times remain per run, so existing consumers are unaffected. A consumer that compares results
should refuse to mix `contended` results with sequential ones (trust tracker #304).

## Test plan / verification
- Unit (`src/benchmark/pool.rs`): the dispatcher policies with simulated workers: exact run
  counts, `exclusive` never overlaps a command with itself, `fair` keeps started counts within
  one, `saturate` fills all workers.
- Integration (`tests/jobs_tests.rs`, `--debug-mode` and, on Unix, real commands): option
  errors, JSON fields, run counts, wall-clock speedup, `exclusive` with a lock directory
  command, `fair` run counts, `PERFRATIO_WORKER`, Ctrl-C.
- Schema: `tests/json_schema_tests.rs` has `--jobs` cases.

## Not done
- `--until-interrupted` / `--runs 0` (run until Ctrl-C), mentioned in the issue as an opt-in
  decision; it applies to sequential runs as well and is left to its own issue.
