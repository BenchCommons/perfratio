# Specification: Measurement floor (`--calibrate`, `--below-floor`)

**Issue:** [#313](https://github.com/BenchCommons/perfratio/issues/313)
**Status:** Shipped. JSON `schemaVersion` 1.23.
**Related:** #318 (`measurement-quality.md`), #240 (contract), `post-fork-timing-linux.md`, `subtract-followups.md`.

## 1. Problem / Motivation

A command that runs for a millisecond is dominated by process creation and timer resolution.
perfratio calibrates the shell spawn time and subtracts it, but never said when a benchmarked
command is at or below what the runner can resolve, and it treated the subtraction as exact.
Megafine (GPL-3.0; this is an independent implementation) keeps a floor measured with a trivial command.

## 2. Contract

### 2.1 Measuring the floor

Once per session and per distinct executor (shell), right after the shell calibration, perfratio
runs a trivial command 2 times (discarded) and then up to 20 times: `true` without a shell
(`cmd /C exit 0` on Windows), `exit 0` through a shell (a builtin of sh, cmd and PowerShell). It uses
the executor of the benchmark, so shell, affinity, priority and output policy are the same. Collection
stops after 0.2 s (at least 5 runs after the 2 discarded ones), so slow spawns cost little startup time. A failure gives no floor and the check is `unavailable`.

- `wall_floor`: median wall time minus the shell spawn time the runner subtracts from every run
  (never below 0).
- `wall_floor_mad`: median absolute deviation of the wall times.
- `rss_floor`: median peak RSS in bytes; omitted when the platform reports none (never `0`).
- `runs`: number of runs behind the estimate.

The floor is not measured with `--debug-mode` (mock executor). "Cached per session" means one
measurement per executor for the whole invocation; nothing is stored on disk.

### 2.2 Below resolution

A result is below resolution when its mean wall time is

- less than **3 floors** (`BELOW_FLOOR_FACTOR`), or
- the lower end of its 95% CI of the mean is at most `wall_floor + wall_floor_mad`. Adding the MAD means the
  uncertainty of the floor, and so of the subtracted overhead, is not treated as exact.

Nothing is hidden: the numbers and the relative speeds are shown. A warning follows the summary when
a ratio involves a below-resolution command.

### 2.3 CLI

```
--calibrate[=<auto|off>]   Measure the runner floor (default: auto)
--below-floor <warn|fail>  Action for a result within the floor (default: warn)
```

`--below-floor fail` makes the `measurement_floor` check `fail` (grade `unreliable`). perfratio prints and
exports the statistics, then exits with code **6** (`--fail-on-quality` is 5, `--fail-if-regressed` is 3).

### 2.4 JSON (`schemaVersion` 1.23, additive)

```json
"perfratio": { "calibration": { "wall_floor": 0.0018, "wall_floor_mad": 0.0003, "rss_floor": 917504, "runs": 20 } },
"results": [ { "quality": { "below_resolution": true, "checks": [ { "id": "measurement_floor", "metric": "wall", "status": "warn", "detail": "..." } ] } } ]
```

`calibration` and `below_resolution` are omitted when no floor was measured (`--calibrate=off`, mock
executor, failure). With several shells only the first executor's floor is exported; each result is
judged against its own executor's floor. The quality check `measurement_floor` is `not_applicable` with
`--calibrate=off`, `unavailable` when the measurement failed, `pass`, `warn` or `fail`.

## 3. Implementation

`src/calibration.rs` (pure rules `Floor::from_samples`, `Floor::classify`; `measure_floor`),
`Executor::floor`/`set_floor`, `Scheduler::run_benchmarks`, `quality::measurement_floor`.

## 4. Not covered

`--region` mode does not exist yet; the floor must be skipped there when it does. The uncertainty of
a `--subtract` baseline is not propagated into the floor. CPU time floor is not measured.

## 5. Tests

Unit (`src/calibration.rs`): floor from samples, the 3-floor rule, CI overlap, MAD widening.
Integration (`tests/calibration_tests.rs`): `true` below and `sleep 0.1` above (Unix), `--calibrate=off`,
`--below-floor fail` exit code 6, debug mode without a floor. Schema: `tests/json_schema_tests.rs`.
