# Spec: `--counters` on macOS and Windows (#411)

**Status:** macOS implemented (Apple Silicon, `-N` only). Windows not done.

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
- Windows: unchanged, `unavailable` / `unsupported_platform`. **Not implemented.**
- JSON contract unchanged: `SCHEMA_VERSION` stays 1.36; only existing fields and enum values
  are used.

## Verification
- Unit tests in `src/perf_counters.rs` (`macos` module); `tests/counters_tests.rs` (`-N` gives
  values > 0, shell gives unavailable plus warning); `tests/topdown_tests.rs`.
- Run only on Apple M4. Not verified: Intel Mac, older macOS versions, Windows.

Refs #411
