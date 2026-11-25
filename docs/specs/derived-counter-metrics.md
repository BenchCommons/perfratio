# Spec: Derived hardware-counter metrics (#213)

## Problem / Motivation
References: #213, #49, `andrewrk/poop#12`, `andrewrk/poop#42`.

Raw cache-miss or branch-miss counts depend on how much work a command does. The rates
(`misses / references`) and IPC/CPI compare commands fairly. #49 collects the raw counters
(`perf_event_open`, Linux only); this spec defines the metrics derived from them.

## Contract
Formulas (per run, then averaged over the runs where the metric is defined):

| Metric | Formula |
|---|---|
| `ipc` | `instructions / cycles` |
| `cpi` | `cycles / instructions` |
| `cache_miss_rate` | `cache_misses / cache_references` |
| `branch_miss_rate` | `branch_misses / branches` |

- Rates are **fractions** (`0.182` = 18.2 %), unitless. The terminal shows percentages.
- A metric is **omitted** (JSON) / empty (CSV) when an input counter is missing or the
  denominator is 0. It is never `0`, `NaN` or infinite in that case. A zero numerator with a
  non-zero denominator is a valid `0`.
- Pairing: requesting `cache-misses` also samples `cache-references`; likewise
  `branch-misses`/`branches` and `instructions`/`cycles` (`counters::required_companions`).
- Terminal summary line: `IPC 1.15 · CPI 0.87 · cache miss 18.2% · branch miss 3.1%`,
  undefined metrics left out. Comparison delta: `18.2% -> 4.5% (-13.7 pp)`
  (`counters::format_rate_delta`).

### JSON (additive, MINOR)
`results[].efficiency`, an object whose keys are each optional; the whole object is omitted
when no metric is defined:
```json
"efficiency": { "ipc": 1.15, "cpi": 0.87, "cache_miss_rate": 0.182, "branch_miss_rate": 0.031 }
```
This is a new optional field, so it is a MINOR addition to the output schema of
`output-schema-contract.md` (#240): add the row `efficiency` to its field table and bump
`schemaVersion` MINOR in the release that first emits it. Until the collector (#49) lands
nothing emits it, so the schema version is unchanged by this change.

### CSV
Columns `ipc,cpi,cache_miss_rate,branch_miss_rate` are appended at the end, only when at
least one result has `efficiency`. Existing column positions do not change.

## Implementation
- `src/counters.rs`: `CounterSample`, `Efficiency` (`from_sample`, `mean_of`,
  `summary_line`), `format_rate_delta`, `required_companions`. Pure functions.
- `src/benchmark/benchmark_result.rs`: `efficiency: Option<Efficiency>`.
- `src/export/csv.rs`: conditional columns.

## Out of scope / depends on #49
There is no counter collector on `master` yet. `BenchmarkRunner::finish` sets
`efficiency: None`. #49 must fill `CounterSample` per run, call `Efficiency::mean_of`,
print `summary_line` under the raw counts, and use `format_rate_delta` in comparisons.

## Acceptance Criteria
- [x] Rate math with zero/missing handling, unit-tested (`src/counters.rs`).
- [x] Additive JSON field and conditional CSV columns.
- [ ] Collector records references/branch totals (#49).
- [ ] Terminal output next to raw counts (#49).
- Not run on real hardware: no PMU access on the development machine (macOS).
