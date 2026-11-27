# Spec: `--metric` Primary Optimization Target (#48)

## Problem / Motivation
References: `sharkdp/hyperfine#736`, `perfratio#48`.

By default, benchmarking tools compute all comparisons, relative speed ratios, rankings, summaries, and outlier detection solely from **wall-clock time**. However, modern performance engineering frequently targets other critical dimensions:
1. **Energy consumption (Joules):** Finding the greenest or most energy-efficient implementation (perfratio's core value proposition).
2. **CPU time (`user + system`):** Measuring algorithmic efficiency or multi-process computational work independent of I/O latency or scheduling delays.
3. **Memory footprint (Peak RSS):** Tracking memory regressions, comparing memory-efficient data structures, or optimizing for constrained environments.
4. **User / System time:** Isolating user-space computation vs kernel overhead.

## Proposed Contract

### 1. CLI Option
```
--metric <METRIC>
    Primary metric used for statistics, outlier detection, comparison, and sorting.
    [default: wall]
    [possible values: wall, cpu, user, system, energy, memory, instructions, cycles, custom:<name>]
```

- When `--metric energy` is passed, energy measurement is automatically enabled (`measure_energy = true`). If energy sampling is unsupported or unprivileged on the host system, perfratio fails immediately with an explanatory error.
- Case-insensitive parsing and common aliases (e.g. `wall-clock`, `rss`).

### 2. Metric Abstraction
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    #[default]
    Wall,
    Cpu,
    User,
    System,
    Energy,
    Memory,
}
```

Methods on `Metric`:
- `name(&self) -> &'static str`: e.g. `"wall"`, `"cpu"`, `"energy"`, `"memory"`.
- `display_header(&self) -> &'static str`: `"Time"`, `"CPU Time"`, `"User Time"`, `"System Time"`, `"Energy"`, `"Memory"`.
- `comparison_words(&self) -> (&'static str, &'static str)`:
  - Time variants: `("faster", "slower")`
  - Energy: `("less energy", "more energy")`
  - Memory: `("less memory", "more memory")`
- `verb(&self) -> &'static str`:
  - Time: `"ran"`
  - Energy & Memory: `"used"`

### 3. Primary Samples Extraction
In `BenchmarkResult`:
```rust
pub fn primary_samples(&self, metric: Metric) -> Option<Vec<f64>>
```
- Extracts the appropriate float values:
  - `Wall`: `self.times.clone()`
  - `User`: `self.user_times.clone()`
  - `System`: `self.system_times.clone()`
  - `Cpu`: zip of user and system times, summed per run
  - `Energy`: `self.energy_joules.clone()`
  - `Memory`: `self.memory_usage_byte` cast to `f64`

### 4. Terminal Report & Summary
- The primary statistic header in the terminal matches the chosen metric:
  - If `Wall`: standard `Time (mean ± σ): ...`
  - If `Energy`: `Energy (mean ± σ): 1.250 J ± 0.050 J`, with `Time` on the secondary line.
  - If `Memory`: `Memory (mean ± σ): 12.5 MB ± 0.5 MB`, with `Time` on the secondary line.
  - If `Cpu`: `CPU Time (mean ± σ): ...`, with `Time` on the secondary line.
- The summary table formats comparative ratios using metric-specific wording:
  - Time: `'command A' ran 1.50 ± 0.02 times faster than 'command B'`
  - Energy: `'command A' used 1.50 ± 0.02 times less energy than 'command B'`
  - Memory: `'command A' used 1.50 ± 0.02 times less memory than 'command B'`

### 5. Sorting & Exports
- `SortOrder::MeanTime` (default) sorts by the primary metric's mean value.
- JSON Export:
  - Includes `"metric": "<metric>"` at the top level of the JSON document.
  - Keeps existing `mean`, `stddev`, `times` (wall time) intact for backwards compatibility.
  - Adds `"primary_metric": { "metric": "...", "mean": ..., "stddev": ..., "samples": [...] }` if metric is not `wall`.

### 6. JSON contract note (`schemaVersion`, #240)
perfratio owns the JSON export contract (`output-schema-contract.md`, `BenchCommons/.product` `CONTRACTS.md`). This feature changes it additively:
- Top-level `"metric"` (string, `wall|cpu|user|system|energy|memory`), always emitted, `"wall"` by default.
- Per-result `"primary_metric"` object (`metric`, `mean`, `stddev`?, `samples`?), emitted only when `metric != wall`. Units: seconds (time metrics), joules (energy), bytes (memory).
- Existing fields (`mean`, `times`, `relative_speed`, ...) keep their meaning and stay wall-clock based. Only `relative_speed[_stddev]` and result ordering follow the active metric, which is why `metric` is exported.
- Version impact: new optional fields are a **MINOR** bump. Once `schemaVersion` lands on master (#240), this PR's fields ship with the first MINOR after `"1.0"` (`"1.1"`), and the field table in `output-schema-contract.md` must list `metric` and `primary_metric` in the same PR. Until then the export is unversioned (legacy shape) and consumers must ignore unknown keys.
- `--metric custom:<name>` (#90, see §8) adds the pattern `custom:<name>` as a `metric` value; `primary_metric.metric` carries it too. Shipped as `schemaVersion` `1.2` (MINOR: only emitted on explicit request).

### 7. Missing data and import
- `BenchmarkResult::primary_mean` returns `Option<f64>`. `None` means the result has no data for the metric (no energy reading, no memory samples). It is never replaced by 0, and a `None` result sorts after every result with data. Its relative speed is NaN, and `primary_metric` is omitted from its JSON entry.
- Under `--metric energy`, every run needs an energy reading. If any run (partial) or all runs (none) lack one, `finish()` fails with an error naming `--metric energy`. Outlier detection therefore never falls back to wall time.
- `--import-json` reads the top-level `metric`. A file whose `metric` differs from the CLI `--metric` is rejected. Files without the field (older exports) are accepted. Under `--metric energy|memory`, an imported result lacking data for the metric is rejected.

### 8. `--metric custom:<name>` (#90)
A value extracted with `--output-metric <name>=<regex>` (`custom-output-metrics.md`) can be the primary metric.
- **Direction:** lower is better, like every other metric, unless declared with `--output-metric 'NAME[unit,higher]=REGEX'` (throughput): then the largest value is best (#317, `metric-descriptors.md`).
- **Unit:** none. The value is used as printed; the terminal shows it with three decimals under the header `<name> (mean ± σ)`, and the secondary `Time` line stays. The summary reads `'A' had 2.50 ± 0.10 times lower value than 'B'`, followed by the absolute value and difference.
- **Validation (OptionsError::InvalidOutputMetric):** `<name>` must match `[A-Za-z0-9_]+` (case-sensitive), and an `--output-metric <name>=...` with the same name must be present. With `--import-json` the second check is skipped because nothing is extracted.
- **Missing values (checked in `finish()`, same null-alignment as #265):** every recorded run needs a value. If no run produced one, the benchmark fails with `'--metric custom:<name>' requires a value for every run, but no run produced '<name>'`. If only some did (possible with `-i`, which writes `null` for the others), it fails with `... but N of M runs have none for '<name>'`. This keeps ranking from silently using a subset of the runs.
- **Samples:** `primary_samples`/`primary_mean`/`primary_stddev` use the non-null entries of `custom_metrics[<name>]` and `custom_metrics_summary[<name>]`.
- **Import:** a file whose `metric` is not the CLI `--metric` is rejected as before. Under `--metric custom:<name>` a result with no values or with `null` entries for `<name>` is rejected.
- **Implementation note:** `Metric::Custom(&'static str)` keeps `Metric` `Copy`; names are interned (`src/metric.rs`).

## Verification Plan
- Unit tests:
  - CLI argument parsing for `--metric` with all valid variants and aliases.
  - `primary_samples` extraction for every `Metric` variant.
  - Comparison wording and formatting helpers.
- Integration tests:
  - `--metric cpu` on `sleep 0.1` vs a CPU-heavy command inverts relative ranking compared to `--metric wall`.
  - `--metric memory` verifies memory-based summary comparison.
  - `--metric energy` requires energy availability or exits with error code 1.
  - JSON export contains the top-level `"metric"` attribute.

  - Missing and partial energy fail the run (`benchmark/mod.rs` unit tests); import with mismatched `metric` or missing energy fails (`tests/metric_tests.rs`).
  - `--metric custom:<name>`: ranking by lower value, JSON `metric`/`primary_metric`, undefined name, no value, partial value (unix), malformed selector, import round trip (`tests/custom_metric_tests.rs`); `Metric` parse/serde unit tests.
