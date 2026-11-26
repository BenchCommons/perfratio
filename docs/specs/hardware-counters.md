# Spec: `--counters` hardware performance counters on Linux (#49)

**Status:** Implemented on Linux (#49, schemaVersion 1.8; software events and the frequency are in `software-events-migrations.md` and `effective-frequency.md`). macOS and Windows report `unavailable`.

## Implementation notes (deviations from the proposal)
- Code: `src/perf_counters.rs`, wired into `run_command_and_measure_common`
  (`src/benchmark/executor.rs`), the runner (`hw_counters`), JSON and `src/delta_table.rs`.
- Default list is `instructions,cycles,cache-references,cache-misses,branches,branch-misses`
  (adds the two denominators needed by #213). `--counters` needs `=` for a list
  (`--counters=cycles`); a bare `--counters` uses the default and does not swallow the command.
- Opening: the fds are opened on the spawning thread with `inherit` and `enable_on_exec`
  (disabled otherwise). The thread never calls `exec`, so only the child tree is counted, from
  the `exec` of the command; counts of exited children are added to the parent fd. `pid=0`
  events need no race-prone attach after `spawn()`.
- Scope (D1 default `all`, user + kernel): when kernel counting is denied the run falls back to
  `user` with one warning and `counters_scope: "user"`. `--scope` and `:u/:k` are #218.
  `exclude_guest` is not set yet.
- Non-Linux: no error. One warning, `measurements.counters = "unavailable"`,
  `counters_unavailable_reason = "unsupported_platform"`, benchmark continues (never abort).
  macOS: the private `kperf`/`kpc` calls (`kpc_force_all_ctrs_set`, `kpc_set_counting`,
  `kpc_get_thread_counters`) all return -1 for an unprivileged process (tested on Apple M4,
  macOS, uid 501) and count per thread, not per child tree, so it is not implemented.
- Overhead: median of 50 `sh -c ''` runs (shell) or 20 `true` runs (`-N`) per event, subtracted
  and clamped at 0. Timed-out runs have `null` counters; warmup runs are not counted.
- `--metric instructions|cycles` ranks by the counter (`Metric::Instructions`/`Cycles`). It adds the
  counter to `--counters` (or enables just that one) and fails if any run has no reading, like
  `--metric energy`. Markdown/AsciiDoc/Org tables get IPC/CPI/miss-rate columns (#213) and the
  delta table notes multiplexed counters (#219). schemaVersion 1.10 adds `counters_coverage`.
- Not run on real hardware: the PMU test (`PERFRATIO_TEST_PERF=1`). A software-event
  (`task-clock`) test checks the open/inherit mechanism wherever `perf_event_open` is allowed.

## Problem / Motivation
References: #49, #48 (`--metric`), #213 (derived efficiency metrics), #240 (JSON contract),
`sharkdp/hyperfine#736`, `andrewrk/poop`.

Wall time is noisy on shared machines and CI runners. Retired instruction counts are almost
deterministic and are the usual low-noise signal for CI regression detection. Cycles, cache
misses and branch misses explain why one variant is faster than another.

## Current State (audit of `master`)
Nothing exists for hardware counters.
- No `perf_event_open` use, no `perf-event2` dependency (`Cargo.toml`: Linux-only section holds only `nix`).
- No `--counters` or `--metric` option in `src/cli.rs` (#48 is open).
- Not to be confused with `ResourceCounters` (`src/benchmark/timing_result.rs`), which holds
  rusage values (context switches, page faults, block I/O) behind `--resource-usage`. This is
  the closest pattern to reuse: per-run struct in `TimerResult.counters`, a `ResourceSeries`
  column set on `BenchmarkResult.resources`, `Option` on non-Unix.
- Reusable plumbing: `EnergySampler` (`src/energy/mod.rs`) shows the start/stop sampler shape and
  the `Dummy` fallback. `timer::unix_timer::wait_with_rusage` reaps the child with `wait4`,
  which is the point where counters must be read.
- JSON `perfratio.measurements` groups (`output-schema-contract.md` §2) give the place to
  report availability.

## Contract

### CLI
```
--counters [<LIST>]   Collect hardware performance counters (Linux only).
                      LIST is comma-separated. Default: instructions,cycles,cache-misses,branch-misses
```
- Names: `instructions`, `cycles`, `cache-misses`, `branch-misses`. Open set: later MINOR
  releases may add `cache-references`, `branches`, `stalled-cycles-frontend`, and so on.
- Unknown name: error listing valid names. Duplicate names: deduplicated, order kept.
- `--counters` without a value uses the default list. It needs `require_equals`
  (`--counters=cycles`) or a trailing position so it does not swallow the command; implementer
  must verify clap behaviour and pick one form.
- Non-Linux targets: error `hardware counters are only supported on Linux`.
- Compatible with `--energy`, `--resource-usage`, `--schedule round-robin`.
- Interaction with #48: once `--metric` exists, `instructions` and `cycles` become valid metric
  names. Until then `--counters` is report-only.

### Measurement semantics
- One counter fd per event, not a `PERF_FORMAT_GROUP` group (inherit plus group reads are
  restricted on older kernels). Each counter: `inherit(true)`, `exclude_kernel(true)`,
  `exclude_hv(true)`, `any_cpu`, created disabled.
- Scope: the benchmarked command and every process it spawns after enable. The fds are opened
  per run, enabled immediately before `spawn()` and read and disabled right after `wait4`
  returns. Counters opened per run (not once per benchmark) so `inherit` only sees that run's tree.
- Counts include perfratio's own fork/exec work between enable and `exec`. Overhead
  calibration: during calibration run `true` N times under the same counters, take the median per
  event, and subtract it from every run, clamped at 0. The shell wrapper (`-S`/default shell) is
  calibrated the same way as in the time path; with `-N` only the fork/exec overhead remains.
- Multiplexing: read `time_enabled`/`time_running` (`PERF_FORMAT_TOTAL_TIME_*`). If
  `time_running < time_enabled`, scale the value and mark the run `multiplexed`. At most 4
  default events fit in typical PMUs, so scaling is the exception.
- Warmup runs are not counted. `--prepare`, `--conclude`, `--setup`, `--cleanup` are not counted.
- Units: raw event counts as `u64`.

### Terminal output
One line after the memory/CPU lines, using means:
```
  Counters (mean):      instr 12.3M · cycles 9.8M (IPC 1.26) · cache-miss 41.2k · branch-miss 18.0k
```
IPC is shown only when both `instructions` and `cycles` are collected. Derived rates are
specified in #213 and are out of scope here, except IPC on this line.

### Availability and degradation
At startup (before the first benchmark) open one probe counter per requested event.
- Failure `EACCES`/`EPERM` (`perf_event_paranoid` too high): print **one** warning that names the
  value read from `/proc/sys/kernel/perf_event_paranoid` and suggests
  `sysctl kernel.perf_event_paranoid=1` (or `CAP_PERFMON`), then continue without counters.
- `ENOENT`/`ENODEV`/`EOPNOTSUPP` (VM without PMU, event unsupported): same, one warning, with
  the event name. If some events work and others do not, keep the working ones and warn once
  for the rest (status `partial`).
- Never abort the benchmark because counters are unavailable.

### JSON (additive, MINOR bump of `schemaVersion`; see `output-schema-contract.md`)
Top level, `perfratio.measurements`:
```jsonc
"measurements": { ..., "counters": "measured" }
```
Values: `measured`, `unavailable` (with `counters_unavailable_reason`: `unsupported_platform`,
`perf_paranoid`, `no_pmu`, `other`), `not_requested`, plus `partial` for mixed outcomes with
`counters_unavailable_events: ["cache-misses"]`. Omitting the key means 1.0 consumers see
nothing new.

Per result:
```jsonc
"counters": {
  "instructions": [12301234, 12299871, ...],   // per run, aligned with "times"
  "cycles":       [9800123, ...]
},
"counters_summary": {
  "instructions": { "mean": 1.23e7, "stddev": 4.1e3, "median": ..., "min": ..., "max": ... }
},
"counters_multiplexed_runs": 0      // optional, only when > 0
```
- Per-run arrays are aligned with `times` (index = run), as the contract requires. Runs removed by
  `--omit-failed-runs` or outlier discarding are removed from every aligned array, like the
  other series (`select(&self.resource_counters, keep)` in `src/benchmark/mod.rs`).
- A counter that failed for a single run is `null` in that position; the summary uses the
  non-null values.
- `--import-json` must accept and round-trip these fields, and ignore them when absent.
- CSV/Markdown/AsciiDoc/Org exports: not changed in the first iteration. A later change may add
  columns after #213.

## Edge cases
- Container: `perf_event_open` blocked by seccomp (Docker default) gives `EPERM`; same path as paranoid.
- `--timeout` kill: the run is already marked timed out; counters for it are dropped (`null`).
- Command exits in microseconds: counts are small; overhead subtraction clamps at 0 and the run
  stays valid.
- Non-deterministic commands (JIT, threads) still vary; the spec promises low noise, not zero.
- Multiple CPUs/hybrid cores (Intel P/E): the generic events map per core type. When the kernel
  exposes distinct PMUs, counts come from the core the process ran on. Document this; do not try to sum PMUs in the first iteration.
- 32-bit and non-x86 Linux: events differ in availability; probe handles it.
- `--concurrency` (#53): counters apply to the whole batch; per-instance counters are out of scope.
- Windows/macOS: error as above; no stub values.

## Test plan
- Unit: counter list parsing (default, duplicates, unknown, case sensitivity); overhead subtraction
  clamps at 0; scaling for multiplexed reads; aligned-array filtering on omit/outlier.
- Unit: JSON serialization with all, some, and no counters; `measurements.counters` statuses.
- Linux integration, `#[ignore]` unless `PERFRATIO_TEST_PERF=1` (CI runners often lack a PMU):
  `--counters instructions -N 'true'` has a smaller mean than `-N 'seq 1000000'`, and relative
  spread of `instructions` over 20 runs of `seq 100000` is below 1%.
- Linux integration with a mocked failing opener (trait injected like `EnergySampler`): one
  warning, benchmark completes, `measurements.counters = "unavailable"`.
- Non-Linux CI job: `--counters` exits non-zero with the documented message.
- CI keeps covering time and memory only; the PMU tests run on bare metal or a self-hosted runner.

## Implementation breakdown
1. Add `perf-event2` under `[target.'cfg(target_os="linux")'.dependencies]`; check MSRV 1.88 and `deny.toml` licenses.
2. `src/counters/mod.rs`: `CounterKind` enum, list parser, `CounterSampler` trait with
   `start()`/`stop()`, `Dummy` fallback (mirrors `src/energy/mod.rs`).
3. `src/counters/linux.rs`: builders, probe, multiplex scaling, read.
4. CLI flag in `src/cli.rs`, `Options` field, non-Linux error, validation in `src/options.rs`.
5. Hook into `timer::execute_and_measure`: enable before spawn, read after `wait_with_rusage`; add the values to `TimerResult` and `TimingResult`.
6. Overhead calibration in the executor calibration step; subtraction.
7. `BenchmarkRunner` collection, filtering with `select(...)`, `BenchmarkResult` fields.
8. Terminal line (`src/output/`) and warnings.
9. JSON export, `measurements.counters`, import round-trip, schema table in `output-schema-contract.md`, MINOR bump and CHANGELOG entry.
10. Tests above; README section and `doc/perfratio.1`.
11. Follow-up (separate issues): `--metric instructions|cycles` (#48), derived rates (#213).

## Acceptance criteria
- Per-run counters and summary in JSON, mean line in terminal output.
- Calibrated overhead subtracted; no negative values.
- One clear warning and normal results on unsupported systems; README section.
