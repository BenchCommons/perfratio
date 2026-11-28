# Spec: `--priority normal|high|idle|realtime` (#80)

## Problem
Scheduler interference (preemption by other processes) is a major noise source.
Running the benchmarked process with a higher priority reduces it. A lower
priority is useful for benchmarking background workloads.

## Contract
```
--priority <POLICY>   normal (default) | high | idle | realtime
```
| Policy | Linux | macOS | Windows |
|---|---|---|---|
| normal | inherited | inherited | inherited |
| high | `setpriority(-20)` | `setpriority(-20)` | `HIGH_PRIORITY_CLASS` |
| idle | `SCHED_IDLE` | `setpriority(19)` | `IDLE_PRIORITY_CLASS` |
| realtime | `SCHED_FIFO`, max priority | `setpriority(-20)` + `TASK_FOREGROUND_APPLICATION` role (#291) | `REALTIME_PRIORITY_CLASS` |

- Applies to every process perfratio starts: the benchmarked command, `--prepare`,
  `--conclude`, `--setup`, `--cleanup`, and the shell spawning time calibration,
  so the subtracted shell overhead is measured under the same priority.
- Unix: set in the child between fork and exec (`pre_exec`, like `--affinity`),
  so the program and all its children inherit it. Windows: `SetPriorityClass`
  on the still-suspended process, before it is resumed.
- `high`/`realtime` without privileges fail with a hint: on Linux, root or
  `sudo setcap cap_sys_nice+ep "$(command -v perfratio)"` (root in a container
  usually lacks `CAP_SYS_NICE`); on macOS, root. Windows silently turns
  `realtime` into `high` without administrator rights.
- `realtime` prints a starvation warning at startup (`--timeout`, #51, does not exist yet).
- The calibration error now keeps its cause, so the hint (or e.g. "No such file
  or directory" for a missing `--shell`) is shown instead of a generic message.

## Windows: detecting a lowered class (#414)
Without administrator rights `SetPriorityClass(REALTIME_PRIORITY_CLASS)` succeeds
but Windows applies `HIGH_PRIORITY_CLASS`. After `SetPriorityClass`, perfratio
reads the class back with `GetPriorityClass` and, if it differs from the request,
prints one warning per process to stderr, e.g.
`Warning: '--priority realtime' requested REALTIME_PRIORITY_CLASS but Windows applied HIGH_PRIORITY_CLASS: ...`.
The run continues (warn, not fail). No JSON change: the effective priority is not
exported, so `SCHEMA_VERSION` is unchanged. If it is exported later, version it.
The comparison is `priority::priority_downgrade_warning` (unit-tested on every
platform); the readback is `#[cfg(windows)]` and **not tested locally** (needs
Windows, CI only compiles it).

## macOS `realtime` (#291)
macOS has no `SCHED_FIFO` for processes. Its real-time mechanism is the Mach
`THREAD_TIME_CONSTRAINT_POLICY`, and the closest non-real-time one is thread QoS
(`pthread_set_qos_class_self_np`). Both are per thread and exec resets them:
measured on macOS 27.2 (M4), a policy set before `execve` reads back as the
default afterwards. perfratio cannot set them on the exec'd program's thread
from outside without `task_for_pid` (root plus entitlements).

What survives exec is task state, so `realtime` on macOS sets, between fork and
exec:
- `setpriority(PRIO_PROCESS, 0, -20)` (needs root, same hint as `high`);
- the `TASK_FOREGROUND_APPLICATION` task role (`task_policy_set`,
  `TASK_CATEGORY_POLICY`), which read back as role 1 after exec.

This is `high` plus the foreground role. It is **not** equivalent to
`SCHED_FIFO`: threads stay time-shared and can be preempted by any
higher-priority thread. It is documented as such in `--help`, the man page and
the README. A benchmarked program that wants real-time scheduling must call
`thread_policy_set` itself.

## Verification
- Linux (Docker, Rust 1.88) with `--cap-add SYS_NICE`: the benchmarked process
  reports policy 0 / 5 (SCHED_IDLE) / nice −20 / policy 1 (SCHED_FIFO) for
  normal / idle / high / realtime (`/proc/$$/stat`). Without the capability,
  `high` and `realtime` fail with the hint.
- macOS: idle → nice 19; high without root → the hint; realtime without root
  → the same hint (`tests/priority_tests.rs`). Exec resetting thread QoS and
  `THREAD_TIME_CONSTRAINT_POLICY`, and keeping the task role and
  `PRIO_DARWIN_BG`, was checked with a small C program on macOS 27.2 (M4).
  `realtime` as root was not run (no sudo on the test machine).
- Windows: `SetPriorityClass` is type-checked with cross-target clippy only.
