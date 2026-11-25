# Spec: `--topdown` Top-down Microarchitecture Analysis, level 1 (#221)

**Status:** Proposed. Scheduled for 1.x, not 1.0.

## Problem / Motivation
References: #221, #49, #217, #218, #219, `perf-stat(1)` (`--topdown`), Ahmad Yasin, "A Top-Down
Method for Performance Analysis and Counters Architecture" (ISPASS 2014).
Shared background and dependencies: `pmu-extensions-common.md`.

Cycles and instructions say that one variant is faster, not why. TMA level 1 splits all issue
slots into four classes (retiring, bad speculation, frontend bound, backend bound) and so names
the bottleneck class. Level 1 only; deeper levels are a follow-up.

## Contract

### CLI
```
--topdown     Collect Top-down Microarchitecture Analysis level 1 (Linux, x86_64 only).
```
Honors `--scope` (not `split`). Not an implicit part of `--counters`. Non-Linux, or a non-x86_64
host: error `--topdown is only supported on Linux x86_64`. Unsupported CPU/kernel/VM: warn once,
continue, `measurements.topdown = "unavailable"`.

### Support matrix (corrections to the issue text)
| CPU | Method | Status |
|---|---|---|
| Intel Ice Lake / Sunny Cove and later (Tiger Lake, Alder Lake P-core, Sapphire Rapids, ...) | `perf_metrics` register: fixed counter 3 `slots` plus metric events | Supported. Needs a kernel that exposes `/sys/devices/cpu/events/slots` and `topdown-retiring` (`cpu_core` on hybrid). |
| Intel Skylake to Cascade Lake | formulas over 4-7 generic events (`UOPS_ISSUED.ANY`, `IDQ_UOPS_NOT_DELIVERED.CORE`, `UOPS_RETIRED.RETIRE_SLOTS`, `INT_MISC.RECOVERY_CYCLES[_ANY]`) | Not supported in v1. With SMT on, perf's own formulas need the `_ANY` events and a core-wide view, which an unprivileged per-task counter cannot give. |
| Intel Atom (Gracemont `cpu_atom`) | `TOPDOWN_{FE,BE}_BOUND.ALL`, `TOPDOWN_RETIRING.ALL` (5 slots per cycle) | Not supported in v1 (pinned E-core runs report `unsupported_cpu`). |
| AMD Zen 4, Zen 5 | event-based "PipelineL1" metrics, no hardware metric register | Supported (below). |
| AMD Zen 1 to Zen 3 | no equivalent events | `unsupported_cpu`. |
| AArch64 | `STALL_SLOT_*`, only on newer cores, needs `slots` cap | Out of scope. |

The issue says "Intel (Ice Lake+, Golden Cove+) and AMD (Zen 4+)", which matches this, but
"`TOPDOWN.BACKEND_BOUND_SLOTS`" etc. are the model-specific event names; the Linux interface
used here is the pseudo events below.

### Intel: `perf_metrics`
Source: kernel `tools/perf/Documentation/topdown.txt`, `arch/x86/events/intel/core.c`
(`EVENT_ATTR_STR(topdown-retiring ... "event=0x00,umask=0x80")`).
- One group, leader first: `slots` (`PERF_TYPE_RAW`, `config = 0x0400`) then metric events
  `topdown-retiring` (`0x8000`), `topdown-bad-spec` (`0x8100`), `topdown-fe-bound` (`0x8200`),
  `topdown-be-bound` (`0x8300`). The `slots` event must be the group leader. Use the PMU `type` from
  `/sys/devices/cpu/type` (or `cpu_core/type` on hybrid) rather than `PERF_TYPE_RAW`, and take
  the encodings from `/sys/devices/cpu/events/topdown-*` at run time, so a kernel that changes
  them keeps working.
- These use fixed counter 3 and the metrics MSR, **no programmable counters**, so `--topdown`
  coexists with `--counters` and `--simd` without multiplexing.
- Fractions: `x = metric_x / (retiring + bad_spec + fe_bound + be_bound)` using the four
  metric values (this is what perf's `tma_*` expressions do, see
  `.../skylake`/`alderlake/*-metrics.json`).
  Newer cores in perf's JSON subtract `INT_MISC.UOP_DROPPING / slots` from frontend bound
  and set bad speculation to `max(1 - (fe + be + ret), 0)`. To match `perf stat --topdown` the
  implementation must mirror the model's JSON, which adds one programmable event
  (`INT_MISC.UOP_DROPPING`, in the group). The allow-list therefore stores per model the extra
  correction, or none.
- Group read: each member is read with its own `read(2)`; no `PERF_FORMAT_GROUP`.
  The kernel resets metric registers on read, so perfratio reads once at the end of the run and never
  mixes this with `rdpmc`.
- Accuracy: perf warns "Topdown accuracy may decrease when measuring long periods"
  (`builtin-stat.c`). Runs are one command lifetime; for commands over several seconds the
  numbers are an approximation and the terminal does not hide this (note under the block,
  once, for runs longer than 5 s).

### AMD Zen 4 / Zen 5: pipeline utilisation
Source: perf JSON `tools/perf/pmu-events/arch/x86/amdzen4/pipeline.json` and `amdzen5/pipeline.json`
("PipelineL1"):

```
total_dispatch_slots = W * ls_not_halted_cyc            # W = 6 (Zen 4), 8 (Zen 5)
frontend_bound   = de_no_dispatch_per_slot.no_ops_from_frontend / total_dispatch_slots
backend_bound    = de_no_dispatch_per_slot.backend_stalls       / total_dispatch_slots
smt_contention   = de_no_dispatch_per_slot.smt_contention       / total_dispatch_slots
bad_speculation  = (de_src_op_disp.all - ex_ret_ops)            / total_dispatch_slots
retiring         = ex_ret_ops                                   / total_dispatch_slots
```
- Six events (`ls_not_halted_cyc`, three umasks of `de_no_dispatch_per_slot`, `de_src_op_disp.all`,
  `ex_ret_ops`). Zen 4 has 6 core counters, so `--topdown` takes **all of them** and
  any `--counters` event is multiplexed against it. The preflight warns before the run (#219).
  The six events are opened as one group so ratios come from the same time window.
- Event codes are taken from the allow-listed perf JSON for the model, not typed from memory.
- The fifth class `smt_contention` exists only on AMD. It is exported as an extra key and shown
  in the terminal when above 0.1%. The four classes sum to 1 minus `smt_contention`.

### Selection
CPU identification and allow-list as in the common doc; kernel capability checked via sysfs for
Intel. Reasons for `unavailable`: `unsupported_platform`, `unsupported_cpu`, `unsupported_kernel`
(no `slots` in sysfs), `perf_paranoid`, `no_pmu` (VM without `slots`), `hybrid_cpu_unpinned`.

### Measurement semantics
- Per run, `inherit`, scope as `--scope`; counting starts before `spawn()` like #49. Overhead
  calibration: the metric fractions are ratios, so calibration is applied to the AMD raw
  events (subtract `true`'s median from each) and not to Intel metrics (not meaningful for
  fractions; documented).
- Short runs: with fewer than about 1M slots (a few hundred microseconds) the fractions are
  dominated by startup. The terminal marks the block `[short run, low confidence]` below that.
- Multi-threaded commands: slots from all threads; fractions are slot-weighted.
- Warmup runs are not counted.

### Terminal output
```
  Topdown (level 1, Intel perf_metrics):
    Retiring:           24.5%
    Bad Speculation:    12.1%
    Frontend Bound:     14.2%
    Backend Bound:      49.2%   <- primary bottleneck
```
Means of per-run fractions. The primary bottleneck is the largest of bad speculation, frontend
bound, backend bound (and `smt_contention` on AMD); retiring is shown but never marked, because
a retiring-dominated run has no bottleneck class at this level. The issue's descriptive
suffixes (`[Memory stalls ...]`) are dropped: at level 1 `backend bound` mixes memory and
core stalls and such text would mislead.

With a reference command, the comparison line shows the shift of the primary class:
`Backend Bound 49.2% -> 15.0% (-34.2 pp)`; implemented only after #48 `--metric` / the
comparison table (#212) define the layout, as an optional row.

### JSON (additive: MINOR)
Fractions in `0..1`; per run, aligned with `times`; `null` if a run was not measured:
```jsonc
"topdown": {
  "level": 1,
  "source": "intel_perf_metrics",           // or "amd_pipeline_util"
  "retiring":        [0.245, ...],
  "bad_speculation": [0.121, ...],
  "frontend_bound":  [0.142, ...],
  "backend_bound":   [0.492, ...],
  "smt_contention":  [0.0, ...]             // AMD only; key absent on Intel
},
"topdown_summary": {
  "retiring": { "mean": 0.245, "stddev": 0.004 }, "...": {},
  "bottleneck": "backend_bound"             // open enum
}
```
`perfratio.measurements.topdown` plus `topdown_unavailable_reason`. The issue's single object with
scalar means is replaced by arrays plus a summary, to keep the contract rule that per-run data is
aligned with `times`. `schemaVersion`: MINOR. Import round-trips the keys.

## Edge cases
- VM: `slots` is typically not exposed to guests; reported `no_pmu`/`unsupported_kernel`. The
  allow-list never guesses; the sysfs check is authoritative on Intel.
- Hybrid: Intel `cpu_core` only, and only when pinned; E-core runs are unsupported.
- Kernel too old for `perf_metrics` (Ice Lake support landed in the 5.x series; confirm the
  minimum on the oldest kernel tested): `unsupported_kernel`.
- NMI watchdog consumes a programmable counter; on AMD this can block the six-event group
  (`EBUSY` or group never scheduled). `time_running = 0` ⇒ `unavailable (other)` with the hint
  `echo 0 | sudo tee /proc/sys/kernel/nmi_watchdog` (#219: never scheduled).
- Group + `inherit`: must be verified on the oldest supported kernel; if rejected, `--topdown`
  falls back to the main process only (no children) with a warning, or is `unavailable`;
  decision recorded with the test result.
- A zero-slot run (command failed to exec): `null`.

## Test plan
- Unit: fraction maths for both sources (including `max(..., 0)` and normalization to 1),
  primary-bottleneck choice (ties, retiring dominant), allow-list from fixture cpuinfo,
  sysfs-capability parser, JSON and round-trip, short-run marker.
- Mocked opener: no `slots`, group not scheduled, permission denied.
- Hardware (`PERFRATIO_TEST_PERF=1`, Ice Lake+ and Zen 4):
  - Pointer-chasing over a 1 GiB array: backend bound largest, above 50%.
  - Tight ALU loop: retiring above 50% on Intel.
  - Random-branch loop: bad speculation clearly above that of a predictable loop.
  - Reference values: use the explicit group
    `perf stat -e '{slots,topdown-retiring,topdown-bad-spec,topdown-fe-bound,topdown-be-bound}'`
    on the same command and require each class within 3 percentage points over 10 runs.
- `--topdown --counters` on Ice Lake: no multiplexing flagged. On Zen 4: multiplexing
  flagged and scaled.

## Implementation breakdown
1. CPU allow-list and capability probe (shared with #211).
2. Group opener with leader and member reads in `src/counters/linux.rs`.
3. Intel and AMD fraction calculators (pure functions).
4. `--topdown` flag, validation, `Options`.
5. Terminal block and bottleneck marker.
6. JSON, measurements group, schema table, CHANGELOG, README, man page.
7. Tests above.

## Acceptance criteria
- `--topdown` parses and validates.
- Level 1 metrics collected on supported Intel and AMD CPUs, matching `perf stat` metric
  groups within the test tolerance.
- Primary bottleneck marked; fractions in JSON.
- Graceful fallback with a reason everywhere else.

## References
#221, #49, #217, #218, #219, #240; Linux `tools/perf/Documentation/topdown.txt`;
`arch/x86/events/intel/core.c`; perf JSON `alderlake/adl-metrics.json`,
`skylake/skl-metrics.json`, `amdzen4/pipeline.json`, `amdzen5/pipeline.json`;
`tools/perf/builtin-stat.c` (`--topdown`); Yasin, ISPASS 2014.
