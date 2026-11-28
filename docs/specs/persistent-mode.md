# Spec: Persistent process mode (`--persistent`) (#370)

**Status:** Implemented (persistent protocol 1, schemaVersion 1.46). SDK support: Rust,
Python, JavaScript/TypeScript, Go, Java and Kotlin (`serve`, #372).

## Problem / Motivation
References: #370, #310 (`region-protocol.md`), #311, Barrett et al., "Virtual Machine Warmup
Blows Hot and Cold", 2017.

Every sample starts a new process, so a JIT runtime never leaves its warm-up and a
service-style workload (many requests to one process) cannot be measured. With `--persistent`
perfratio starts the command **once** and drives every run as one iteration of that process.
The program times each iteration itself, so the time excludes the pipe round trip.

## Contract

### CLI
```
--persistent     Start every command once; every warm-up and benchmark run is an iteration.
```
- The command runs **without a shell** (like `-N`); `--shell` conflicts. Setup, prepare,
  conclude and cleanup commands run as usual, before and after each iteration.
- Conflicts with `--concurrency`, `--jobs`, `--region`, `--region-scope`, `--until`,
  `--output-metric`, `--subtract`, `--energy`, `--counters`, `--topdown`, `--simd`,
  `--validate` and `--expect-output-sha256`: they need a process per run.
- `--runs`, `--warmup`, `--timeout`, `-i` keep their meaning. `--warmup N` sends N iterations
  that are not recorded. To **see** the warm-up use `-w 0` and many runs.
- One process per command (parameter values give separate commands). The process of a
  benchmark is stopped when the next one starts and at the end.
- User and system CPU time and memory are not collected (the process outlives the run), so
  they are exported as 0. The runner floor calibration is skipped.

### Wire format, persistent protocol 1
perfratio sets `PERFRATIO_PERSISTENT_PROTOCOL=1` and `PERFRATIO_REGION_PROTOCOL=1` and removes
`PERFRATIO_REGION_FILE`. Requests go to the program's **stdin**, one per line:

| Request | Meaning |
|---|---|
| `run <n>` | Run iteration `n` (0, 1, 2, ... including warm-up) and answer. |
| `quit` | Exit. Closing stdin has the same meaning. After `quit`, perfratio waits up to 2 s; whether the program exits by itself or not, its process group (grandchildren) is then killed (Unix). |

The program answers on **stdout**. Only lines that start with `@perfratio ` belong to the
protocol; every other line is the program's own output and is ignored. After the prefix the
text is the wire format of the region protocol (`hello`, `start`, `stop`, monotonic `t_ns`):

```
@perfratio hello 1 [<sdk-id>]            once, as soon as the program is ready
@perfratio start iteration <t_ns>        per iteration, around the measured work
@perfratio stop iteration <t_ns>
@perfratio done <n>                      ends the answer to `run <n>`
@perfratio fail <n> <message>            ends the answer; the iteration failed (no times needed)
```
The time of an iteration is the sum of the intervals of the region `iteration`, read by the
same parser as `--region` (so overlap, a stop without start and a backwards clock are the same
errors). Other region names in an answer are validated and ignored.

### Errors
| Situation | Behaviour |
|---|---|
| No `hello` within 60 s, or the process exits first | Error "did not say 'hello'". |
| `hello <n>` with `n` different from 1 | Error "region protocol mismatch ...". |
| `done` without an `iteration` interval, or a different `n` | Error naming the iteration. |
| `fail <n> <message>` | Error "failed iteration: <message>", unless `-i` ignores failures. With `-i` the iteration keeps a measured time in `times` and in all statistics, like a failing normal command: the `iteration` interval if the program sent one before `fail`, else the wall time of the request/answer round trip. Never `0.0` (before, `0.0` was recorded and lowered mean, min and stddev). `--omit-failed-runs` drops it as usual. |
| The process exits mid-benchmark | Error "the persistent process exited ... before it answered". |
| No answer within `--timeout` | The process is killed; error "did not answer in time". |

### Warm-up detection and steady state
Applied to the measured iterations (`times`), needs at least 10 of them:
1. The steady-state level is the median of the second half.
2. The band is `max(10% of the level, 3 sigma)`, where sigma is the noise of the second half
   estimated from its consecutive differences (scaled MAD divided by √2), so that a drift or a
   step does not widen it.
3. A second half whose two halves have medians further apart than
   `max(10% of the level, 5.3 sigma / √(n/4))` is still drifting: `no_steady_state`.
4. The rolling median (window 5) is compared with the level; the warm-up ends after the last
   window outside the band (`first = j + 2 + 1` for the last bad window starting at `j`). A
   single outlier in the steady phase does not extend the warm-up.
5. A warm-up that reaches the second half is `no_steady_state`.

This is a simple robust rule, not the changepoint analysis of Barrett et al.; the full
series stays in `times` so a better analysis can be applied afterwards.

### JSON (schemaVersion 1.46, additive)
```jsonc
"results": [{
  "mode": "persistent",
  "times": [0.040, 0.040, 0.010, ...],   // per-iteration time as reported by the program
  "iterations": {
    "protocol": 1, "sdk": "python/1.1.0", "count": 40,
    "status": "steady_state",             // or "no_steady_state" | "too_few_iterations"
    "warmup_iterations": 5,               // only with steady_state
    "steady_state": { "first_iteration": 5, "count": 35, "mean": 0.0101, "stddev": 0.0003,
                      "min": 0.0098, "max": 0.0112 }
  }
}]
```
`mode` and `iterations` are absent without `--persistent`. The terminal shows a
`Persistent:` line with the warm-up count and steady-state mean. The first-run and off-CPU
warnings are not issued in this mode.

## Implementation
- `src/persistent.rs`: protocol, `parse_response`, `analyze`, `PersistentExecutor` (wraps the
  raw executor, keeps one process).
- `src/benchmark/executor.rs`: `MockExecutor::with_persistent` simulates a warm-up (the first
  measured iterations take 4, 2 and 1.5 times as long).
- `src/benchmark/mod.rs`, `benchmark_result.rs`: `mode`, `iterations`, terminal line.
- SDKs: `sdk/python/perfratio_region.py` and `sdk/rust/src/lib.rs` (`serve`), with
  `persistent_example.py` and `examples/persistent.rs`.

## Verification
- `src/persistent.rs` unit tests: analysis (flat, step, outlier, drift, late), response parsing.
- `tests/persistent_tests.rs`: `--debug-mode` (all platforms) and a shell script playing a
  persistent process with fabricated time stamps (Unix): one start, exact times, warm-up
  count, version mismatch, uninstrumented command, missing interval, failure and `-i`, death,
  timeout.
- `tests/persistent_tests.rs` also covers a failed iteration under `-i` (time > 0) and the kill of
  grandchildren when the program exits by itself within the 2 s quit grace (Unix).
- `tests/sdk_tests.rs`: the Python and Rust examples under real `perfratio --persistent`.
- `tests/json_schema_tests.rs`: exports validated against the schema.

## Out of scope
Several processes per benchmark (a pool of workers), request payloads, CPU and memory per
iteration, changepoint detection of several phases, `--persistent` with `--jobs`.
