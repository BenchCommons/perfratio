# Spec: Metric descriptors and status taxonomy (`perfratio.metrics`) (#317)

Shipped in `schemaVersion` 1.20 (additive, MINOR). Part of the output contract of #240.

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
  `unitless` and `lower_is_better` (the documented default); declaring unit and direction in
  `--output-metric` is not implemented yet.
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

## Not done (remaining of #317)
- `--output-metric NAME=REGEX[:unit][:higher]` and direction-aware ranking, delta table and
  `--compare` (they still assume lower is better).
- Reading descriptors back in `--import-json`.

## References
#317, #240, #319, `output-schema-contract.md`.
