# Spec: Custom Output Metrics (`--output-metric 'NAME=REGEX'`) (#90)

## Problem / Motivation
References: `sharkdp/hyperfine#170`, `#370`, `#718`, `#90`.

Many command-line utilities, benchmarks, compilers, and test suites output domain-specific numeric metrics during execution (e.g. database query latency, transactions per second, compilation phase timing, iteration counts, memory allocated).

Currently, users can only benchmark wall-clock time (and OS rusage/energy). To measure domain metrics across multiple runs with statistical rigor (mean, stddev, median, min, max, percentiles), users must write external parsing scripts.

## Contract

```
--output-metric <NAME[unit,higher]=REGEX>
                               Extract a custom numeric metric from the command's stdout.
                               NAME must be alphanumeric/underscores.
                               REGEX must contain at least one capture group.
                               The metric value is parsed as a float from the first
                               capture group of the last match in the output.
                               Can be specified multiple times for different metrics.
--output-metric-stderr         Read the metrics from stderr instead of stdout
                               (requires --output-metric; applies to all metrics).
```

### 1. CLI Validation & Format
- Argument syntax: `<NAME>=<REGEX>` or `<NAME>[unit,higher]=<REGEX>`: the optional brackets declare a unit and `higher` (larger is better) or `lower` (default), see `metric-descriptors.md` (#317, `schemaVersion` 1.28).
  - `NAME`: non-empty, ASCII alphanumeric and underscores only (`[a-zA-Z0-9_]+`).
  - `REGEX`: valid regular expression containing at least one capturing group (`captures_len() >= 2`).
- Invalid name, missing `=`, invalid regex, or missing capture group causes `OptionsError::InvalidOutputMetric(String)` with an actionable explanation.
- Conflicts with `--show-output` (`-d`) and `--until` because the stream cannot be both displayed/consumed and captured safely.
- Compatible with `--output=null` (default) and `--output=pipe`: the captured stream is piped to perfratio either way. `--output=inherit` and `--output=<file>` are rejected with `OptionsError::InvalidOutputMetric` (checked after option parsing, so it also covers per-command lists).
- `--output-metric-stderr` requires `--output-metric`. The other stream keeps its `--output` policy.

### 2. Execution & Extraction
- When `--output-metric` is specified, the benchmark runner captures stdout, or stderr with `--output-metric-stderr` (bounded up to 1 MiB tail buffer to protect against unbounded memory growth).
- For each metric, the regular expression is executed against the captured output:
  - If multiple matches occur, the **last** match in the stream is selected.
  - The value of capture group 1 is parsed as an `f64`.
  - If no match occurs, or the capture cannot be parsed as a float:
    - The run fails with error: `Metric '<NAME>' could not be extracted from output in <when>`.
    - If `-i` / `--ignore-failure` is active, the failure is ignored according to standard failure handling policies: a warning is printed and the run is kept. Its metric value is **missing** (see "Alignment").

#### Alignment of `times` and `custom_metrics`
Every list in `custom_metrics` has exactly one entry per recorded run, in the same order as `times` (#90). A run whose metric was not extracted (only possible with `-i`) contributes `null`. Statistics (`custom_metrics_summary`, terminal, CSV) are computed over the non-null values only; a metric with no value at all has no summary entry. Example, 3 runs with the metric missing in run 2: `"times": [t0, t1, t2]`, `"custom_metrics": {"x": [10.0, null, 12.0]}`, summary mean `11.0`. When the first run is excluded from the statistics (first-run policy), its entry is dropped from every list together.

### 3. Statistics & Reporting
- For each metric across successful runs:
  - `mean`: arithmetic mean.
  - `stddev`: sample standard deviation (when runs > 1).
  - `median`: 50th percentile.
  - `min`, `max`: extreme values.
- Terminal Output:
  Printed underneath `Time (mean ± σ)`:
  ```
    <name> (mean ± σ):    <mean> ± <stddev>
  ```
- JSON Export:
  Each benchmark entry in `"results"` includes:
  `"custom_metrics": { "<name>": [<val1>, null, <val3>, ...] }` (`number | null` per run, aligned with `times`).
- CSV Export:
  For each metric, columns are appended:
  `mean_<name>`, `stddev_<name>`, `median_<name>`, `min_<name>`, `max_<name>`.

## Contract version
`custom_metrics` elements changed from `f64` to `f64 | null` and the lists are now aligned with `times`. `schemaVersion` is not emitted yet (see `output-schema-contract.md`), so this is part of the shape that `1.0` will freeze: consumers (tools-benchmark) must accept `null` elements. `output-schema-contract.md` is updated accordingly.

## Primary metric: `--metric custom:<name>` (#90, #48)
A metric extracted with `--output-metric` can be the primary metric for ranking, relative speed, summary and exports. Lower is better unless declared `higher`. Every recorded run needs a value: an undefined `<name>` is an options error, and a metric that no run (or only some runs, with `-i`) produced fails the benchmark, reusing the alignment rules above. Full contract in `metric-target.md` §8; JSON `schemaVersion` `1.2` (`output-schema-contract.md`).

## Verification Plan
- Unit tests:
  - `OutputMetric::from_str`: valid formats, missing `=`, bad names, missing capture groups, bad regex.
  - `OutputMetric::extract`: single match, multiple matches (selects last), float formats (integer, decimal, scientific notation), non-numeric capture, no match.
- Integration tests:
  - Single `--output-metric` with command printing metric.
  - Multiple `--output-metric` flags extracting independent metrics.
  - Missing metric causing command failure.
  - Missing metric ignored with `-i`, including JSON alignment (`[10, null, 12]` vs 3 `times`, summary over non-null values).
  - `--output=pipe` works; `--output=inherit` is rejected.
  - `--output-metric-stderr` reads stderr and fails when the metric is only on stdout; requires `--output-metric`.
  - JSON export contains `"custom_metrics"`.
  - CSV export contains `mean_<name>` columns.
  - Conflicts with `--show-output`.
  - `--metric custom:<name>` selector, errors and ranking: `tests/custom_metric_tests.rs`.
