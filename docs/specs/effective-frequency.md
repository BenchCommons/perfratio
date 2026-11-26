# Spec: effective CPU frequency and throttling detection (#222)

**Status:** Implemented on Linux (#222, schemaVersion 1.9). Not run on hardware: the
`PERFRATIO_TEST_PERF=1` checks against `perf stat` and fixed-frequency governors.

## Implementation notes (deviations from the proposal)
- Code: `src/frequency.rs` (basis, per-run quotient, CV/drift warnings, sysfs reader),
  `src/perf_counters.rs` (`export`, hidden `task-clock`), `src/delta_table.rs`.
- `task-clock` is added automatically whenever `cycles` is requested and is not exported or
  printed unless the user lists it (then it is a counter in nanoseconds). `--counters=frequency`
  collects `cycles` (and the hidden `task-clock`); `cycles` is still printed.
- Basis: scope `all` with `task-clock` gives `task_clock`; scope `user` gives `user_time`
  (marked `~`); `all` without `task-clock` gives no frequency. `system_time` is reserved for
  `--scope kernel` (#218) and not produced yet.
- Runs under 5 ms on CPU keep their value in `effective_frequency_hz` (`null` only when `cycles`
  or the time is missing) but are left out of `mean_effective_frequency_hz`,
  `stddev_effective_frequency_hz`, the terminal value and the warnings. The mean and stddev are
  omitted when no run is long enough.
- Terminal: `cycles 4.24G (IPC 1.21) @3.42 GHz ± 0.18 GHz` (`±` only when it is > 0).
- Warnings (stderr, at least 10 eligible runs, hidden by `--suppress-outlier-warnings`): the CV
  and drift texts above, plus one against the hardware limit: the mean is below 70% of
  `cpuinfo_max_freq` (highest value of `/sys/devices/system/cpu/cpu*/cpufreq`, restricted to the
  `--affinity` CPUs). The sysfs value is read only on Linux and is not exported. The tip is
  printed once after the warnings.
- Delta table: row `CPU frequency` (mean ± stddev), always without a verdict.
- Hybrid CPUs: `hybrid_cpu_unpinned` is not implemented; the number is the time-weighted mean.

## Problem / Motivation
References: #222, #49, #213, #218, #64, `perf-stat(1)`, `tools/perf/util/stat-shadow.c`.
Shared background and dependencies: `pmu-extensions-common.md`.

A 30% time difference between runs is often DVFS or thermal throttling, not the code. `perf
stat` prints `# 3.42 GHz` next to `cycles`. perfratio can compute the same number per run from
counters it already collects, and warn when it drifts.

## Contract

### What is computed
`effective_frequency_hz = cycles / task_clock_seconds` for each run, where
- `cycles` is `PERF_COUNT_HW_CPU_CYCLES` counted over **user and kernel** (scope `all`) for the
  benchmarked process tree, and
- `task_clock_seconds` is `PERF_COUNT_SW_TASK_CLOCK` (nanoseconds the task tree was on a CPU),
  opened alongside.

This is the clock rate while the command was actually running, because unhalted cycles are
counted only while the thread executes. It matches how `perf stat` derives `GHz`
(`cycles / task-clock`).

### Why `task-clock` and not the issue's `user + system` or wall time
- `ru_utime` + `ru_stime` come from scheduler accounting. On kernels with tick-based CPU
  accounting their resolution is a scheduler tick (1 to 10 ms), which makes the quotient noisy for
  short runs. `task-clock` is a nanosecond software timer.
- Wall time includes waiting (I/O, sleeps) and gives a frequency that is too low for any
  command that is not CPU-bound; this is the failure mode the feature must not have.
- With `inherit`, `task-clock` sums over threads and children, as `cycles` does, so the ratio is the
  time-weighted mean frequency for multi-threaded commands. Wall time would be wrong there.

### Scope pairing (the bias to avoid)
`cycles:u / task-clock` is wrong: `task-clock` includes kernel time, so the result is low by
the kernel fraction. Rules:
1. Preferred: `cycles` in scope `all` and `task-clock`. Basis `"task_clock"`.
2. If the kernel scope is denied (`perf_event_paranoid = 2`, `counter-scope-modifiers.md`) the
   `cycles` counter is user-only. Then use `cycles:u / user_time` with `user_time` from rusage
   (`ru_utime` of the run). Basis `"user_time"`; it is coarser (tick resolution possible) and
   the terminal marks the value `~`. Do not use `task-clock` here.
3. `--scope kernel` or an explicit `cycles:k`: `cycles:k / system_time`, basis `"system_time"`.
4. If `cycles` is `split` into `:u` and `:k`, the sum is the numerator and rule 1 applies.
The frequency uses `cycles` shared with `--counters` when `--counters` already collected
`cycles` in a compatible scope, and opens its own `cycles` and `task-clock` otherwise.
A fixed counter on Intel (core cycles) and a software event cost no programmable counters.

### Activation
- Automatic whenever `cycles` is collected (`--counters` includes `cycles`): the quotient costs
  one extra software event (`task-clock`). Also available without `--counters` as
  `--counters=frequency` (shorthand for `cycles` + `task-clock`, printed as frequency only).
- No new top-level flag. Rationale: the issue asks for it "when cycles are collected"; a flag
  would only matter if the extra fd were expensive, and it is not.

### Terminal output
```
  Counters (mean):
    Cycles:        4.24G   [Effective frequency: 3.42 GHz ± 0.18 GHz]
    Instructions:  5.12G   (IPC 1.21)
```
Mean ± standard deviation over per-run values, formatted as GHz above 1 GHz, otherwise MHz.
In the one-line #49 form: `cycles 4.24G @3.42 GHz`. `~3.42 GHz` for basis `user_time`.
Runs with `task-clock` below 5 ms are excluded from the displayed mean and from the warning
(the clock ramp and the exec path dominate). They stay in the JSON series like any other run.

### Frequency-variation and throttling warning
Evaluated per benchmark, only with at least 10 runs after dropping runs under 5 ms (otherwise
no warning, no tip):
1. **Variation:** `CV = stddev(f) / mean(f)` over per-run frequencies. Warn if `CV > 5%`.
2. **Drift:** ordinary least squares of `f` on run index. Let `drop = (fit(last) - fit(first)) /
   fit(first)` and `r` the Pearson correlation. Warn if `drop <= -5%` and `r <= -0.5`
   (a steady decline, which is what thermal throttling looks like; random noise gives small
   `|r|` even with a large CV).
3. Messages:
   ```text
   Warning: CPU frequency varied by 7.2% (CV) across 30 runs (mean 3.42 GHz).
   Warning: CPU frequency fell from 4.2 GHz (run 1) to 3.1 GHz (run 30): likely thermal throttling.
   Tip: let the CPU cool down between runs, or fix the frequency:
        sudo cpupower frequency-set -g performance
   ```
   The first and last values are the regression fit at run 1 and run N, not raw samples, so a
   single outlier cannot produce the headline. The issue's "from run 1 to run 10" example is
   replaced by this definition.
4. Warmup runs do not count. Hidden with `--suppress-outlier-warnings`; `--style none`
   as for other warnings.

Round-robin schedules (`--schedule round-robin`): per-benchmark series are still in run order for
that benchmark; the drift test is valid but interleaved runs with other commands share heat. The
warning text is unchanged.

### `--check-system` link (#64)
No new check. The warning's tip repeats the governor advice of the existing `CPU governor` line.

### JSON (additive: MINOR)
SI units per the contract (hertz, not GHz as the issue proposes), per run and aligned with
`times`, `null` where the run was too short or a counter could not be read:
```jsonc
"effective_frequency_hz": [3.42e9, 3.40e9, ...],
"mean_effective_frequency_hz": 3.41e9,        // mean of non-null per-run values
"stddev_effective_frequency_hz": 1.8e8,
"effective_frequency_basis": "task_clock"     // task_clock | user_time | system_time
```
The issue's `effective_frequency_ghz` / `frequency_series_ghz` become the three keys above.
Present only when `perfratio.measurements.counters` is `measured` or `partial` with `cycles`
available. `task-clock` itself is not exported as a counter in v1 (the run time of the task tree
is already `user + system` to first order); a future change may add it under `counters`.
`schemaVersion`: MINOR. Import round-trips; absent on older files.

## Edge cases
- **Hybrid CPUs:** P and E cores run at different frequencies and a task event is bound to one
  PMU (`pmu-extensions-common.md`). Without `--affinity` to one core type the frequency is
  `unavailable (hybrid_cpu_unpinned)`.
- **SMT:** `cycles` counts unhalted cycles of the hardware thread, the same quantity
  `perf stat` uses; two siblings running together can show lower per-thread throughput but the
  same clock.
- **Turbo and AVX licences:** reported as measured; that is the point.
- **Frequency-invariant cycle counters:** Intel `ref-cycles` and similar count at constant
  rate; this spec uses core `cycles`. A constant-rate alternative (`cycles / ref-cycles *
  TSC_hz`, the APERF/MPERF method) works without any time source but needs `ref-cycles` and
  TSC calibration; not in scope.
- **AMD and Arm:** `CPU_CYCLES` is the core clock on both; AMD maps it to event `0x76`
  (`ls_not_halted_cyc`). No vendor-specific code is needed.
- **VMs:** needs a vPMU with `cycles`; `task-clock` always works. Guest `cycles` may be
  virtualised to a fixed rate on some hypervisors, which gives a constant frequency; document
  that the number is the guest-visible clock.
- **Throttling not visible:** power-limit throttling that lowers the clock mid-run still shows
  because the numerator drops; throttling that only inserts idle (clock modulation, deep halts
  not counted as cycles) shows as lower `task-clock` efficiency, not as lower frequency. The
  warning does not claim to catch it.
- **Very high task-clock but no cycles** (permission or no PMU): the feature is `unavailable`
  with the counters reason; no frequency is invented from time.
- **`--timeout` kill:** run is `null`.

## Test plan
- Unit: quotient by basis (task_clock, user_time, system_time); scope pairing rules table;
  `~` marker; short-run exclusion; CV and OLS drift on synthetic series (flat, noisy flat, steady
  decline, step, one outlier, fewer than 10 runs); formatting GHz/MHz; JSON round-trip with nulls.
- Mocked opener: kernel scope denied selects `user_time` basis; no PMU gives `unavailable`.
- Hardware (`PERFRATIO_TEST_PERF=1`):
  - With `cpupower frequency-set -d F -u F` fixed to F: reported mean within 2% of F for a spin
    loop; with the `userspace` governor at two values the two reports differ accordingly.
  - Compare with `perf stat -e cycles,task-clock` GHz (it prints `cycles / task-clock`): within 1%.
  - Sleep-heavy command (`sleep 0.1`): frequency is not artificially low (wall-time basis would
    be).
  - Multi-threaded spin on 4 threads: matches the per-core clock, not 4x.
- A stress test that heats the CPU (long `stress-ng` before the runs) produces the drift
  warning on a laptop-class machine; manual, not CI.

## Implementation breakdown
1. `task-clock` software event in the #49 sampler; scope-pairing function.
2. Per-run frequency calculation and series in `TimingResult` / `BenchmarkResult`.
3. Summary stats and terminal formatting.
4. CV and drift warnings in the warnings module.
5. JSON keys, schema table, CHANGELOG, README, man page.
6. Tests above.

## Acceptance criteria
- Effective frequency per run when `cycles` is available, shown next to cycles.
- Correct scope pairing; never `cycles:u / task-clock`.
- Warning on CV > 5% or a steady decline, with the defined thresholds.
- JSON export (hertz); CSV and other tabular exports unchanged in the first iteration, as #49.

## References
#222, #49, #213, #218, #64, #240; `perf-stat(1)`; `tools/perf/util/stat-shadow.c`;
`perf_event_open(2)` (`PERF_COUNT_HW_CPU_CYCLES`, `PERF_COUNT_SW_TASK_CLOCK`,
`PERF_COUNT_HW_REF_CPU_CYCLES`).
