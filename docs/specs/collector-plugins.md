# Spec: collector plugin protocol, `--collector-plugin` (#384)

**Status:** Implemented, `schemaVersion` 1.44 (additive, MINOR). Part of the output contract of #240. Parent: #365.
Related: #317 (`metric-descriptors.md`), #383 (`valgrind-collector.md`).

## Problem / Motivation
Every new metric source (a GPU power sensor, a battery gauge, a custom hardware counter) needed
a change in perfratio. A versioned protocol lets an external program supply metrics that enter
the descriptor registry (#317) and the JSON export without touching perfratio.

## Contract

### CLI
`--collector-plugin PATH`, repeatable. `PATH` is an executable and must contain a directory part (`./plugin`, not `plugin`); a bare name is an error. perfratio runs it directly: no
shell, no arguments, working directory of perfratio. Configure a plugin through its environment
(it inherits perfratio's) or through its own config file. Not combinable with `--concurrency N`
(N > 1) or `--jobs N` (N > 1): a plugin observes one run at a time (options error).

### Transport
UTF-8 JSON, one object per line, in both directions, over the plugin's stdin and stdout. The
plugin's stderr goes to the stderr of perfratio. A reply line is at most 65536 bytes. perfratio
is always the requester; the plugin answers each request with exactly one line.

### Messages (protocol version 1)

| perfratio sends | plugin replies |
|---|---|
| `{"type":"hello","protocol":1}` | `{"type":"hello","protocol":1,"name":"gpu","metrics":[{"id":"energy","unit":"J"}]}` (within 10 s) |
| `{"type":"start","run":N}` | `{"type":"ok"}` |
| `{"type":"stop","run":N}` | `{"type":"ok"}` |
| `{"type":"read","run":N}` | `{"type":"metrics","values":{"energy":1.5}}` |
| `{"type":"shutdown"}` | none; perfratio then closes stdin and the plugin exits |

Any request may be answered with `{"type":"error","message":"..."}`. Replies other than `hello`
arrive within 5 s.

Sequence for each timed run `N` (the 0-based benchmark run, warmup and `--prepare`/`--conclude`
are not bracketed): `start`, spawn of the command, exit of the command, `stop`, `read`.
`start` is sent before the command is spawned and `stop` after it has exited, so the round trips
are outside the timed interval. A plugin therefore measures an interval (a device, the system,
a package), not "the pid"; it is not told which process to watch. For a metric that needs the
process itself (a simulator that must be the parent), the protocol does not fit: see
`valgrind-collector.md`.

`hello` reply fields:

| Field | Rules |
|---|---|
| `protocol` | must equal 1; checked before anything else |
| `name` | `[a-z][a-z0-9_]{0,31}`; unique among plugins; `cachegrind` and `callgrind` are reserved |
| `metrics[]` | 1 to 64 entries |
| `metrics[].id` | `[a-z][a-z0-9_]{0,31}`, unique within the plugin |
| `metrics[].unit` | 1 to 16 characters, no whitespace, control characters, `[`, `]`, `,` or `=` |
| `metrics[].direction` | optional: `lower_is_better` (default), `higher_is_better`, `neutral` |
| `metrics[].kind` | optional: `measured` (default), `estimated`, `simulated`, `derived` |
| `metrics[].scope` | optional: `process`, `process_tree`, `batch`, `package`, `system` (default) |

`values` of a `metrics` reply maps a declared id to a finite number, or to `null` (no value this
run). An undeclared id, a string, a non-finite number, a wrong reply type, an `error` reply,
a reply over the size limit, a plugin that exits or that does not answer in time is an error
that stops the benchmark with a message naming the plugin (a plugin that does not answer is
killed). Protocol 2 would be negotiated by the `protocol` field of `hello`; a plugin that
speaks another version is rejected.

### Trust model
- A plugin is **trusted code**: it runs with the privileges and environment of the user who
  runs perfratio, exactly like the benchmarked command. perfratio does not sandbox it. Run only
  plugins you would run by hand. The path is never searched on `PATH` and never passed through
  a shell, so no quoting or expansion of the path can change what runs.
- Everything a plugin **sends** is untrusted data. Names, ids, units and values are validated
  against the rules above before they reach the export; text from an `error` message is cut to
  200 printable characters before it is shown. A plugin cannot add keys outside
  `results[].collector_metrics`, name a metric after a perfratio metric, or inject non-finite
  numbers into the statistics.
- perfratio never sends the plugin the command line, the output of the command or the results.
  The messages carry only the protocol version and the run number.
- A plugin is stopped when perfratio ends (`shutdown`, then stdin is closed, then it is killed
  after 0.5 s).

### JSON export (`schemaVersion` 1.44)
Additive; files without the keys are unchanged and import as before.

- `results[].collector_metrics`: object from `<name>.<id>` to an array with one entry per run
  (`null` where the plugin had no value). Optional: absent without a collector.
- `results[].collector_metrics_summary`: object from the same keys to `{mean, stddev?, median,
  min, max}` of the present values.
- `perfratio.metrics[]`: one descriptor per key, id `collector.<name>.<id>`, with the declared
  `unit`, `direction`, `kind` and `scope`, `source` `plugin:<name>`, `fields`
  `["collector_metrics","collector_metrics_summary"]`. `status` is `available`, or `partial`
  with reason `no_value_in_some_runs` when some run has `null`. A file imported from an export
  carries no declaration: its keys are described as unit `unitless`, `neutral`, `measured`.
- The keys are not primary metrics: `--metric` and ranking do not read them, and `--compare`
  and the delta table ignore them. They are in the descriptor registry (`DYNAMIC_FIELDS` of
  `src/metric_registry.rs`).

### Versioning
`SCHEMA_VERSION` 1.44 (1.43 is reserved for another pull request of the 1.x series, merged
before this one). The keys do not collide.

## Example collector
`examples/collector_plugin.rs` is a complete plugin (`cargo build --example collector_plugin`).
It reports the time between `start` and `stop` as `elapsed` and counts runs. With
`EXAMPLE_COLLECTOR_MODE` it misbehaves on purpose for the tests.

## Tests
- `src/collector.rs`: `hello` and `metrics` validation, line limit.
- `tests/collector_tests.rs`: a full run against the example plugin (values per run, none for
  warmup, descriptors), five protocol violations, a missing plugin, rejection with
  `--concurrency` and `--jobs`.
- `tests/json_schema_tests.rs`: the export with a plugin validates against the strict schema.
- `tests/metric_descriptor_tests.rs`: the new result keys are covered by the registry.

## Limitations
- No per-process attachment, no early-exit notification: `stop` follows the exit of the command.
- One plugin request is in flight at a time, so plugins cannot be used with `--jobs` or
  `--concurrency`.
- Plugins that fail abort the benchmark; there is no "continue without this metric" mode.

## References
#384, #317, #240, `metric-descriptors.md`, `valgrind-collector.md`; PAPI components.
