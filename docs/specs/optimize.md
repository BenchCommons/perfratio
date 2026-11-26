# Spec: `--optimize <VAR> <MIN> <MAX>` parameter search (#98)

**Status:** Shipped (#98), `schemaVersion` 1.4. `--metric` (#48) has landed, so `--optimize` works
with every metric (`wall`, `cpu`, `energy`, `memory`, `custom:<name>`). Deviations from the
proposal below: when the range fits into the budget (`MAX - MIN + 1 <= budget`) all values are
evaluated and `algorithm` is `"exhaustive"`; the JSON `optimize` object also has `best_value`
and, with other parameters, `fixed`; points are evaluated and exported one by one. Follow-ups
(not done): re-evaluation of the final bracket under noise, `--reference`, HTML marker for the
best point (#60), float parameters. The energy path is tested with the mock executor
(`--debug-mode`) and with pure unit tests on the search; it was not run against real RAPL.

## Problem / Motivation
References: #98, #48 (`--metric`), `sharkdp/hyperfine#320`, #60 (HTML report), #240 (JSON contract).

"Which value of X is best?" (`make -j N`, thread count, batch size, `zstd -N`) is common.
`-P x 1 64` benchmarks all 64 values, which can take hours. For perfratio the answer that
minimizes energy often differs from the one that minimizes time, which is the point of
performance per watt.

## Current State (audit of `master`)
Nothing exists: no `optimize` identifier in `src`.
Dependencies and building blocks:
- `--metric` (#48) is **not implemented** (`src/cli.rs` has no `metric` argument). Until it lands,
  `--optimize` can only minimize mean wall time. This is the main blocker for the energy case.
- `-P/--parameter-scan` and `-L` generate the full command list up front in
  `Commands::from_cli_arguments` (`src/command.rs`). `--optimize` needs the opposite: the next
  parameter value depends on previous results, so commands are created lazily.
- `Command::new_parametrized` can build a command for one value.
- `Scheduler::run_benchmarks` (`src/benchmark/scheduler.rs`) iterates a fixed command list, so it
  needs an adaptive driver. Round-robin interleaving across a lazily grown list is not defined
  and is rejected in the first iteration.
- Significance: `stats::deep::compare_samples` and `compare::compare` (`src/compare.rs`, Welch
  t-test, `MAX_P_VALUE = 0.05`) are reusable for best vs runner-up.
- Energy: `EnergySampler` exists; per-run `energy_joules` and `mean_energy_joules` are in `BenchmarkResult`.

## Contract

### CLI
```
--optimize <VAR> <MIN> <MAX>   Search integer parameter VAR in [MIN, MAX] for the value that minimizes
                               the primary metric (--metric, default: mean wall time).
--optimize-budget <N>          Max number of distinct values to evaluate [default: 12].
```
- `MIN` and `MAX` are integers, `MIN <= MAX`, otherwise error. `MIN == MAX` evaluates one value.
- `VAR` is referenced as `{VAR}` in the command, as with `-P`. The command must use it, else error
  (same rule as `-P`).
- `--optimize-budget` must be >= 3 (error below). Budget counts distinct values; cache hits are free.
- Conflicts: `-P`/`-L`/`-F` on the same variable name (error). Different variables combine: the search runs once per combination of the others, and the summary lists one best value per combination.
- Conflicts in the first iteration: `--schedule round-robin`, `--import-json`, `--reference` (reference command has no parameter; allowed once defined, tracked as follow-up), `--parameter-file` for the optimized variable.
- `--metric energy` requires `--energy` and a working RAPL; otherwise error before the search starts. Without #48 only wall time is accepted.
- `--warmup`, `--runs`, `--min-runs`, `--target-precision`, `--setup`, `--prepare`, `--cleanup`, `--shell` apply to each evaluation as in a normal benchmark.

### Search algorithm
- Assumption: the metric is unimodal in VAR over `[MIN, MAX]`. Documented in `--help` and the README.
- Integer golden-section search with an evaluation cache (`BTreeMap<i64, f64>`), as sketched in #98:
  interior points `a = hi - round(0.618 (hi-lo))`, `b = lo + round(0.618 (hi-lo))`; narrow
  the bracket until `hi - lo <= 2` or budget is spent; then evaluate the remaining values in the final bracket if the budget allows.
- Endpoints `MIN` and `MAX` are always evaluated when budget >= 5 so that a monotonic metric (best
  at an edge) is detected and reported as "best at boundary".
- `f(x)` = mean of the primary metric of one full `BenchmarkRunner` run for `x`.
- Ties (`fa == fb`): prefer the lower value of VAR; deterministic.
- A failed command (non-zero exit without `-i`) aborts, as a normal benchmark does.
- Noise: with very noisy metrics golden section can discard the true minimum. Mitigation in the
  first iteration is documentation plus the unimodality warning; re-evaluation of the final
  bracket is a follow-up.
- Ctrl-C: stop the search, print and export the points evaluated so far (`interrupted: true`).

### Unimodality warning
After the search, sort evaluated points by VAR. If a value strictly higher than the best on each
side is lower than a point closer to the best (the curve has an interior local maximum between
the best and an evaluated point, by more than the combined standard error), print one warning:
"The metric does not look unimodal in <VAR>; the result may be a local minimum." Needs at least 4 points.

### Terminal output
```
Optimization of j ∈ [1, 64] for energy (golden-section search, 11 evaluations):
  j=24  1.82 J ± 0.03   ← best
  j=16  1.86 J ± 0.04
  j=40  1.97 J ± 0.05
  ...
Best: j=24 (energy 1.82 J; wall 3.41 s). The difference from j=16 is not significant (p=0.21).
```
- Points are listed best first. The runner-up for the significance statement is the best
  *other* evaluated value; test uses `compare_samples` on the per-run primary-metric samples.
- Each evaluation also prints the usual benchmark block above the summary, unless `--style=basic` or `--style=none` hides it (existing behaviour).

### JSON (additive, MINOR bump of `schemaVersion`; see `output-schema-contract.md`)
Every evaluated point is a normal `results[]` entry with `parameters: { "j": "24" }`, so existing
consumers work unchanged. Top level gains an optional key:
```jsonc
"optimize": {
  "var": "j", "min": 1, "max": 64,
  "metric": "energy",              // "time" until #48 lands
  "algorithm": "golden-section",
  "budget": 12, "evaluations": 11,
  "best": 24,
  "best_command": "make -j 24",
  "runner_up": 16, "p_value": 0.21, "significant": false,
  "unimodal_warning": false,
  "at_boundary": false,
  "evaluated": [1, 64, 24, 40, 16, ...]   // in evaluation order
}
```
With several combinations of other parameters, `optimize` is an array of such objects with an
extra `"fixed": { "other": "x" }`. Results order is evaluation order. `relative_speed` is computed
against the best point (the fastest by the primary metric).

## Edge cases
- Range of 1 or 2 values: no search, evaluate all.
- Budget larger than the range: evaluate everything (degenerates to `-P`) and say so.
- `MIN > MAX`, non-integer, negative bounds: negative integers are allowed; empty ranges are not.
- Parameter formatting inside the command is decimal with no padding.
- Metric returns `NaN` or 0 (e.g. energy unreadable mid-run): error naming the value.
- Plateau (all equal): terminates by budget; best is the lowest VAR.
- `--runs` very small (e.g. 1): stddev undefined; significance reported as "not computable".
- `--max-benchmarking-time`/`--timeout` apply per evaluation.
- Export formats other than JSON: ordinary one-row-per-evaluation tables.

## Test plan
- Unit `golden_section_min`: `f(x) = (x-17)^2` on [1, 64] finds 17 within budget 12 and without
  re-evaluating cached points; a plateau terminates; budget exhaustion returns best seen; tiny ranges
  (1, 2, 3 values); minimum at either boundary; ties pick the lower value; negative bounds.
- Unit: unimodality detector on a W-shaped curve vs a V-shaped one.
- Unit: option validation (missing `{VAR}`, conflicts, budget < 3, `MIN > MAX`).
- Integration: `--optimize n 1 20 -N -r 2 "sh -c 'sleep 0.00{n}'"` finds 1 or 2.
- Integration: JSON contains all evaluated points plus the `optimize` object; Ctrl-C test keeps partial results.
- Energy path: mocked `EnergySampler` giving a parabola in VAR; `--metric energy` finds its minimum (no PMU/RAPL needed in CI).
- Property test: for random convex integer functions, the result is within one step of the
  true minimum given budget >= ceil(log_phi(range)) + 5 (the two endpoint evaluations cost 2 more than the +3 first estimated).

## Implementation breakdown
1. Land #48 `--metric` (prerequisite for the energy case). Wall-time-only version may ship first.
2. `src/optimize.rs`: `golden_section_min` with cache and budget, plus the unimodality check. Pure functions, unit tested.
3. CLI args and validation (`src/cli.rs`, `src/options.rs`), conflicts.
4. Lazy command creation: refactor `Commands`/`Scheduler` so a driver can request "benchmark command for VAR=x" and receive the `BenchmarkResult`.
5. Adaptive driver in `Scheduler` that runs `f(x)` via `BenchmarkRunner`, collects results in evaluation order; Ctrl-C handling.
6. Summary output and significance with `compare_samples`.
7. JSON `optimize` object, schema contract table, MINOR bump, `--import-json` round-trip.
8. Multi-combination support (one search per combination of other parameters).
9. Tests above; README and man page; CHANGELOG.
10. Follow-ups: re-evaluation of the final bracket under noise, `--reference`, HTML report marker for best (#60), float parameters.

## Acceptance criteria
- Finds the optimum of unimodal metrics in O(log range) benchmarks.
- Works with `--metric energy`, reports significance versus the runner-up, warns when the curve is not unimodal.
