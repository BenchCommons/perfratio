# Spec: time-budgeted sampling, `--duration` (#216)

## Problem / Motivation
References: #216, #240 (fast languages need short, noise-resistant runs), `andrewrk/poop`.
Microbenchmarks finish the default 10 runs in milliseconds, which is dominated by
scheduling noise. Users want "sample each command for N seconds" instead of a run count.

## Contract
```
--duration <DURATION>   sample each command until its summed run time reaches DURATION
                        (5s, 5000ms, 1m; same parser as --timeout)
```
- **No short flag.** `-d` is already `--show-output`; reusing it would break scripts.
- The budget is the **sum of the measured run times** of that command (wall clock of the
  command, including the first measurement run). Warmup, `--prepare`, `--conclude` and
  perfratio's own overhead are not counted. This is reproducible under `--debug-mode`,
  where runs are simulated and take no real time.
- Floor: at least 3 runs (enough for a mean and σ), even if run 1 exceeds the budget.

## Precedence and exclusion
| Option | Behavior |
|---|---|
| `-r/--runs` | clap conflict (a fixed count and a budget contradict each other) |
| `--target-precision` | clap conflict (two stop rules); `--max-benchmarking-time` already requires `--target-precision`, so it is rejected too |
| `-m/--min-runs` | replaces the floor of 3 (default minimum 10 does not apply) |
| `-M/--max-runs` | caps the runs; wins over the budget and over the floor (`-M 2` gives 2 runs) |
| `--timeout` | compatible; a timed-out command stops sampling, as before |
| `--warmup N`, `--warmup auto` | compatible; warmup runs happen first and are not counted |
| `--schedule round-robin` | compatible; each command has its own budget and stops separately, the others continue |
| `--aggregate-parameter-runs` | sampling does not stop in the middle of a cycle |
| `--min-benchmarking-time` internal default (3 s) | not used; `--duration` replaces it. It still estimates the progress bar length, which grows as needed |

## JSON (additive, optional)
Per result, present only with `--duration`:
- `target_duration_seconds` (number): the budget.
- `total_sampling_seconds` (number): summed run time actually sampled (>= budget unless capped by `-M`/timeout).
- `runs_done` (integer): runs performed.

These are optional additions and are omitted otherwise, so they are not a breaking change
to the output contract (#240). Once `schemaVersion` exists they count as an additive minor bump.

## Tests
`tests/duration_tests.rs` (all `--debug-mode`): run count and JSON fields, 3-run floor,
`-m`/`-M`, `--warmup auto` with round-robin, absence without the flag, conflicts and invalid
values, `--timeout`.
