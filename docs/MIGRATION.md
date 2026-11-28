# Migrating to perfratio

This guide covers moving from [hyperfine](https://github.com/sharkdp/hyperfine) to perfratio 1.0. See [CHANGELOG.md](../CHANGELOG.md) for the full list of changes.

## Binary and package names

| From | To |
|---|---|
| `hyperfine` binary | `perfratio` |

## Environment variables

| Old | New | Status |
|---|---|---|
| `$HYPERFINE_ITERATION` | `$PERFRATIO_ITERATION` | removed in 1.0: no longer exported, no warning. Hyperfine compatibility is promised for flags and JSON import only, not for environment variables |
| `$HYPERFINE_RANDOMIZED_ENVIRONMENT_OFFSET` | `$PERFRATIO_RANDOMIZED_ENVIRONMENT_OFFSET` | renamed, no fallback |

The `{iteration}` placeholder ([#149](https://github.com/BenchCommons/perfratio/pull/149)) also works without a shell, so prefer it over the variable with `-N`.

No perfratio release shipped these aliases, so there is no deprecation period: scripts must use the `PERFRATIO_*` names ([#225](https://github.com/BenchCommons/perfratio/issues/225)).

## Flags

Every hyperfine 1.20 flag keeps its meaning. perfratio adds the flags below. `perfratio --help` is the reference.

Changed or aliased:

- `--ignore-exit-code` is an alias of `--ignore-failure` (which takes a comma-separated list of exit codes, as in hyperfine 1.20).
- Short flags were added for `--style` (`-l`), `--sort` (`-t`), `--show-output` (`-d`), `--output` (`-O`) and `--input` (`-I`).
- `--warmup` accepts `auto`.
- `--setup` and `--cleanup` can be given once per command, like `--prepare`.
- `--parameter-scan` and `--parameter-list` can be combined and repeated, and `--max-benchmarks` bounds the Cartesian product.
- `--sort=command` with `--reference` annotates the relative-speed table.
- Output shows the median, and percentiles with `--deep-stats`. `--deep-stats` respects `--time-unit`.
- Exports are written atomically and no longer truncated at startup. `/dev/stdout` and FIFOs work as export targets.
- `--reference` is honored in all exports.
- The command after `--` is run as an exact argv.
- Round-robin mode rejects parametrized `--setup`/`--cleanup` unless `--allow-setup-with-round-robin` is given, and the ambiguous alias for the common run count was dropped ([#145](https://github.com/BenchCommons/perfratio/pull/145)).

New:

| Area | Flags |
|---|---|
| Energy | `-E`, `--energy` (Linux RAPL: sysfs powercap with readable `energy_uj`, or the perf power PMU), `--energy-backend auto\|sysfs\|perf` (or `PERFRATIO_ENERGY_BACKEND`), `--energy-baseline DURATION` (idle power and net energy) |
| Statistics | `--deep-stats`, `--discard-outliers`, `--suppress-outlier-warnings`, `--suppress-warnings`, `--target-precision`, `--duration`, `--precision`, `--outlier-method modified-z\|iqr`, `--first-run include\|separate\|discard` |
| Comparison | `--metric wall\|cpu\|user\|system\|energy\|memory`, `--delta-table`, `--reference` |
| Run control | `--timeout`, `--until`, `--until-stderr`, `--subtract`, `--shell` per command, `--omit-failed-runs`, `--filter-failed`, `--show-output-on-failure`, `--schedule`, `--round-robin` |
| Parameters | `--parameter-file`, `--parameter-sample`, `--aggregate-parameter-runs`, `--expand-used-parameters`, `--seed`, `--max-benchmarks` |
| System | `--check-system`, `--affinity`, `--priority`, `--resource-usage` |
| Data | `--output-metric NAME=REGEX`, `--output-metric-stderr`, `--label KEY=VALUE`, `--import-json`, `--export-html`, `--export-markdown-runs`, `--export-orgmode-runs`, `--export-asciidoc-runs`, `--export-asciidoc` |
| CI | `--compare`, `--fail-if-regressed`, `--export-diff-markdown` |

`--output-metric` values are exported in `custom_metrics`, one entry per run aligned with `times`; a run where an ignored failure left the metric out is `null`. Speedup summary lines carry a significance marker (`[+]`, `[-]`, `[~]` without colors).

## JSON export


The current `schemaVersion` is `"1.39"`. perfratio writes run metadata under a top-level `perfratio` key, with `version`, `command_line`, `started_at`, `labels` and `system`. Files from hyperfine have no such key. `--import-json` reads hyperfine and perfratio files.

| Change | Since |
|---|---|
| `perfratio` metadata key | merged |
| Per-result `user_times`, `system_times`, `median`, `percentiles`, `geometric_mean`, `relative_speed`, `first_run`, `custom_metrics`, per-run parameters | merged |
| `"schemaVersion"` as the first key | 1.0 |
| `perfratio.measurements` with `time`, `memory`, `energy`, `resources` set to `measured`, `unavailable` or `not_requested`, plus `energy_unavailable_reason` | 1.0 |
| Top-level `metric` and per-result `primary_metric` (`--metric`) | 1.1 |
| `idle_watts` and `mean_net_energy_joules` (`--energy-baseline`) | 1.1 |
| Per-result `significance` and `p_value` against the reference | additive |
| `perfratio.energy_backend` (`--energy-backend`) | additive |
| `energy_samples` per result, JSON Schema file | not yet, see `docs/specs/output-schema-contract.md` |

Rules for consumers, from the schema contract:

- Read `schemaVersion` first. A file without it is the legacy shape.
- Accept any MINOR version of the MAJOR you support and ignore unknown keys.
- An unavailable value is omitted, never written as `0`. Check `perfratio.measurements.energy` before using energy fields.
- Units are seconds, bytes, joules and watts.

Details: [docs/specs/output-schema-contract.md](specs/output-schema-contract.md) (#240).

## Other behavior changes

- On Linux, wall time excludes fork/spawn overhead ([#208](https://github.com/BenchCommons/perfratio/pull/208), [#231](https://github.com/BenchCommons/perfratio/pull/231)), and per-run user, system and memory use `wait4`. Absolute times differ slightly from hyperfine.
- The winget package id is no longer `sharkdp.hyperfine` ([#132](https://github.com/BenchCommons/perfratio/pull/132)).
- Parameter values are inserted into commands verbatim; do not pass untrusted values ([#139](https://github.com/BenchCommons/perfratio/pull/139)).
