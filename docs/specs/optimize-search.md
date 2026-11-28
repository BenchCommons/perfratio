# Spec: Multi-dimensional and Bayesian parameter search for `--optimize` (#385)

**Issue:** #385. **Status:** implemented. **Schema:** additive, `schemaVersion` 1.45.
Extends `optimize.md` (#98). Parent: #303. Related: #297, #336.

## Problem / Motivation
`--optimize VAR MIN MAX` (#98) searches one integer or fractional parameter by golden-section
search and assumes the metric is unimodal in it. Real tuning has several interacting parameters
(`make -j N` with a batch size, a thread count and a buffer size) and metrics that are noisy or
not unimodal. Three things are missing: a search over more than one variable, algorithms that do
not assume unimodality, and a check that the winner is not a lucky measurement (the winner's
curse: the point picked because it looked best is biased low).

## Contract

### CLI
```
--optimize <VAR> <MIN> <MAX>        repeatable: one grid axis per occurrence
--optimize-algorithm golden|random|halving|bayes
--optimize-validate                 benchmark winner and runner-up again in a fresh round
--optimize-budget <N>               max distinct points (at least 3, default 12)
--seed <N>                          already exists; seeds all random choices
```
- One `--optimize` without `--optimize-algorithm` (or with `golden`) is the golden-section search
  of #98, unchanged in behaviour and output.
- Several `--optimize` default to `random`. `golden` with several is an error: it is a one-variable
  algorithm.
- Integers only for the new algorithms. `-D` (fractional grid) is an error with them; it stays
  available for the one-variable golden-section search.
- The command must use `{VAR}` of every searched variable. A name given twice, a name that is
  also a `-P`/`-L`/`--parameter-file` variable, round-robin scheduling, `--import-json` and
  `--reference` are errors, as for #98. Other parameters combine with the search: one search per
  combination.
- `--optimize-confirm` (pooled re-runs) belongs to golden-section and is an error with the other
  algorithms; `--optimize-validate` replaces it. `--optimize-validate` and `--optimize-algorithm`
  without `--optimize` are errors, as is `--optimize-validate` with the plain golden-section search.

### Grid and budget
The grid is the product of the integer ranges. `--optimize-budget` caps the number of **distinct
points** that are benchmarked. A grid that fits into the budget is evaluated completely in grid
order (`algorithm` is `exhaustive`), whatever the algorithm. Ties go to the lower point
(lexicographic by the order of `--optimize` on the command line).

### Algorithms
- **random**: `budget` distinct points drawn uniformly from the grid, each benchmarked as in a
  normal run (`--runs` and the other stopping options apply).
- **halving** (successive halving, eta = 3): `budget` random points get `R` runs, where `R` is
  the lower run bound of the command line (`--runs`, `--min-runs`; default 10). The best third
  (at least 2) are benchmarked again with `3R` runs, then `9R`, and so on, until two points
  remain; those are benchmarked at the highest fidelity and ranked. Every benchmark of a rung is
  independent (samples are not pooled across rungs). Rungs use a fixed number of runs, so
  `--runs N` means "first rung of N", not "N runs in all rungs", and sequential stopping rules do
  not shorten them. `results[]` keeps the last benchmark of each point.
- **bayes**: `2d + 1` random points (`d` variables, at least 3), then one point per step: a
  Gaussian process (squared-exponential kernel on the coordinates scaled to `[0,1]`, length scale
  chosen from 0.15, 0.3, 0.6 and 1.2 by marginal likelihood, measurement noise from the standard
  error of each point) is fitted to the standardized metric, and the unevaluated point with the
  highest expected improvement is benchmarked next. Grids up to 4096 points are scored completely,
  larger ones on 1024 random unevaluated points per step. It needs no dependency (a small
  Cholesky solver). The model assumes a smooth surface; on a rough one it degrades to roughly
  random search. A flat or non-finite model falls back to the first candidate.
- All randomness is a SplitMix64 stream seeded with `--seed`, so a search is reproducible for a
  given seed and its meaning does not change with dependency upgrades.

### Winner and significance
The winner is the lowest value among the points of the highest fidelity reached. The runner-up is
the next one. `p_value` and `significant` compare them with the same test as `--compare`
(`compare_samples`, level 0.05) and are omitted when it is not computable (fewer than 3 samples).
`at_boundary` is true if any variable of the winner is at its minimum or maximum.

### Validation (`--optimize-validate`)
After the search (not after Ctrl-C), the winner and the runner-up are benchmarked again, with
the options of the command line (halving: the runs of its last rung). These samples are separate
from the search. `validation.status`:
- `confirmed`: in the fresh round the winner is faster than the runner-up, p < 0.05;
- `refuted`: the runner-up is faster, p < 0.05. A warning is printed;
- `inconclusive`: the difference is not significant or not computable (also without a runner-up).

`validation.drift = value / search_value - 1` is the winner's curse made visible (positive: the
winner got slower). Validation runs are not part of `--optimize-budget` and not `results[]`
entries. They are counted in `validation.runs`, not in `cost.runs`.

### Budget accounting
`optimize.cost`: `benchmarks` (a point benchmarked on two rungs counts twice), `points` (distinct,
at most `budget`), `runs` (sum over all benchmarks), and for halving `rungs: [{runs, points}]`.

### Terminal
```
Optimization of a ∈ [1, 64], b ∈ [1, 16] for wall (bayesian search, 24 points, 240 runs):
  a=13 b=4  1.00 s ± 0.01 s   ← best
  a=12 b=4  1.01 s ± 0.01 s
  … 22 more
Best: a=13 b=4 (wall 1.00 s). The difference from a=12 b=4 is not significant (p=0.40).
Validation in a fresh round: confirmed (wall 1.00 s, +0.2% from the search, p=0.01 against the runner-up).
```
At most 10 points are listed.

### JSON (schemaVersion 1.45, additive)
The `optimize` object keeps its fields. New: `vars`, `best_point`, `runner_up_point`,
`evaluated_points`, `seed`, `cost`, `validation`; new `algorithm` values `random`,
`successive-halving`, `bayesian`. The one-variable fields (`var`, `min`, `max`, `best`,
`runner_up`, `evaluated`) are written for a one-variable search and omitted for several variables;
`unimodal_warning` is not written by the new algorithms (it needs a one-dimensional curve).
This is MINOR: no existing file changes, and a consumer that handles the golden-section output
keeps working. A consumer of the new searches reads `best_point`, which is present for every
search of 1.45 (golden-section keeps writing `best`).
`docs/schema/perfratio-export.schema.json` makes `var`, `min`, `max`, `best`, `evaluated` and
`unimodal_warning` optional; see `output-schema-contract.md`.

**Version note:** 1.45 is the version reserved for this change in the merge order of the schema PRs (#402=1.43, #403=1.44, #405=1.45); this change adds no field that depends on the others.

## Edge cases
- Budget larger than the grid: exhaustive. A grid of one point: that point.
- A metric that is missing, 0 or not finite aborts, as in #98.
- Ctrl-C: the points so far are ranked and exported, `interrupted: true`, no validation.
  The ranking uses the highest rung reached; if Ctrl-C hits mid-rung, that rung can hold a single
  point, which then ranks first without having been compared at that fidelity.
- `halving` on an exhaustive grid still runs its rungs, so `cost.benchmarks` can exceed `points`.
- Grids over `u128` saturate when counting; points are decoded from a `u128` index.

## Test plan (implemented)
- `src/search.rs` unit tests on synthetic surfaces with an injected evaluator (no process is
  started, fully deterministic): random determinism, distinct points and budget; exhaustive
  grids; halving rung sizes, fidelity and survivors; Bayesian search finding the minimum of a
  smooth two-variable bowl (within 6% of the optimum with 24 of 1024 points), flat surfaces, grids
  too large to enumerate; interruption; errors; ties; analysis and JSON shape; validation status.
- `tests/optimize_search_tests.rs` with `--debug-mode`: end-to-end JSON for each algorithm, seeds,
  rungs and run counts, validation, combination with `-L`, option errors.
- `tests/json_schema_tests.rs`: two-variable and halving exports validate against the schema.

## Not done / questions for the maintainers
- Fractional and non-numeric variables in the new algorithms (`-D` is rejected). Needs a decision
  on per-variable steps (`-D` is global today).
- Log-scaled axes. Powers of two are common (`block 1 1024`); scale by `2**{x}` in the shell for now.
- Noise-aware halving (re-using the samples of lower rungs) and parallel evaluation with `--jobs`.
- An HTML marker for the multi-variable winner (the report marks golden-section winners only).
- Whether `--optimize-validate` should default to on for the new algorithms.
