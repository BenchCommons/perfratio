# Spec: Platform parity matrix (#291)

## Status
Documentation plus three small fixes (see "Gaps fixed here"). No change to the JSON export.

## Problem / Motivation
References: #291, #240, #295, #293, #298.

The goal of #291 is "yes" on Linux, macOS and Windows for every feature. The table in the
issue is out of date: the energy, `--resource-usage`, `--affinity`, `--priority realtime` and
`--check-system` rows all changed after it was written. This spec records what the code on
`master` does today, per feature and per OS, including how it degrades (terminal warning,
reason in the JSON), and lists what is still missing.

## How this was derived
- Code: `cfg(target_os)` modules and `src/cli.rs` help on `master`.
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
| `--resource-usage` | yes | yes (ctx switches, faults, block I/O) | partial: page faults and I/O operation counts; context switches and major faults omitted from JSON, shown as `n/a` |
| `--energy` | yes: `sysfs_powercap`, else `perf_pmu`; bare metal only | partial: `ioreport`, Apple Silicon only. On the test machine: `energy: unavailable`, reason `ioreport_frozen` (CPU channels read 0 on macOS 27); warning and `Energy:` line name the cause | partial: `windows_emi`; needs an EMI device. Reasons `emi_not_found`, `emi_unreadable`. Not run on hardware |
| `--energy-backend` of another OS | reason `unsupported_platform` | reason `unsupported_platform` (verified with `sysfs`) | reason `unsupported_platform` |
| `--counters` | yes (PMU) | partial (#420): `instructions` and `cycles` with `-N` on Apple Silicon (`proc_pid_rusage`); with a shell they are dropped with a warning that suggests `-N`; other events: no | no: warning, `counters: unavailable`, reason `unsupported_platform` |
| `--simd` | yes (PMU) | no: warning, `simd: unavailable`, reason `unsupported_platform` | no: same as macOS |
| `--topdown` | yes, x86_64 only | no: warning, `topdown: unavailable`, reason `unsupported_platform` (`counters-macos-windows.md`) | no: same as macOS |
| Raw PMU events, `--list-events` (#394) | yes (sysfs `event_source`, `perf_event_open`) | no: `--list-events` prints the built-in names and "No PMUs found ... (only available on Linux)"; raw events warn as `--counters` does elsewhere | no: same as macOS |
| `--gpu` NVML / AMD SMI (#401) | yes, via `dlopen`; not run on GPU hardware | no: warning, reason `gpu_library_not_found` (neither NVML nor AMD SMI exists on macOS; verified) | no: `nvml.dll` loader not implemented, reason `unsupported_platform` |
| `--energy-attribution cpu-time\|cgroup` (#402) | yes (`/proc/stat`, cgroup `cpu.stat`) | no: warning once that `/proc/stat` is needed, no estimate; measured energy unaffected (verified) | no: same as macOS (from code) |
| `--power-meter COMMAND` (#402; requires `--energy`) | yes (`sh -c`) | yes (`sh -c`; verified with `--power-meter 'echo 5'`) | yes (`cmd /C`; from code) |
| `--thermal` (temperature sampling) | yes (sysfs, `interference.thermal` in °C) | partial (#419): no temperature without root; the thermal pressure level (`nominal` to `sleeping`) goes to `interference.thermal_pressure`, `interference.thermal` is absent (verified) | no: warning, nothing sampled |
| `--cooldown until-temp=C` | yes | no: warning "no temperature sensor can be read on this host ...; not waiting", with the macOS cause (verified) | no: same warning |
| `--collector-plugin PATH` (#403) | yes | yes: JSON lines over pipes, no OS-specific code (verified with a shell-script plugin) | yes (from code); `PATH` must be something `CreateProcess` runs directly (`.exe`, `.bat`, `.cmd`), not a `.sh` script |
| `--collector cachegrind\|callgrind` (#403) | yes, needs Valgrind | no: error, non-zero exit, because Valgrind cannot be run (not installed here; Valgrind does not support Apple Silicon, see `valgrind-collector.md`) | no: same error (from code) |
| `--membw` memory bandwidth and NUMA (#404) | yes (uncore IMC via `perf_event_open`, `numa_maps`); not run on Linux hardware here | no: two warnings, `membw` and `numa` `unavailable` with reason `unsupported_platform` (verified) | no: same (from code) |
| cgroup quota/throttle counters | yes (automatic, no flag) | no: nothing is read, no warning | no: same |

### Control of the benchmarked process

| Feature | Linux | macOS (verified) | Windows (from code) |
|---|---|---|---|
| `--affinity 0-3` (CPU list) | yes (`sched_setaffinity`) | no: error naming `p-cores`/`e-cores` (verified); macOS has no public API to pin to CPUs | yes (mask, CPUs 0-63) |
| `--affinity p-cores` / `e-cores` | no: error "only supported on macOS" | partial: scheduling preference via `PRIO_DARWIN_BG` and the foreground task role; recorded as `affinity` in the export (verified). Apple Silicon only | no: same error as Linux |
| `--jobs N` with `--pin-workers`, `--reserve-cpus` | yes | no: warning "workers cannot be pinned ... ignored", run continues (verified) | yes |
| `--priority high` / `idle` | nice -20 / `SCHED_IDLE` | nice -20 / nice 19; non-root `high` fails with "raising the priority requires root" (verified) | `HIGH_` / `IDLE_PRIORITY_CLASS` |
| `--priority realtime` | `SCHED_FIFO` at max priority | partial: nice -20 plus foreground task role, not a real-time policy; needs root (error as non-root, verified) | `REALTIME_PRIORITY_CLASS`; without administrator rights Windows lowers it to high, and perfratio now warns after reading the class back (#418; from code, needs Windows to run) |
| `--check-system` | governor, turbo, load, power source, temperature | load, power source, Low Power Mode, thermal speed limit (`pmset`), thermal pressure level (#419) (verified) | power source, power plan, CPU load, CPU frequency limit; no turbo check |

### Exit and reporting behaviour common to all
- A requested measurement that cannot work never changes the exit code, except where the
  table says "error" (CPU-list `--affinity` on macOS, `--priority` without
  privileges, `--collector cachegrind|callgrind` without a working Valgrind). Where it degrades, `measurements.<group>` is `unavailable` and the reason is an
  open enum (new values are a MINOR change, see `output-schema-contract.md`).
- Energy reasons: `unsupported_platform`, `rapl_not_found`, `rapl_unreadable`,
  `unsupported_cpu`, `ioreport_not_found`, `ioreport_frozen`, `emi_not_found`,
  `emi_unreadable` (`energy::Unavailable`). Counter reasons: see `src/perf_counters.rs`.

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
- #411 Hardware counters on macOS and Windows. #420 delivered `instructions` and `cycles` on
  macOS with `-N`, and `--topdown` now degrades like `--counters`. Missing: Windows, other
  events on macOS, Intel Macs (`counters-macos-windows.md`).
- #412 Temperature sampling and `--cooldown until-temp` on macOS and Windows. #419 added the
  thermal pressure level on macOS (`--thermal`, `--check-system`). Missing: a temperature in
  °C on macOS (needs root) and any reading on Windows (`thermal-macos-windows.md`).
- #413 Validate IOReport and EMI energy on real hardware; macOS 27 via CLPC.
- #414 Detect Windows lowering `--priority realtime` to high. #418 added the warning. Missing:
  a run on Windows, since the check is not exercised in CI.

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
- `options::thermal_warning_tests::warns_only_when_requested_where_unsupported`
- `thermal::tests::macos_reports_a_pressure_level` (macOS only)
- macOS behaviour in the matrix was run manually with the debug binary; it is not in CI.

Refs: #291.
