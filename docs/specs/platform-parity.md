# Spec: Platform parity matrix (#291)

## Status
Documentation plus three small fixes (see "Gaps fixed here"). No change to the JSON export,
so `SCHEMA_VERSION` stays at 1.36.

## Problem / Motivation
References: #291, #240, #295, #293, #298.

The goal of #291 is "yes" on Linux, macOS and Windows for every feature. The table in the
issue is out of date: the energy, `--resource-usage`, `--affinity`, `--priority realtime` and
`--check-system` rows all changed after it was written. This spec records what the code on
`master` does today, per feature and per OS, including how it degrades (terminal warning,
reason in the JSON), and lists what is still missing.

## How this was derived
- Code: `cfg(target_os)` modules and `src/cli.rs` help on `master` (ef49a5e).
- **macOS column: verified by running the binary** on an Apple M4, macOS 27.2.1, as a
  non-root user. Commands and results are in the matrix.
- **Linux and Windows columns: read from the code only.** Not run for this spec. The Windows
  CI jobs compile and run unit tests, not the OS APIs (see #293, #295).

## Contract

Legend: **yes** works as on Linux; **partial** works with documented differences; **no**
rejected or ignored. "Warning" is a line on stderr; "reason" is
`measurements.<group>_unavailable_reason` in the JSON export.

### Measurements

| Feature | Linux | macOS (verified) | Windows (from code) |
|---|---|---|---|
| Wall time, user/system CPU time | yes | yes | yes |
| Peak memory | yes | yes | partial: main process only (`GetProcessMemoryInfo`), not children of an intermediate shell |
| `--resource-usage` | yes | yes (ctx switches, faults, block I/O) | partial: page faults and I/O operation counts; context switches and major faults omitted from JSON (schema 1.3), shown as `n/a` |
| `--energy` | yes: `sysfs_powercap`, else `perf_pmu`; bare metal only | partial: `ioreport`, Apple Silicon only. On the test machine: `energy: unavailable`, reason `ioreport_frozen` (CPU channels read 0 on macOS 27); warning and `Energy:` line name the cause | partial: `windows_emi`; needs an EMI device. Reasons `emi_not_found`, `emi_unreadable`. Not run on hardware |
| `--energy-backend` of another OS | reason `unsupported_platform` | reason `unsupported_platform` (verified with `sysfs`) | reason `unsupported_platform` |
| `--counters`, `--simd` | yes (PMU) | no: warning, JSON `counters: unavailable`, reason `unsupported_platform` (verified for `--counters`; `--simd` warns the same way) | no: same as macOS |
| `--topdown` | yes, x86_64 only | no: **error**, `--topdown is only supported on Linux x86_64`; no JSON reason exists (verified) | no: same error |
| `--thermal` (temperature sampling) | yes (sysfs) | no: warning (added here); `interference.thermal` absent | no: same |
| `--cooldown until-temp=C` | yes | no: warning "no temperature sensor can be read on this host; not waiting" (verified) | no: same |
| cgroup quota/throttle counters | yes (automatic, no flag) | no: nothing is read, no warning | no: same |

### Control of the benchmarked process

| Feature | Linux | macOS (verified) | Windows (from code) |
|---|---|---|---|
| `--affinity 0-3` (CPU list) | yes (`sched_setaffinity`) | no: error naming `p-cores`/`e-cores` (verified); macOS has no public API to pin to CPUs | yes (mask, CPUs 0-63) |
| `--affinity p-cores` / `e-cores` | no: error "only supported on macOS" | partial: scheduling preference via `PRIO_DARWIN_BG` and the foreground task role; recorded as `affinity` in the export (verified). Apple Silicon only | no: same error as Linux |
| `--jobs N` with `--pin-workers`, `--reserve-cpus` | yes | no: warning "workers cannot be pinned ... ignored", run continues (verified) | yes |
| `--priority high` / `idle` | nice -20 / `SCHED_IDLE` | nice -20 / nice 19; non-root `high` fails with "raising the priority requires root" (verified) | `HIGH_` / `IDLE_PRIORITY_CLASS` |
| `--priority realtime` | `SCHED_FIFO` at max priority | partial: nice -20 plus foreground task role, not a real-time policy; needs root (error as non-root, verified) | `REALTIME_PRIORITY_CLASS`; without administrator rights Windows lowers it to high **silently** (gap, #414) |
| `--check-system` | governor, turbo, load, power source, temperature | load, power source, Low Power Mode, thermal speed limit (`pmset`) (verified) | power source, power plan, CPU load, CPU frequency limit; no turbo check |

### Exit and reporting behaviour common to all
- A requested measurement that cannot work never changes the exit code, except where the
  table says "error" (`--topdown`, CPU-list `--affinity` on macOS, `--priority` without
  privileges). Where it degrades, `measurements.<group>` is `unavailable` and the reason is an
  open enum (new values are a MINOR change, see `output-schema-contract.md`).
- Energy reasons: `unsupported_platform`, `rapl_not_found`, `rapl_unreadable`,
  `unsupported_cpu`, `ioreport_not_found`, `ioreport_frozen`, `emi_not_found`,
  `emi_unreadable` (`energy::Unavailable`). Counter reasons: see `src/perf_counters.rs`.

### Rows added by open pull requests (pending merge)
These PRs are open and not part of `master`. Cells follow each PR description and have not
been checked here. Each adds a row to the matrix once merged.

| Feature (PR) | Linux | macOS | Windows |
|---|---|---|---|
| Raw PMU events, `--list-events` (#394) | yes (sysfs `event_source`, `perf_event_open`) | no, same as `--counters` | no, same as `--counters` |
| `--gpu` NVML / AMD SMI (#401) | yes, via `dlopen`; not run on GPU hardware | not stated in the PR | no: `nvml.dll` loader not implemented, reason `unsupported_platform` |
| `--energy-attribution cpu-time\|cgroup` (#402) | yes (`/proc/stat`, cgroup `cpu.stat`) | not stated; needs `/proc/stat` | not stated; needs `/proc/stat` |
| `--power-meter COMMAND` (#402) | yes | yes (any OS per PR) | yes (any OS per PR) |
| `--collector-plugin` (#403) | yes | yes (JSON-lines over pipes; not stated per OS) | not stated |
| `--collector cachegrind\|callgrind` (#403) | yes, needs Valgrind | no: Valgrind unsupported on Apple Silicon | no |
| `--membw` memory bandwidth and NUMA (#404) | yes (uncore IMC, `numa_maps`) | no: warning, reason `unsupported_platform` | no: same |

Merge order matters: open PRs that add JSON fields each pick the next `schemaVersion`, so
they renumber it in the order they merge. Update this table when they land.

## Gaps
Fixed in this change (small, no JSON change):
1. `--energy` on macOS and Windows printed `Energy: RAPL unprivileged/unavailable on host`
   and no warning, which is wrong off Linux. Now a once-per-process warning and the `Energy:`
   line give the reason from `energy::Unavailable::message()`. Linux RAPL wording is unchanged.
2. `--thermal` was silently ignored off Linux. It now warns once.
3. `--help` text was stale: `--priority` long help said `realtime` is "not available on
   macOS" (it is, since #298); `--check-system` listed no Windows checks; `--thermal` did not
   say it is Linux only.

Open, tracked as issues (large, or need hardware or a design decision):
- #411 Hardware counters on macOS and Windows; make `--topdown` degrade like `--counters`.
- #412 Temperature sampling and `--cooldown until-temp` on macOS and Windows.
- #413 Validate IOReport and EMI energy on real hardware; macOS 27 via CLPC.
- #414 Detect Windows lowering `--priority realtime` to high.

Accepted limits, no issue:
- macOS cannot pin processes to CPUs with public APIs; `p-cores`/`e-cores` is the closest
  equivalent (see `affinity.md`).
- macOS `--priority realtime` is not a real-time policy: thread QoS and time-constraint
  policies are reset by exec (see `priority.md`).
- Windows has no context-switch or major-fault counters in job accounting
  (see `windows-resource-usage.md`).
- cgroup counters have no equivalent on macOS and Windows; no flag exists to warn about.

## Tests
- `energy::baseline_tests::every_reason_has_a_message_and_only_rapl_ones_mention_rapl`
- `options::thermal_warning_tests::warns_only_when_requested_off_linux`
- macOS behaviour in the matrix was run manually with the debug binary; it is not in CI.

Refs: #291.
