# Spec: Cachegrind/Callgrind adapter, `--collector cachegrind|callgrind` (#383)

**Status:** Implemented, `schemaVersion` 1.37 (additive, MINOR). Part of the output contract of #240. Parent: #365.
Related: #384 (`collector-plugins.md`), #317 (`metric-descriptors.md`).

## Problem / Motivation
On noisy CI machines or without PMU access, wall time and hardware counters vary or are not
available. Instruction counts and cache misses from a simulator do not depend on the machine
load, so they can detect small regressions there (the approach of iai and gungraun).

## Contract

### CLI
`--collector cachegrind` or `--collector callgrind`. One value. Needs Valgrind: perfratio runs
`valgrind --version` at start and fails with an install hint if it cannot run. The binary is
`valgrind` from `PATH`; `PERFRATIO_VALGRIND` replaces it (non-standard installs, tests). Like a
plugin it is trusted code run with the user's privileges. Not combinable with `--concurrency N`
or `--jobs N` (N > 1).

### What runs
After the **first timed run** of each command (and before `--conclude`) perfratio runs the command
once more:

```text
valgrind --tool=<tool> --trace-children=yes --cache-sim=yes --log-file=DIR/valgrind.log \
         --<tool>-out-file=DIR/<tool>.out.%p -- PROGRAM ARGS...
```

with the same program, arguments, environment (including `PERFRATIO_*`) and working directory,
no stdin and discarded output. `DIR` is a new directory with mode 0700 that is removed
afterwards. The command is run through `std::process::Command`, never through a shell of
perfratio's own. Under the default `--shell` this means Valgrind follows `sh -c` and counts the
shell and the program (children are followed and their files summed); `--shell=none` counts only
the program.

The counts are deterministic for a given binary, input and environment, so one run is enough. It
is not timed, is not part of `times`, and is slower than a normal run (typically 20 to 50 times).
A command that exits non-zero under Valgrind fails the benchmark unless the failure is ignored
(`-i`), like a timed run. If Valgrind writes no profile, the error shows the end of its log.

Known deviations from the timed runs: `--prepare` ran once for the timed run, so a command that
consumes its input is run a second time on the state it left; the Valgrind run has no
`--input`; affinity and priority are not applied.

### Metrics
Read from the totals of the profile (`summary:` of Cachegrind, `totals:` of Callgrind), summed
over the process files. Events a profile lacks are left out, never written as `0`.

| id (`<tool>.`) | from events | unit | `kind` |
|---|---|---|---|
| `instructions` | `Ir` | count | `simulated` |
| `data_reads`, `data_writes` | `Dr`, `Dw` | count | `simulated` |
| `l1_instruction_misses` | `I1mr` | count | `simulated` |
| `ll_instruction_misses` | `ILmr` | count | `simulated` |
| `l1_data_misses` | `D1mr` + `D1mw` | count | `simulated` |
| `ll_data_misses` | `DLmr` + `DLmw` | count | `simulated` |
| `estimated_cycles` | L1 hits + 5 × LL hits + 35 × RAM hits (the formula of iai) | count | `estimated` |

All have `direction` `lower_is_better`, `scope` `process_tree`, `source` `valgrind.<tool>`.
The simulated cache is Valgrind's model of the machine's cache, not the hardware: use the counts
to compare two builds, not to predict time.

### JSON export (`schemaVersion` 1.37)
The same keys as the plugin protocol (`collector-plugins.md`): `results[].collector_metrics`
(`"cachegrind.instructions": [1000]`, one entry because the run is single and deterministic),
`collector_metrics_summary` and descriptors `collector.cachegrind.instructions` and so on in
`perfratio.metrics[]`. Additive; no existing field changes. Same version note as the plugin
spec: #393 to #400 also use 1.37, so the second pull request to merge renumbers by rebase.

## Why the adapter does not use the plugin protocol (#384)
The issue allows the adapter to use the protocol if that fits. It does not:

- The protocol is `start`, spawn, `stop`, `read` around a command that perfratio spawns. Valgrind
  must be the **parent** of the command and runs it itself, 20 to 50 times slower, so it has to
  replace the timed run or run beside it.
- Extending the protocol so that a plugin returns a wrapper command would give every plugin the
  power to rewrite the command line that is timed. The trust model of the protocol relies on a
  plugin only observing.

Both share what is visible to consumers: `collector_metrics`, the `collector.<name>.<id>`
descriptors and the `kind`/`source` fields. The code is independent (`src/valgrind.rs`,
`src/collector.rs`), so a later wrapper capability can be added without changing the adapter's
output.

## Tests
- `src/valgrind.rs`: parsing of a Cachegrind and a Callgrind fixture, cache-less profiles,
  summing children, malformed profiles, command wrapping (program, arguments with spaces and
  `$`, environment).
- `tests/collector_tests.rs`: a fake `valgrind` script (set by `PERFRATIO_VALGRIND`) that writes a
  fixture profile, for the export, the descriptors and the failure paths; a missing Valgrind; the
  option conflicts. `real_cachegrind_counts_are_deterministic` uses the real Valgrind and is
  skipped when it is not installed.

## Limitations
- Valgrind runs on Linux and some Unix systems; there is no Windows or (Apple silicon) macOS
  support, and the option fails there with the Valgrind error.
- One Valgrind run per command (not per parameter-independent run), so no spread is reported.
- No per-function or per-line data; only the totals.
- Branch simulation and `--metric` ranking on these values are not implemented.

## References
#383, #384, #317, #240; gungraun, iai, Valgrind Cachegrind and Callgrind manuals.
