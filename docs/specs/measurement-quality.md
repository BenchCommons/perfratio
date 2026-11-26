# Specification: Measurement quality report (`--quality`, `--fail-on-quality`)

**Issue:** [#318](https://github.com/BenchCommons/perfratio/issues/318)
**Status:** Completed except the checks that wait for sibling issues (§4). JSON `schemaVersion` 1.18 (1.21 adds `below_resolution`, #313).
**Related:** #54 (interference), #219 (counter coverage), #240 (contract), #316 (workload validation),
`interference-diagnostics.md`, `tukey-outliers.md`, `target-precision.md`, `workload-validation.md`.

## 1. Problem / Motivation

perfratio prints separate warnings (outliers, interference, off-CPU, energy domains) and exports
ad-hoc fields. A consumer such as tools-benchmark or a CI gate has no single, machine-readable
answer to "should I trust this measurement?". The quality report is that answer: a list of objective
checks per benchmark and a grade derived from them by a fixed rule.

The report states facts, never hypotheses about causes (no "backend bound", no "thermal
throttling" guesses). Existing diagnostics are reused, not recomputed: `Diagnostics`
(`src/stats/diagnostics.rs`), `OutlierReport` (`src/outlier_detection.rs`),
`PrecisionReached`, `counters_coverage`, `energy_samples` and `validation` of the result.

## 2. Contract

### 2.1 Checks

Every benchmark has all checks below, in this order. A check has an `id` (open enum, consumers
ignore unknown ids), an optional `metric`, a `status` and an optional `detail` (one sentence).

| id | metric | pass | warn | fail | unavailable | not_applicable |
|:--|:--|:--|:--|:--|:--|:--|
| `sample_sufficiency` | primary metric | ≥ 10 runs (≥ 20 when percentiles are exported) | 3–9 runs, or 10–19 with percentiles | < 3 runs | no completed run | |
| `statistical_precision` | primary metric | `--target-precision` met, or without a target half-width ≤ 5% | target not met, or half-width in (5%, 20%] | half-width > 20% (no target) | precision not computable | |
| `outlier_burden` | `time`, and `memory` / `energy` when their outlier statistics exist | outliers < 10% of the runs | ≥ 10% (`is_severe`), or outliers cause most of the variance | | fewer than 4 runs (time) | |
| `trend` | `time` | no significant drift | Mann–Kendall trend, the thresholds of `interference-diagnostics.md` | | fewer than 8 runs | |
| `interference` | `time` | one cluster of run times | two separated clusters (multimodal) | | fewer than 8 runs | |
| `measurement_floor` | `wall` | mean at least 3 runner floors and its CI above the floor (#313) | within the floor (`--below-floor warn`) | within the floor with `--below-floor fail` | the floor could not be measured | `--calibrate=off`, `--debug-mode` |
| `counter_coverage` | `counters` | all counters read, scheduled ≥ 90% | scheduled < 90% (PMU multiplexing), or some readings missing | scheduled < 50% | counters requested, none read | `--counters` not given |
| `energy_coverage` | `energy` | every run has an energy sample | some runs without | fewer than half of the runs have one | `--energy` given, no sample | `--energy` not given |
| `workload_validation` | none | all validated runs correct | | any run invalid (also when omitted) | no run validated | no validation options |

Rules that hold for every check:

- A check whose data source gave nothing reports `unavailable`, never `pass`.
- `not_applicable` means the feature was not requested.
- The energy value is the energy of the whole measurement domain (the package), not of the process;
  the `energy_coverage` detail says so.

### 2.2 Grade

Deterministic, documented, not a score:

1. any check `fail` → `unreliable`;
2. else any check `warn` → `with_caveats`;
3. else `comparable`.

`unavailable` and `not_applicable` do not change the grade. Grades are ordered
`comparable` < `with_caveats` < `unreliable`.

### 2.3 CLI

```
--quality <off|summary|full>    Terminal output of the report
--fail-on-quality <grade>       Exit with code 5 if a benchmark's grade is worse than <grade>
```

- Without `--quality`, the report is printed only when some check is `warn`, `fail` or
  `unavailable`. `summary` always prints the grade and the checks that are not `pass`; `full` prints
  every check; `off` prints nothing. `--quality` never changes the JSON, which always has the report.
- `--fail-on-quality comparable` fails on `with_caveats` and `unreliable`;
  `--fail-on-quality with_caveats` fails on `unreliable`; `unreliable` never fails. The exit code is
  **5**. It sits next to `--fail-if-regressed` (exit code 3, the size of a change): use both in CI.
  If both gates fail, the regression error (3) is reported.
- The block is printed on stdout after the benchmark's summary and warnings.

### 2.4 JSON (`schemaVersion` 1.18, additive)

```json
"quality": {
  "grade": "with_caveats",
  "checks": [
    { "id": "sample_sufficiency", "metric": "wall", "status": "warn",
      "detail": "5 runs; 10 or more give a usable confidence interval" },
    { "id": "counter_coverage", "metric": "counters", "status": "not_applicable" }
  ]
}
```

Schema: `docs/schema/perfratio-export.schema.json`. Imported results keep the `quality` they were
exported with and are included in `--fail-on-quality`.

## 3. Implementation

`src/quality.rs`: `QualityReport::assess(&BenchmarkResult, &QualityContext)` is a pure function of the
result and of what was requested (`--metric`, `--counters`, `--energy`), so checks are table-testable.
`BenchmarkRunner::finalize` calls it and prints; `main.rs` applies the gate.

## 4. Not yet covered

These checks are reserved ids and appear once their data exists (#314, #315 are open; `measurement_floor` shipped with #313, `measurement-floor.md`):
`environment_stability` (cgroup throttling,
thermal), `environment_compatibility` (only in `--compare`). Adding a check changes the report, so
it raises the JSON MINOR version only if it adds fields; a new `id` alone does not.

## 5. Tests

Unit (`src/quality.rs`): grade derivation as a table, a passing and a failing fixture for every
check, terminal rendering by mode, JSON shape. Integration (`tests/quality_tests.rs`): the report in
the JSON of every benchmark, a noisy fixture that flips the grade, the gate and its exit code,
`--quality` modes, a failed validation, schema (`tests/json_schema_tests.rs`).
