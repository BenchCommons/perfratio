# Spec: unique run identity (`PERFRATIO_RUN_ID`) and per-run artifact directories (#308)

**Status:** Shipped in 1.1 (schemaVersion 1.19).

## Problem / Motivation
References: #308, #53 (`concurrency.md`), megafine "Run id" (idea only, no code taken).

`PERFRATIO_ITERATION` is not unique: warmup runs are `warmup-N`, every command restarts at 0 and
the instances of a `--concurrency` batch all see the same value. A `--prepare 'generate > in.txt'`
used with concurrent instances makes them overwrite each other's file, which corrupts both the
result and the measurement. Hooks need an id that is unique for every process that is started.

## Contract

### Environment variables
Exported to `--prepare`, the command, `--conclude`, and also to `--setup` and `--cleanup`:

| Variable | Value |
|---|---|
| `PERFRATIO_RUN_ID` | u64, starts at 0, one counter per perfratio process |
| `PERFRATIO_WORKER_ID` | 0-based position of the instance in a `--concurrency N` batch, N > 1. Not set otherwise (an inherited value is removed) |
| `PERFRATIO_RUN_DIR` | with `--artifact-dir DIR`: `DIR/run-<PERFRATIO_RUN_ID>`, created before the process starts |

`PERFRATIO_ITERATION` keeps its meaning.

### Uniqueness
- A run is one warmup run or one timed run. Ids increase by one per run, across warmup and timed
  runs, across commands and across parameter combinations.
- A batch of N instances takes N consecutive ids: instance `i` sees `first + i` and
  `PERFRATIO_WORKER_ID=i`. `--prepare` and `--conclude` run once per batch and see `first`, the
  id of instance 0. So `tmp.$PERFRATIO_RUN_ID` is collision free for every process that writes it.
- `--prepare`, the command and `--conclude` of the same run see the same id.
- `--setup` and `--cleanup` and the commands of `--subtract` take a fresh id each.
- Calibration runs (shell spawning time, counter overhead) are not runs of the benchmark and do not
  take ids from the counter; they see `18446744073709551615` (`u64::MAX`).
- Ids are unique within one perfratio process only. Two perfratio processes started at the same
  time both start at 0: use `--artifact-dir` with a directory of their own.

### Expansion
The variables are environment variables, so a command line expands them only when it runs through
a shell (the default; not with `-N`/`--shell=none`). With `-N` the command can still read them
from its environment.

```sh
# parallel safe: every instance has its own input file
perfratio --concurrency 4 \
  --prepare 'generate > in.$PERFRATIO_RUN_ID.txt' \
  --conclude 'rm in.$PERFRATIO_RUN_ID.txt' \
  'consume < in.$PERFRATIO_RUN_ID.txt'
```

Note that with `--concurrency`, `--prepare` runs once per batch and sees the id of instance 0, so
files the instances read must be created by the instances themselves, or be named from the
`PERFRATIO_RUN_ID` of the batch and shared read-only. Per-instance files are best made by the
command, e.g. `sh -c 'work > out.$PERFRATIO_RUN_ID'`.

### `--artifact-dir <DIR>`
Creates `DIR/run-<id>` (and missing parents) for each run before it starts and exports it as
`PERFRATIO_RUN_DIR`. The directories are kept: they hold the artifacts of the run (outputs, logs,
profiles). Creating a directory happens before the process is spawned and is not part of the
measured time.

### JSON (additive, `schemaVersion` 1.18 to 1.19)
Per result:
- `run_ids`: u64 per run, aligned with `times` (with `--first-run=separate` the dropped first run is
  dropped here too). With `--concurrency` it is the id of instance 0.
- `instance_run_ids`: with `--concurrency`, one row of N ids per run aligned with
  `instance_times`. The index in the row is the `PERFRATIO_WORKER_ID`.

## Implementation
- `src/run_id.rs`: the counter, `RunScope` (opened by the runner around prepare, command and
  conclude of one run) and the per-thread worker identity (set by `run_batch` for each instance).
  A thread-local carries the scope so that the `Executor` trait is unchanged.
- `executor::run_command_and_measure_common` exports the variables.

## Test plan
- Unit: counter, scope nesting, worker ids (`src/run_id.rs`).
- Integration (`tests/run_id_tests.rs`, also on Windows with `%VAR%` syntax): prepare, command and
  conclude see one id; 4 instances writing `tmp.$PERFRATIO_RUN_ID` produce ids `0..16` with no
  duplicate and `id % 4 == worker`; `--artifact-dir` creates one directory per run; JSON fields.

## Out of scope
`--jobs` (parallel worker pool, #53 follow-up) will give its workers `PERFRATIO_WORKER_ID` through
the same thread-local identity.
