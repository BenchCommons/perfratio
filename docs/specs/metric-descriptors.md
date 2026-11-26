# Spec: Metric descriptors and status taxonomy (`perfratio.metrics`) (#317)

Shipped in `schemaVersion` 1.20 (additive, MINOR); unit and direction of custom metrics and descriptor import in 1.28. Part of the output contract of #240.

## Problem / Motivation
Every metric of the export was a special case: fixed fields for time, memory, energy and counters,
custom `--output-metric` values with an implied unit and an implied "lower is better". A consumer
could not tell the unit, the direction of "better", what the value covers, whether it was
measured or derived, or why it is missing.

## Contract
`perfratio.metrics` is an array with one descriptor per metric of the export. It is generated
from one registry in code (`src/metric_registry.rs`, `REGISTRY` plus the names known only at run
time), so a metric that is exported has a descriptor. The flat fields of `results[]` do not
change; descriptors point to them through `fields`.

```json
{ "id": "cpu.ipc", "unit": "ratio", "direction": "higher_is_better", "scope": "process_tree",
  "source": "perfratio.derived", "kind": "derived", "status": "available",
  "fields": ["efficiency"] }
```

| Key | Values (all open enums except where noted) |
|---|---|
| `id` | dotted lowercase: `time.wall`, `time.user`, `time.system`, `cpu.utilization`, `memory.peak_rss`, `energy.total`, `energy.net`, `energy.domain.<name>`, `power.average`, `power.idle`, `resources.<field>`, `counter.<key>` (key as in `results[].counters`, with its `:u`/`:k` suffix), `cpu.ipc`, `cpu.cpi`, `cache.miss_rate`, `branch.miss_rate`, `cpu.effective_frequency`, `topdown.<class>`, `simd.<bucket>`, `simd.vectorization_ratio`, `batch.throughput`, `batch.instance_time`, `custom.<name>`, `region.<name>` (seconds, scope `region`, new in 1.21, `region-protocol.md`) |
| `unit` | `s`, `bytes`, `J`, `W`, `Hz`, `ns`, `count`, `ratio`, `percent`, `runs/s`, `unitless` (custom metrics) |
| `direction` | `lower_is_better`, `higher_is_better`, `neutral` |
| `scope` | `process`, `process_tree`, `batch`, `package`, `system`, `region`. Energy is `system` for one instance and `batch` with `--concurrency` (see `results[].energy_scope`). |
| `source` | where the value comes from: `perfratio.timer`, `os.rusage`, `linux.perf_event_open`, `perfratio.derived`, `command_output`, or the energy backend (`perfratio.energy_backend`) |
| `kind` | `measured`, `estimated`, `simulated`, `derived` |
| `status` | `available`, `partial`, `unsupported`, `permission_denied`, `below_resolution`, `contaminated`, `invalid`, `not_requested` |
| `reason` | present when `status` is not `available`; the same tokens as `perfratio.measurements.*_unavailable_reason` |
| `fields` | keys of `results[]` that hold the values |

Rules:
- A metric group that was not requested is not listed, so `not_requested` does not appear in the
  table today. Time and memory are always listed; the `batch.*` entries only with `--concurrency`.
- The status follows `perfratio.measurements`: `measured` is `available`, `partial` is `partial`,
  `unavailable` is `permission_denied` for `perf_paranoid`, `kernel_scope_denied`,
  `rapl_unreadable` and `emi_unreadable`, and `unsupported` for every other reason.
- An unavailable metric keeps its descriptor with the reason and has **no values** in `results[]`.
  It is never written as `0`. For `--counters` with an unusable PMU the group is described by
  the single id `counter`; with partial counters every requested event that could not be opened
  has its own `counter.<key>` descriptor.
- A counter or custom metric that has no value in some runs is `partial`. Custom metrics are
  `unitless` and `lower_is_better` unless declared with `--output-metric` (below).
- `status`, `reason`, `kind`, `scope`, `unit` and `direction` are open enums: consumers tolerate
  new values.
- `docs/schema/perfratio-export.schema.json` describes `perfratio.metrics[]`
  (`$defs/metricDescriptor`).

## Compatibility
Additive: MINOR bump 1.19 to 1.20. Files without `perfratio.metrics` (older exports) still
import; `--import-json` ignores the key and the descriptors are regenerated on the next export.

## Tests
- `tests/metric_descriptor_tests.rs`: every key of `$defs/result` in the schema is in `REGISTRY`,
  `NON_METRIC_FIELDS` or `DYNAMIC_FIELDS`; for exports of 11 flag combinations every result key
  and every counter, energy-domain and custom name has a descriptor; unavailable and partial
  groups are described and carry no values.
- Unit tests in `src/metric_registry.rs` for ids, directions and the status mapping.

## Declaring unit and direction (1.28)
`--output-metric NAME[unit,higher]=REGEX`. The brackets are optional and follow `NAME`:

| Form | Meaning |
|---|---|
| `NAME=REGEX` | unitless, lower is better (unchanged) |
| `NAME[ms]=REGEX` | unit `ms`, lower is better |
| `NAME[higher]=REGEX` | unitless, higher is better |
| `NAME[ops/s,higher]=REGEX` | unit and direction, in either order; `lower` is accepted and is the default |

A unit is 1 to 16 characters without whitespace, control characters, `[`, `]`, `,` or `=`. An
empty item, a repeated unit or direction, more than two items or a missing `]` is an options
error. The declaration sets `unit` and `direction` of the `custom.<NAME>` descriptor; the flat
fields (`custom_metrics`, `custom_metrics_summary`) are unchanged.

Direction is read by:
- ranking and the summary of `--metric custom:<NAME>`: the largest value is the reference and
  `relative_speed` stays >= 1 (best over worst value); the words are "higher value" / "lower
  value" so that "X had 2.50 times higher value than Y" is the better one first;
- `--sort`/`--sort-order` by mean, `best_of`, and the bootstrap interval of the ratio;
- `--pareto` and `--constraint`: the default direction of an axis `custom:<NAME>`;
  `--objective` still overrides it;
- the exported descriptor, and the terminal (`NAME [unit] (mean ± σ)`, and the unit next to
  the value and difference of the summary).

`--compare` and the delta table (#212) only handle wall time (always lower is better), so they
have nothing to flip. `--fail-if-regressed` budgets apply to wall time too.

## Import (1.28)
`--import-json` reads `perfratio.metrics[]` of the file: for each `custom.<name>` descriptor
whose name is in the results, `unit` and `direction` (`higher_is_better`; every other value
means lower) are used for ranking and written again by the next export. A declaration on the
command line wins over the file. Descriptors are untrusted: a unit that breaks the rules above
is dropped, other ids and malformed entries are ignored. Files without descriptors import as
before (lower is better).

## Tests (1.28)
`tests/metric_direction_tests.rs`, unit tests in `src/output_metric.rs` (syntax) and
`src/benchmark/relative_speed.rs` (direction-aware ranking).

## References
#317, #240, #319, `output-schema-contract.md`.
