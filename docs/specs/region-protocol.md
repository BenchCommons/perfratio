# Spec: Region timing protocol (`--region`) (#310)

**Status:** Implemented (protocol 1, schemaVersion 1.19). Reference clients for C/C++, Rust and
Python are specified in `region-sdks.md` (#311).

## Problem / Motivation
References: #310, #311, #240, #79 (`--until`), #56 (`--subtract`), #90 (`--output-metric`).

The wall time of a process includes interpreter or VM start-up, JIT warm-up, data loading and
shutdown. For cross-language comparisons (`catalog-algorithms`) these dominate short algorithms
and make languages differ for reasons unrelated to the algorithm. `--output-metric` lets a
program print its own timing, but that is unversioned, unsynchronised and tied to a regex.

With `--region` the program brackets the code of interest with `start(name)` / `stop(name)`
calls of a small client. perfratio reports the time inside the region next to the
whole-process time. Idea after megafine's `--region` and LIKWID's marker API; the protocol is an
independent design (megafine is GPL-3.0, no code copied).

## Contract

### CLI
```
--region[=NAME]        Measure the region NAME (default: "default"). Repeatable.
--metric region:NAME   Rank on the time inside the region NAME (implies --region=NAME).
```
`NAME` is 1 to 64 ASCII letters, digits or `_`. The `=` is required so that `--region cmd` does
not swallow the command. `--region` conflicts with `--concurrency` and `--subtract`.

### Transport
For every **benchmark run** (not for warmup, setup, prepare, conclude or cleanup commands)
perfratio creates an empty private file (mode 0600 on Unix) in the temporary directory and passes
it to the command:

| Variable | Value |
|---|---|
| `PERFRATIO_REGION_FILE` | Absolute path of the event file. The client appends to it. |
| `PERFRATIO_REGION_PROTOCOL` | `1`, the protocol version perfratio speaks. |

perfratio reads and deletes the file when the run ends. The file replaces the pipe or handle
passed by descriptor that the issue sketched: a path works the same on Linux, macOS and Windows
without handle inheritance, cannot block the program when a pipe buffer fills, and survives
grandchild processes that keep the descriptor open. Clients buffer events in memory and write
them once, at `flush()` or on exit, so no I/O happens inside a region.

**No-op without perfratio.** When `PERFRATIO_REGION_FILE` is unset or empty (no perfratio, or a
warmup run), clients must do nothing: `start`/`stop` return immediately and never fail.

### Wire format, protocol 1
UTF-8 text, one event per line, `\n` or `\r\n`. Blank lines and lines starting with `#` are
ignored.

```
hello <protocol> [<sdk-id>]
start <name> <t_ns>
stop <name> <t_ns>
```

- `hello` is the first line, exactly once. `<protocol>` is the version the client implements.
  `<sdk-id>` is an optional token without spaces (for example `rust/1.1.0`), exported as `sdk`.
- `<t_ns>` is an unsigned decimal count of nanoseconds read from the **monotonic** clock
  (`CLOCK_MONOTONIC`, `mach_continuous_time`/`mach_absolute_time`, `QueryPerformanceCounter`).
  Only differences inside one file are used, so the epoch is irrelevant. Wall-clock (adjustable)
  clocks are not allowed.
- Events appear in chronological order.

### Semantics
- A region's time in a run is the sum of all its `start`/`stop` pairs; the number of pairs is
  `count_per_run`.
- Regions of **different** names may nest or overlap. A second `start` of a name that is still
  open, a `stop` without a `start`, a `stop` earlier than its `start`, and a `start` never
  stopped are errors that stop the benchmark with a message naming the region and the line.
- Version 1 is single-thread per name: concurrent use of one name from several threads is not
  defined. Use different names per thread.
- Names that were not requested with `--region` are validated and ignored.

### Errors
| Situation | Behaviour |
|---|---|
| The program wrote nothing (no `hello`) in a successful run | Error: "reported no region ... must bracket the code with the perfratio region SDK". A failed run that wrote nothing (ignored with `-i`) counts as a run without the region. |
| `hello <n>` with `n` different from 1 | Error "region protocol mismatch: the program speaks protocol n but this perfratio speaks protocol 1". |
| Malformed line, invalid name, unknown event | Error with the line number. |
| A requested region reported by no run | Error "The program never reported the region 'x'" (usually a typo). |
| A run reports others but not the requested region | The run's entry is `null`, never 0. The region's `status` is `region_missing` and the run is left out of its statistics. With `--metric region:x` every run must report `x`, otherwise error. |
| Event file larger than 64 MiB | Error (regions started in a tight loop). |

### Scope of the other metrics
User and system CPU time, peak RSS, hardware counters, resource usage and energy stay **whole
process**. Only the metrics under `regions` and `--metric region:<name>` are region scoped. A
later issue may add region-scoped collection.

### JSON (schemaVersion 1.19, additive)
```jsonc
"perfratio": { "region_protocol": 1, "measurements": { "region": "measured" } },
"results": [{
  "times": [0.52, 0.51],                       // whole process, unchanged
  "regions": [{
    "name": "sort",
    "status": "measured",                      // or "region_missing"
    "mean": 0.31, "stddev": 0.002, "min": 0.309, "max": 0.311,
    "count_per_run": [1, 1],                   // null where the run did not report it
    "times": [0.309, 0.311],                   // null where the run did not report it
    "sdk": "rust/1.1.0"                        // when the client sent one
  }]
}]
```
`perfratio.region_protocol` and `measurements.region` (`measured` | `not_requested`) are always
about the request: `measured` means `--region` or `--metric region:<name>` was given. With
`--metric region:<name>` the document's `metric` is `region:<name>` and `primary_metric` carries
the region samples. Without `--region` none of the new keys appear except
`measurements.region: "not_requested"`.

Terminal output prints one `Region NAME (mean ± σ)` line per region that is not the primary
metric.

## Implementation
- `src/region.rs`: protocol constants, parser (`parse`), per-run file (`RunFile`).
- `src/benchmark/executor.rs`: creates the file for benchmark runs, sets the variables, reads the
  report (`collect_regions`). The mock executor of `--debug-mode` reports every requested region
  as half of the simulated run time, one pair.
- `src/benchmark/mod.rs`: keeps the per-run series next to the custom metrics under the keys
  `region:<name>` and `region_count:<name>` (so first-run exclusion and omitted failed runs
  align them), exports them as `regions`.
- `src/metric.rs`: `--metric region:<name>` (the `Custom` variant with a `region:` key).

## Verification
- `src/region.rs` unit tests, including a seeded randomized test that pairing and summation hold
  for random interleavings of intervals across names.
- `tests/region_tests.rs`: `--debug-mode` tests (all platforms) and a shell fake-SDK
  (Unix): summation, missing region, uninstrumented command, version mismatch, overlap.
- `tests/json_schema_tests.rs` validates exports with regions against the JSON Schema.
- Real SDKs: `tests/sdk_tests.rs` (#311).

## Out of scope
Region-scoped CPU time, counters and energy; several threads per name; nested same-name
regions; streaming events while the program runs.
