# Measuring the same algorithm across languages

Recommended perfratio flags for comparing implementations of one algorithm written in different languages (requirements from [#240](https://github.com/BenchCommons/perfratio/issues/240)). Every flag below exists in the current build (checked against `perfratio --help`).

```sh
perfratio -N --warmup auto --first-run=separate \
          --output-metric 'algo_ns=ALGO_NS=(\d+)' \
          --label lang=<lang> --label algorithm=<name> \
          --export-json result.json '<run command>'
```

| Flag | Why |
|---|---|
| `-N` (`--shell=none`) | Skips the shell, so shell startup is not charged to every language. |
| `--warmup auto` | Warms up until the last 5 timings agree within 1% (at most 100 runs). JIT and VM runtimes (JVM, .NET, V8, PyPy) reach steady state. Compiled languages stop after a few runs. |
| `--first-run=separate` | Reports the cold start (runtime boot, class loading, page cache) in `first_run` instead of mixing it into the statistics. |
| `--output-metric NAME=REGEX` | The program times only its algorithm and prints it, for example `ALGO_NS=12345`. The value lands in `custom_metrics`, apart from process startup, which `times` always includes. Name the unit in the metric (`algo_ns`). |
| `--region[=NAME]` | The program brackets only its algorithm with the region SDK (`sdk/`: C/C++, Rust, Python; see [region-sdk.md](region-sdk.md)). The region time lands in `results[].regions`, apart from startup, JIT warm-up and shutdown, and `--metric region:NAME` ranks on it. More robust than `--output-metric` because the protocol is versioned and uses a monotonic clock. |
| `--label KEY=VALUE` | Stores language and algorithm identity in the JSON (`perfratio.labels`), so consumers do not depend on file names. |
| `--export-json FILE` | Machine-readable result. See [specs/output-schema-contract.md](specs/output-schema-contract.md). |

## Optional flags

| Flag | Use |
|---|---|
| `-E`, `--energy` | Only on bare-metal Linux with readable RAPL `energy_uj`. Check `perfratio.measurements.energy` before using energy fields. `--energy-backend perf` uses the perf power PMU when `energy_uj` is not readable; `--energy-baseline DURATION` adds idle power and net energy. |
| `--min-runs N`, `--max-runs N` | Enough samples for very fast implementations. |
| `--duration DURATION` | Samples each command until the summed run time reaches DURATION (at least 3 runs), for example `--duration 5s` for fast implementations. Conflicts with `--runs` and `--target-precision`. |
| `--metric METRIC` | Ranks and compares by `wall`, `cpu`, `user`, `system`, `energy` or `memory` instead of wall time. The JSON records it as `metric`. |
| `--delta-table` | One table of wall time, peak memory, energy, power and CPU time against the reference command. |
| `--target-precision PCT` | Runs until the 95% confidence interval of the mean is within PCT of the mean. |
| `--affinity CPUS` | Pins to CPUs to reduce migration noise (Linux and Windows). |
| `--check-system` | Reports noise sources before benchmarking. |
| `--resource-usage` | Per-run OS resource counters. |
| `--discard-outliers` | Excludes outlier runs from the statistics. |
| `--schedule round-robin` | Interleaves commands to reduce temporal bias when several languages run in one invocation. |

## JSON fields for consumers

Read `schemaVersion` first (currently `"1.35"`), accept any 1.x and ignore unknown keys. Run metadata is under the `perfratio` key. Check `perfratio.measurements.energy` (`measured`, `unavailable` or `not_requested`) before reading energy fields. With `--output-metric-stderr` the metric is read from stderr instead of stdout.

Correctness checks of the algorithm output and per-language builds belong to the catalog contract in tools-benchmark, not to perfratio.
