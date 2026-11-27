# Spec: Region-scoped CPU time, energy and hardware counters (`--region-scope`) (#371)

**Status:** Implemented (schemaVersion 1.33). Linux: CPU time, energy (RAPL), counters. macOS:
CPU time, energy (IOReport). Windows: CPU time, energy (EMI). Counters need Linux. Bare-metal
check of the energy path: #337.

## Problem / Motivation
References: #371, #310 (`region-protocol.md`), #49, #240.

Region timing (#310) scopes only wall time. CPU time, counters and energy still cover the whole
process, so interpreter start-up, JIT warm-up and data loading leak into cross-language
comparisons. `--region-scope` reports these values for the code inside the regions.

## Contract

### CLI
```
--region-scope     With --region / --metric region:NAME: also report CPU time, energy
                   (with --energy) and counters (with --counters) inside each region.
```
Without `--region` or `--metric region:NAME` the flag is an error. It conflicts with
`--concurrency` and `--subtract` like `--region`. Whole-process values are unchanged.
Not done: `--metric region:NAME:cpu|energy|instructions`. `Metric` is `Copy` and the region
selector is a time key, so ranking on a scoped value needs its own variant. The values are in
the JSON export.

### Protocol: live markers
protocol 1 stays as it is. perfratio sets `PERFRATIO_REGION_LIVE=1` for the benchmark runs;
a client that sees `1` writes every marker to the event file when it records it (instead of at
exit), so a watcher can see it. Clients that ignore the variable still work: perfratio reads
the markers only when the program exits, so the region has no sample and the scoped values are
`null`. The three reference SDKs (C, Rust, Python) support it. The `t_ns` timestamps are the same
and the file content is identical, so the timing regions of #310 do not change.

### How the values are read
A watcher thread of perfratio polls the event file (`POLL`, 25 µs; the effective interval is
the timer granularity of the OS: about 60 µs on Linux, 1 ms or more on Windows). For every
`start`/`stop` line it takes one sample of the child:

| Value | Linux | macOS | Windows |
|---|---|---|---|
| CPU user/system | `/proc/<pid>/stat` (`utime`, `stime`; pid reuse is detected with `starttime`) | `proc_pidinfo(PROC_PIDTASKINFO)` (validated on Apple Silicon only: on the Intel CI runner it advanced by 7 ms during a 0.3 s busy loop, so treat Intel Mac values as unverified) | `GetProcessTimes` on the child handle |
| Energy | the `EnergySampler` of `--energy-backend`: the window is closed and reopened at each marker and the windows are summed | IOReport | EMI |
| Counters | `perf_event_open` on the child pid, opened after the spawn, counting from then on | none | none |

The value of a region in a run is the sum of `sample(stop) - sample(start)` over its pairs.
Negative differences count as 0. A marker that has no sample (child already gone, counter not
readable) makes that value `null` for the run, never 0. After the child exited no sample is
taken.

### Error bound
A sample lags its marker by the delay `D` between the client's clock read and the read of the
child: the client's write (a few µs), the poll interval (up to about 60 µs on Linux, up to
1 ms or more on Windows) and the read itself (CPU: a syscall, a few µs; energy: a sampler read,
tens of µs for RAPL sysfs, ms for IOReport and EMI). For one region the error of a value is

```
|error| <= D_stop_max * rate + D_start_max * rate
```
where `rate` is the value's rate of change: at most 1 s of CPU time per second per running
thread, the package power for energy (about 10 to 200 W), the instruction rate for counters.
Both markers are late by similar amounts, so the errors cancel to `|D_stop - D_start| * rate`
for a steady workload. Examples on Linux with `D` = 100 µs: CPU time at most 0.2 ms, energy
at most 4 mJ at 20 W. A region should last much longer than `D` (at least 100 `D`, i.e. 10 ms,
for 1% on Linux). The SDK's write is made after the start's clock read, so it adds 5 to 20 µs
inside the region and lies outside it after a stop. Use more runs for short regions, and
`--region-scope` is not meant for regions shorter than a millisecond.

### What is counted
- CPU time and counters are those of the process perfratio started. With a shell (the default)
  that is the shell unless it replaces itself with the command. Use `-N`/`--shell=none` for exact
  values. Descendants are not included.
- Counters count the initial thread of the program (`perf_event_open` on the pid, no
  `inherit`: the kernel adds the counts of children only when they exit). A multi-threaded
  region undercounts. Per-region counting of all threads needs `inherit` at spawn and is not
  possible with mid-run reads.
- Energy is machine-wide (RAPL package, IOReport, EMI), as in the whole-process value, so it
  includes other processes. The same caveats as `--energy` apply.
- The watcher thread uses CPU time of perfratio, not of the child, but it competes with the
  program for a core on a saturated machine. Do not combine with `--affinity` to one CPU shared
  with the watcher without checking the result.

### JSON (schemaVersion 1.33, additive)
```jsonc
"regions": [{
  "name": "sort", "status": "measured", "times": [0.31, 0.30], ...,
  "metrics": [                                   // only with --region-scope
    { "name": "cpu_user", "unit": "s", "scope": "region",
      "mean": 0.29, "stddev": 0.01, "min": 0.28, "max": 0.30, "values": [0.30, 0.28] },
    { "name": "cpu_system", "unit": "s", "scope": "region", ... },
    { "name": "energy", "unit": "J", "scope": "region", ... },       // with --energy
    { "name": "instructions", "unit": "count", "scope": "region", ... } // with --counters
  ]
}]
```
`metrics` is omitted without `--region-scope` and when no run has a value. `values` is aligned
with `results[].times`; `null` means not readable. `results[].user`, `system`, `energy_joules`
and `counters` keep their whole-process meaning.

## Implementation
- `src/region_scope.rs`: line scan, pairing (`totals`), samples (`Source`, `LiveSource`), the
  watcher, CPU readers per OS.
- `src/timer/mod.rs`: `observe_spawn`, a one-shot hook that runs right after the spawn.
- `src/perf_counters.rs`: `open_for_pid`.
- `src/benchmark/executor.rs`: sets `PERFRATIO_REGION_LIVE`, installs the watcher, stores the
  values as series `region_scope:<name>:<metric>`; the mock executor simulates them.
- `src/benchmark/benchmark_result.rs`: `RegionMetric`.

## Verification
- Unit tests with injected readings (`src/region_scope.rs`): deltas, summed pairs, overlap, a
  missing sample, backwards counters, `/proc/<pid>/stat` parsing, FILETIME units, a watcher on a
  file written in pieces.
- `tests/region_tests.rs`: `--debug-mode` export and flag validation.
- `tests/sdk_tests.rs`: the Python client against `--region-scope` (busy loop of 0.3 s between
  idle phases; Linux and Apple Silicon).
- `tests/json_schema_tests.rs` validates the export against the 1.33 schema.

## Out of scope
`--metric region:NAME:cpu|energy|instructions`; counters on macOS and Windows; counters of all
threads; descendants of the program; streaming without the SDK change.
