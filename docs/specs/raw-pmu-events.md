# Spec: Raw PMU events and event registry (#377)

## Problem / Motivation
`--counters` accepted a fixed list. Vendor events could not be requested and the
user could not see which events the CPU supports.

## Contract
`--counters` items, next to the built-in names. Each takes the `:u` / `:k`
modifier (`perf` style `/u` after the closing slash is accepted too) and follows `--scope`.

| Syntax | Meaning |
|---|---|
| `raw:r01c2` or `r01c2` | `PERF_TYPE_RAW`, config `0x01c2` (1 to 16 hex digits) |
| `cpu/event=0x3c,umask=0x01/` | terms placed with `<pmu>/format/<term>` bit ranges (`config`, `config1`, `config2` too), perf type from `<pmu>/type`. A term without `=` is a flag (1). Values are hex (`0x`) or decimal; a value wider than its field is an error |
| `cpu/mem-loads/` | named event: `<pmu>/events/<name>` holds the terms |
| `mem-loads` | a name that is not built in is looked up as a named event of `cpu`, then `cpu_core` |

Items are split at commas outside `pmu/.../`. PMU and event names are limited
to `[A-Za-z0-9_.-]`, so they cannot leave the sysfs root.

`perfratio --list-events` prints the built-in events, the raw syntax and, per
PMU under `/sys/bus/event_source/devices`, its type, terms and named events.
PMUs with a `cpumask` (uncore) are marked system-wide: they count per socket,
and `perf_event_open` on a benchmarked process refuses them, so the event is
reported unavailable with the usual single warning.

### JSON (MINOR, schemaVersion 1.37)
Raw events add keys to `results[].counters`, `counters_summary` and
`counters_coverage`: `r01c2`, `cpu/event=0x3c,umask=0x01/`, `cpu/mem-loads/`
(`rNNNN` keys are lowercased; `pmu/.../` keys keep the case as written, with no spaces; no
modifier) plus `:u` / `:k` as for built-in events.
No new field; no change for consumers that accept unknown keys.

### Behavior
* Raw events are never part of derived metrics (IPC, miss rates).
* `--debug-mode` gives every raw event a deterministic 1,000,000 counts/s.
* Non-Linux: the usual "only supported on Linux" warning. `pmu/.../` syntax
  needs `/sys` to resolve and fails to parse elsewhere with "PMU not found".
* Raw events are opened per process, like built-in events (`config1`/`config2`
  go into `perf_event_attr`).

## Tests
Parser and sysfs discovery against a fake tree (`src/pmu_events.rs`), CLI via
`--debug-mode` (`tests/counters_tests.rs`). Not run on hardware: `perf_event_open`
with raw configs and the real sysfs layouts (tracked in #337).

## References
perf_event_open(2), PAPI, #337
