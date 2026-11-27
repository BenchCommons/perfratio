# Spec: multi-metric adaptive stopping — `--target-precision-metrics` (#369)

**Status:** Implemented. JSON `schemaVersion` 1.37 (shared with #367, not yet released).

## Problem / Motivation
`--target-precision` (`target-precision.md`) stops on the wall-clock time only. When memory or
energy is the metric of interest, the runs can stop while that metric is still imprecise.

## Contract

### CLI
```
--target-precision-metrics METRICS   comma-separated: wall, memory, energy (default: wall)
```
Requires `--target-precision`. The target (`--target-precision PCT`), the confidence level and the
rule (`--target-precision-method`, so the sequential rule of #322 too) are those of the
single-metric option and apply to each listed metric. The runs of a command stop when **every**
listed metric has a relative half-width of its mean at or below the target, with the same bounds
as before (`--min-runs`, `--max-runs`, `--aggregate-parameter-runs` cycles,
`--max-benchmarking-time`). Unknown names and duplicates: unknown fails before any run;
duplicates are ignored. Without the option the behaviour is unchanged.

- `wall`: the wall-clock times, with the baseline uncertainty of `--subtract` as before.
- `memory`: peak memory of each run.
- `energy`: gross energy of each run that has a sample.
- A first run excluded by `--first-run` counts for none of them.
- A metric that cannot be waited for does not hold the runs: no sample at all (energy without a
  sensor, or on CI) or a mean of 0 or less (nothing to be relative to). It is reported with
  `met: false`. Waiting would run until the time budget for a target that cannot be reached.
- `--duration` and `--runs` keep priority over stopping; the entries are reported anyway.

### JSON (`results[].precision_metrics`, new in 1.37, MINOR, additive)
Array with one entry per listed metric, in the order given:
`metric` (`wall`, `memory`, `energy`; open enum), `target`, `reached` (relative half-width or
`null` without 2 samples or a positive mean), `runs` (samples of that metric), `met`.
`results[].precision` is unchanged and stays the wall-clock entry of `--target-precision`.
The schema (`docs/schema/perfratio-export.schema.json`, snapshot `docs/schema/1.37/`) and
`output-schema-contract.md` list the field.

## Assumptions
Independent runs with finite variance per metric (as in #322). Stopping on several metrics at
once is a stopping time like any other: the confidence sequence stays valid, the fixed
interval is as optimistic as it is for one metric. The metrics are correlated (a slow run
uses more energy); the rule does not use that.

## Tests
- `tests/target_precision_tests.rs` (`--debug-mode`, deterministic, time and memory only):
  entries and order, stop at `--min-runs` with an energy entry that has no samples, absence
  without the option, duplicates, invalid names, requires `--target-precision`. The JSON schema test
  has a case.
- `src/stats/sequential.rs` (`two_metric_coverage`): seeded simulation of wall time and energy
  with the confidence sequence stopping when both reach 2%; both means stay inside their
  interval at the stop at the nominal rate (400 simulations, tolerance 3 points).
- Energy (RAPL) is not run in CI (needs bare-metal Linux with a readable `energy_uj`).

Refs: #369, #322, #367.
