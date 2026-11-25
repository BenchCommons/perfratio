# Spec: PMU permissions preflight (#217)

## Problem / Motivation
Hardware counters (`perf_event_open`, planned for `--counters` #49 and `--simd`
#211) fail on many Linux systems: `kernel.perf_event_paranoid` is too high
(Debian/Ubuntu patches use 3 or 4), a container seccomp profile blocks the
syscall, or a VM has no PMU. Silent zeros or a bare `EACCES` leave the user
searching for the cause (see andrewrk/poop#17).

## Contract
New module `src/pmu.rs` (pure logic, unit tested; only `probe()` is Linux-only):
- `parse_paranoid(&str) -> Option<i32>` for `/proc/sys/kernel/perf_event_paranoid`.
- `probe() -> Probe`: one `perf_event_open` of a user-space instructions counter
  (`exclude_kernel`, `exclude_hv`, disabled), closed at once.
- `decide(paranoid, probe) -> PmuStatus`: `EACCES`/`EPERM` → `Denied`;
  `ENOENT`/`ENODEV`/`EOPNOTSUPP`/`ENOSYS` → `Unsupported` (no PMU, typical in VMs);
  other errno → `Other`; no probe → judged by the sysctl alone.
- `diagnostic(&PmuStatus, program) -> Option<String>`: multi-line guidance with
  copy-pasteable commands, for features that need counters to call before runs:
  - paranoid > 2: `sudo sysctl kernel.perf_event_paranoid=2`, a
    `/etc/sysctl.d/99-perf.conf` line, and `setcap cap_perfmon+ep`.
  - paranoid <= 2 but denied: container seccomp / `CAP_PERFMON` guidance.
  - unsupported: enable vPMU passthrough in the hypervisor.

The threshold is 2, not 1: counting only user-space events of the own process
tree (`exclude_kernel`) is allowed up to 2. Counting kernel events needs <= 1.

### `--check-system`
Linux gains a `HW counters` line, for example
`i HW counters  permission denied (perf_event_paranoid=3)  → sudo sysctl ...`.
Counters are optional, so a problem has the new status `Info` (`i`), which does
not fail `--check-system=strict`. Without the real probe (fake root, other
platforms) only the sysctl is judged; the line is omitted when neither is known.

### Fallback (for the future `--counters` / `--simd`)
Callers pass `diagnostic()` text as an error when the counters were required, or
print it once as a warning and continue with time/CPU/memory when optional.
This change adds no new flag and no behavior outside `--check-system`.

## JSON
No change to the JSON export, so no contract version bump.

## Tests
Unit: sysctl parsing, errno classification, diagnostics per cause, summaries,
the `--check-system` line and the full fake-root report.
Not run on real hardware: `probe()` (cfg(linux), developed on macOS); it needs
a Linux run with and without `perf_event_paranoid` > 2 and inside a container.
