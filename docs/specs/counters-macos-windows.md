# Spec: `--counters` on macOS and Windows (#411)

**Status:** macOS implemented (Apple Silicon, `-N` only). Windows implements `cycles` only, with `-N` (not run on Windows hardware yet; CI covers it).

## Problem / Motivation
`--counters` (#49) was Linux only. On macOS the `kperf`/`kpc` API needs root and counts per
thread, so the counters were `unavailable`. A task-level source exists that needs no root:
`proc_pid_rusage(RUSAGE_INFO_V4)` returns `ri_instructions` and `ri_cycles` (measured on an
Apple M4, uid 501, no root).

## Contract
- macOS, with `-N` / `--shell=none`: `instructions` and `cycles` are measured per run.
  `measurements.counters = "measured"` (or `partial` when other events were requested, which
  are listed in `counters_unavailable_events`). Other events and `:u`/`:k` scopes have no source.
- The child is read while still a zombie (`waitid(WNOWAIT)`), right before `wait4`. Only the
  spawned process is counted, not its children. `waitid` and `proc_pid_rusage` run before
  `stop_timer`, so their cost (microseconds) is inside the measured time and is not subtracted.
- macOS with a shell: counters would measure the shell, so they are dropped:
  `counters = "unavailable"`, `counters_unavailable_reason = "other"`, plus a warning that
  suggests `-N`. That warning replaces the other counter warnings (skipped events, task-level
  note), which would describe counters that are not read.
- Intel Macs: the fields stay 0, the probe fails and the counters are `unavailable`.
- `--simd` stays Linux only: warning on macOS, `simd = unavailable / unsupported_platform`.
- `--topdown` outside Linux x86_64 is no longer an error: warning, `topdown = "unavailable"`,
  `topdown_unavailable_reason = "unsupported_platform"`.
- Windows, with `-N` / `--shell=none`: `cycles` is measured per run with `QueryProcessCycleTime`
  (`kernel32`, `windows-sys` feature `Win32_System_WindowsProgramming`), which needs no admin.
  ETW PMC and `SetThreadProfiling` need admin or a driver and are not used.
  - The value is the cycle counter of all threads of the spawned process (user and kernel). On
    current Windows it is derived from the TSC, so it is reference cycles, not core clocks, and
    it does not include interrupt time. It is not a PMU count.
  - It is read in `CPUTimer::stop`, after `Child::wait` and before the `Child` is dropped: the
    handle `std::process` holds stays valid after the process exits. Like on macOS, the read
    happens before the timer result is returned, and its cost is not subtracted.
  - Only the spawned process is counted. perfratio runs the command in a Job Object, but the
    job API has no cycle query: `JOBOBJECT_BASIC_ACCOUNTING_INFORMATION` and
    `..._AND_IO_ACCOUNTING_INFORMATION` carry times, page faults and I/O only, and
    `QueryProcessCycleTime` has no job variant. Summing the processes of the job is not possible
    either, since the ones that already exited cannot be queried.
  - `instructions`, cache, branch, software events and `:u`/`:k` scopes have no source. They are
    skipped with the existing reason, as on macOS: `counters = "partial"` with
    `counters_unavailable_events` (and `counters_unavailable_reason = "no_pmu"`), or
    `unavailable` when nothing requested can be read. In the second case the warning says that
    Windows can only read cycles.
  - With a shell (the default, `cmd.exe`) the counters would measure the shell: dropped as on
    macOS (`unavailable`, reason `other`, warning that suggests `-N`).
  - `--simd` and `--topdown`: as on macOS (`unsupported_platform`, warning, benchmark continues).
  - The hidden `task-clock` (divisor of the effective frequency) has no source, so the effective
    frequency is not computed on Windows.
- JSON contract on Windows: no new fields, values or reasons. `counters`, `counters_scope`,
  `counters_unavailable_events` and the existing reasons are emitted by one more platform.
  `SCHEMA_VERSION` stays 1.48 (no schema change; `scripts/check-schema-version.sh` passes).
  The value of `cycles` on Windows is TSC based: compare it only with other Windows exports.
- macOS: JSON contract unchanged; only existing fields and enum values are used.

## Verification
- Unit tests in `src/perf_counters.rs` (`macos` module); `tests/counters_tests.rs` (`-N` gives
  values > 0, shell gives unavailable plus warning); `tests/topdown_tests.rs`.
- Windows: unit tests in `src/perf_counters.rs` (`windows` module) and `tests/counters_tests.rs`
  (`windows_*`: `-N` gives cycles > 0 and `instructions` stays absent, shell gives unavailable
  plus warning, `instructions` alone gives unavailable). They run in the Windows CI jobs.
- Run on Apple M4 (macOS). Not verified: Intel Mac, older macOS versions, Windows ARM64,
  Windows virtual machines (the cycle counter may not be virtualized).

Refs #411 (instructions, SIMD and top-down on Windows stay unsupported)
