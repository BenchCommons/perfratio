# Spec: software events: CPU migrations and context switches (#220)

**Status:** Implemented on Linux (#220, schemaVersion 1.12) for `cpu-migrations`,
`context-switches`, `page-faults` and `task-clock`. Not implemented: `alignment-faults` and
`emulation-faults` (names not accepted yet), the `--check-system` line after the runs, and the
rusage fallback for context switches.

## Implementation notes (deviations from the proposal)
- Code: `src/perf_counters.rs` (`CounterKind::SOFTWARE`, `plan`, `jitter_warning`),
  `src/system_check.rs` (`scheduler_check`), `src/delta_table.rs` (rows).
- Accepted names: `cpu-migrations`, `context-switches`, `page-faults`, `task-clock`.
  Shorthands in `--counters`: `sw` (`cpu-migrations,context-switches`), `default` (the hardware
  list, so `--counters=default,sw` works) and `frequency` (see `effective-frequency.md`).
  The default list is unchanged.
- Every software event is opened with `exclude_kernel = 0`, whatever the hardware scope became.
  `counters_scope` describes the hardware events only. A `:u`/`:k` modifier on a software event
  is an error ("software events cannot be scoped"); `--scope` does not change them, their JSON
  keys never carry a suffix, and they take no part in the user-space fallback of the hardware
  events (`counter-scope-modifiers.md`).
- Denied kernel scope: the events are `unavailable` with `kernel_scope_denied`. With only
  software events requested, `measurements.counters` is `unavailable`; mixed with hardware
  events it is `partial`, with `counters_unavailable_events` and
  `counters_unavailable_reason: "kernel_scope_denied"` (the reason is now also written for any
  `partial` result).
- Terminal: `cpu-migr 0.4 · ctx-sw 123`, appended in the order requested. `task-clock` prints
  as a duration. The warning is printed per benchmark after the runs (stderr) and is hidden by
  `--suppress-outlier-warnings`.
- `--check-system`: `i Scheduler migrations  not measurable (kernel scope denied)` when
  `--counters` includes `cpu-migrations` and the open was denied. The post-run line is not
  implemented: the jitter warning above already names the number of migrations.
- Delta table (`--delta-table`): rows `Context switches`, `CPU migrations`, `Page faults`
  (lower is better) when both commands have the series.
- Tests: pure tests for parsing, planning (mocked opener, `EACCES` gives
  `kernel_scope_denied`), the warning condition and the delta rows;
  `tests/software_events_linux_tests.rs` opens the real events and skips when denied.
  Not run on hardware: the `PERFRATIO_TEST_PERF=1` comparisons with `perf stat`, the
  `cpu-migrations:u` privilege check and the affinity checks.

## Problem / Motivation
References: #220, #49, #47 (rusage context switches), #64 (`--check-system`), #217, #218,
`perf-stat(1)`, `perf_event_open(2)`.
Shared background and dependencies: `pmu-extensions-common.md`.

A scheduler migration moves a thread to another core, so its L1/L2 contents are cold there.
`getrusage` reports context switches (`ru_nvcsw`, `ru_nivcsw`, #47) but nothing about
migrations. `perf stat` counts `cpu-migrations` and `context-switches` by default. They explain
outliers that no hardware counter does, and they cost no PMU registers.

## Contract

### Events
Added to the `--counters` name set (#49), as `PERF_TYPE_SOFTWARE`:

| Name | `config` | Meaning | Notes |
|---|---|---|---|
| `cpu-migrations` | `PERF_COUNT_SW_CPU_MIGRATIONS` | times a task of the run moved to a different CPU | No rusage equivalent. |
| `context-switches` | `PERF_COUNT_SW_CONTEXT_SWITCHES` | times a task of the run was switched out | Cross-check for #47's `ru_nvcsw + ru_nivcsw`. |
| `alignment-faults` | `PERF_COUNT_SW_ALIGNMENT_FAULTS` | kernel-handled unaligned accesses | Never counted on x86 (man page). |
| `emulation-faults` | `PERF_COUNT_SW_EMULATION_FAULTS` | kernel-emulated instructions | Architecture dependent. |

Source for the definitions: `perf_event_open(2)`. Implementation of the counting:
`include/linux/perf_event.h` (`perf_event_task_migrate`, `perf_event_task_sched_in`,
`perf_event_task_sched_out`): a migration is flagged on the task when it is moved and counted
when the task is next scheduled in; a context switch is counted at schedule-out.

- `alignment-faults` and `emulation-faults` are accepted names but are **not** in the default
  list and are opt-in (`--counters=alignment-faults`). On x86 they are always 0, so perfratio
  reports them `unavailable` with reason `not_applicable` instead of a column of zeros.
  Supporting them elsewhere needs a hardware check that is out of scope for the first
  iteration; the names are reserved so the JSON key is stable.
- `--counters` keeps the #49 default list (hardware events). Software events are requested by name, or by the shorthand
  `--counters=default,sw` (`sw` = `cpu-migrations,context-switches`). Rationale: adding events
  to the default list changes the default JSON (MINOR, but also changes what existing users
  with `--counters` pay for). Revisit once #49 has shipped.
- Software events work with `--counters=cpu-migrations` alone, with no hardware events, and
  on hosts without a PMU.

### Scope and privilege (important)
Migrations and context switches are counted from kernel context (the scheduler). The kernel
filters software events with `perf_exclude_event()` using the register state at the counting
site (`kernel/events/core.c`), so an event opened with `exclude_kernel = 1` is **not**
counted. Reading the code, `cpu-migrations:u` and `context-switches:u` are always 0.
(`perf stat` on locked-down hosts prints exactly this: `context-switches:u` with `0`.
Confirm in the hardware test below.)

Consequences:
1. These events must be opened with `exclude_kernel = 0` and `exclude_user = 0`. They ignore
   `--scope` and `:u` / `:k` (`counter-scope-modifiers.md`); a modifier on them is an error
   (`software events cannot be scoped`).
2. Opening with `exclude_kernel = 0` needs `perf_event_paranoid <= 1` or `CAP_PERFMON`
   (`perf_allow_kernel()`), higher than the user-space threshold of #217 and #49.
3. At `perf_event_paranoid = 2` the values are **not measurable**. perfratio must report them
   `unavailable` with reason `kernel_scope_denied`, never `0`. A zero would read as "no
   migrations" when the truth is "not measured".
4. Fallbacks: context switches have one (rusage from #47, `--resource-usage`), labelled
   `context-switches` with `source: "rusage"` only if the user did not enable the perf event.
   For migrations there is none: `/proc/<pid>/sched` (`se.nr_migrations`) needs
   `CONFIG_SCHED_DEBUG` and the child is gone when `wait4` returns.

### Measurement semantics
- Same lifecycle as #49: one fd per event, per run, `inherit = 1`, enabled before `spawn()`,
  read after `wait4`, `read_format` with `TOTAL_TIME_*` (never scaled in practice since
  software events are not multiplexed; `coverage` is 1.0).
- Counts cover the benchmarked process and all processes it spawns during the run. With the
  default shell (`-S`) the shell's own scheduling is included, like all #49 counters; `-N`
  removes it. Calibration (#49) subtracts the median of the calibration command, clamped at 0.
  For software events the overhead is normally 0 and small otherwise.
- `--affinity <one CPU>` makes migrations impossible: the value is 0 by construction. The jitter
  warning below is suppressed in that case.
- A task being migrated by `sched_setaffinity` from the parent before `exec` counts as one
  migration (the child starts on the parent's CPU, then moves). Calibration subtracts this.
- Warmup runs are not counted.

### Terminal output
Appended to the #49 counters line, only when requested:
`... · cpu-migr 0.4 · ctx-sw 123`. Mean per run, one decimal under 10, otherwise integer.

### Scheduler jitter warning
Replaces the unconditional text of the issue with a defined condition, evaluated per
benchmark after all runs, only when `cpu-migrations` was measured and there are >= 5 runs:
1. Let M = runs with at least one migration, N = the other runs. Require `|M| >= 1`,
   `|N| >= 2` and `|M| / total >= 10%`.
2. Compare wall time: warn when `mean(time of M) - mean(time of N) > stddev(time of N)`
   (the migrated runs are slower by more than the noise of the quiet runs).
3. Message (stderr, like other warnings; hidden by `--suppress-outlier-warnings`
   (`suppress-outlier-warnings.md`) and `--style none` rules for warnings):
   ```text
   Warning: 14 CPU migrations in 6 of 30 runs; those runs were 21% slower.
   Tip: pin the command with '--affinity <CPUS>' to avoid scheduler migrations.
   ```
   The percentage is `(mean(M) / mean(N) - 1)`. If all runs had migrations no comparison is
   possible and the line is informational (`... in all 30 runs`), shown without the tip
   condition (2).
4. Not shown with `--affinity` set to a single CPU, or when `cpu-migrations` is unavailable
   (then `--check-system` and the counters warning explain why).

This is a correlation hint, not proof; the text says "those runs were slower" and nothing
about cause.

### `--check-system` (#64)
Add one informational line (status `Info`, never fails `strict`, as in #217):
`i Scheduler migrations  not measurable (perf_event_paranoid=2)  → sudo sysctl kernel.perf_event_paranoid=1`
when `--counters` includes `cpu-migrations` and the kernel scope probe fails. After the runs,
a benchmark with migrations adds `! Scheduler migrations  14 in 30 runs → use --affinity`.
The system check runs before the benchmark, so the post-run line is a warning, not a check
result.

### JSON (additive: MINOR)
Same shape as every #49 counter, so no new structure:
```jsonc
"counters": {
  "cpu-migrations":   [0, 0, 1, 0, ...],     // per run, aligned with "times"
  "context-switches": [41, 44, 120, ...]
},
"counters_summary": { "cpu-migrations": { "mean": 0.1, "max": 1, ... } }
```
The issue's name `cpu_migrations` is replaced by the `perf` name `cpu-migrations`, matching the
other #49 keys (`cache-misses`). `perfratio.measurements.counters` is `partial` when some
requested events are unavailable, with `counters_unavailable_events: ["cpu-migrations"]` and
`counters_unavailable_reason: "kernel_scope_denied"` (open enum, new value = MINOR).
CSV and other tabular exports: unchanged in the first iteration (as #49).
`schemaVersion`: MINOR. Optional keys only.

## Edge cases
- VM without PMU: works (software events need no PMU). Needs paranoid <= 1 as above.
- Container seccomp: `EPERM`; same path as #217.
- Very short commands (< 1 ms): counts are 0 or 1; warning needs >= 5 runs and 10% affected,
  so a single spurious migration does not trigger it.
- Hybrid CPUs: a P-to-E migration is counted. It matters more than a same-type move; the
  warning text does not distinguish (follow-up).
- Cgroup v2 CPU pinning (`cpuset`) can make migrations impossible; the count is 0 and that is
  correct.
- `--timeout` kill: counters for the killed run are `null`, as in #49.
- `inherit` and software events: children created after enable are counted; an exited child's
  count is folded into the parent's event when it exits.

## Test plan
- Unit: name parsing (`sw` shorthand, `alignment-faults` on x86 gives `not_applicable`, modifier
  on a software event rejected); warning condition with synthetic series (below 5 runs, 10%
  threshold boundary, all runs migrated, single-CPU affinity); percentage maths; JSON
  round-trip with `null`; `unavailable` vs 0.
- Mocked opener: `EACCES` on exclude_kernel = 0 gives `kernel_scope_denied` and no zeros.
- Hardware (`PERFRATIO_TEST_PERF=1`, `perf_event_paranoid <= 1`):
  - `--affinity 0 -N 'seq 1000000'`: `cpu-migrations` all 0.
  - Load generator with `taskset -c 0-1` and a busy sibling process: `cpu-migrations` > 0 in
    at least one of 100 runs of a 50 ms spin command.
  - `context-switches` vs `perf stat -e context-switches`: within 20% for a command that
    sleeps 10 times (`for i in $(seq 10); do sleep 0.01; done`).
  - `context-switches` vs rusage (`ru_nvcsw + ru_nivcsw`): same order of magnitude.
  - Confirm that `cpu-migrations:u` is 0 so the privilege note above is verified, not assumed.
- Non-hardware: warning text snapshot test with `insta`.

## Implementation breakdown
1. Extend `CounterKind` with software variants and `is_software()`; reject scope on them.
2. Opener for `PERF_TYPE_SOFTWARE` in `src/counters/linux.rs` (always `exclude_kernel = 0`).
3. Privilege-aware availability: `kernel_scope_denied` and `not_applicable` reasons.
4. Series collection and filtering with the other #49 arrays.
5. Jitter warning in `src/benchmark/` warnings (next to `OutliersDetected`).
6. `--check-system` lines.
7. JSON fields, schema table, CHANGELOG, README (`--affinity` tip), man page.
8. Tests above.

## Acceptance criteria
- `cpu-migrations` and `context-switches` measured via `PERF_TYPE_SOFTWARE`.
- Not measurable cases show as unavailable, never 0.
- Warning follows the defined condition and points to `--affinity`.
- Per-run series in JSON next to the other counters.
- `--check-system` reports whether migrations are measurable.

## References
#220, #47, #49, #64, #217, #218; `perf_event_open(2)` (`PERF_COUNT_SW_*`);
`include/linux/perf_event.h`; `kernel/events/core.c` (`perf_exclude_event`, `perf_allow_kernel`).
