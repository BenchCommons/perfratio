# Spec: `--affinity <CPUS>|p-cores|e-cores`

Issue references: original `--affinity` feature; macOS core classes in #291.

## Problem / Motivation
Migrations between cores, and on hybrid CPUs between performance (P) and
efficiency (E) cores, are a major source of timing noise. On a hybrid CPU a
command that sometimes runs on P-cores and sometimes on E-cores has a bimodal
distribution. Per-core-type counters (`--simd`, `--topdown`, effective
frequency, see `pmu-extensions-common.md`) are only meaningful when the command
stays on one core type.

## Contract
```
--affinity <CPUS>     Linux, Windows: '2', '0-3', '0,2,4-5'
--affinity p-cores    macOS (Apple Silicon): prefer performance cores
--affinity e-cores    macOS (Apple Silicon): keep on efficiency cores
```
It applies to every process perfratio starts: the benchmarked command,
`--prepare`, `--conclude`, `--setup`, `--cleanup` and the shell spawning time
calibration.

| Platform | CPU list | `p-cores` / `e-cores` |
|---|---|---|
| Linux | `sched_setaffinity` in the child before exec (hard) | rejected: "only supported on macOS" |
| Windows | `SetProcessAffinityMask` on the suspended process (hard, CPUs 0-63) | rejected: "only supported on macOS" |
| macOS | rejected, the error suggests `p-cores`/`e-cores` | task policy in the child before exec (preference) |
| other Unix | rejected | rejected |

CPU lists are validated against the configured CPU count. `p-cores` and
`e-cores` are case-insensitive and need `hw.nperflevels` > 1; on Intel Macs
(one core type) they are rejected with "this system has a single core type".
Nothing is added to the JSON export, so the schema version is unchanged.

### macOS mechanism
macOS has no API that pins a process to CPUs.
- `THREAD_AFFINITY_POLICY` (`thread_policy_set`) is only an L2-sharing hint
  between threads of one task, and Apple Silicon does not support it.
- Thread QoS (`pthread_set_qos_class_self_np`, `taskpolicy -c`) picks the core
  type on Apple Silicon, but exec resets a thread's QoS. Measured on macOS 27.2
  (M4): QoS `BACKGROUND` set before `execve` reads back as `DEFAULT` (0x15)
  afterwards.
- Task state survives exec, and the benchmarked program and its children
  inherit it. perfratio therefore sets, between fork and exec:
  - `e-cores`: `setpriority(PRIO_DARWIN_PROCESS, 0, PRIO_DARWIN_BG)`, as
    `taskpolicy -b` does. Apple Silicon runs darwin-background tasks on the
    E-cores only. Side effects: the scheduling priority drops to 4
    (`ps -o pri`), and disk and network I/O are throttled, so use it for
    CPU-bound commands.
  - `p-cores`: it clears `PRIO_DARWIN_BG` and sets the
    `TASK_FOREGROUND_APPLICATION` role (`task_policy_set`,
    `TASK_CATEGORY_POLICY`). Default-QoS threads are already eligible for
    P-cores. This is a preference: under load the scheduler can still move
    threads to E-cores, and a thread can lower its own QoS.

Neither setting needs privileges. Both are documented as scheduling
preferences, not as the hard pinning that Linux and Windows provide.

## Verification
- Unit tests (`src/util/affinity.rs`): CPU list and core-class parsing, core
  class rejected off macOS and on single-core-type Macs, CPU lists rejected on
  macOS with the alternative named.
- Integration tests (`tests/affinity_tests.rs`): on macOS, `e-cores` gives
  the benchmarked shell priority 4 and `p-cores` does not; CPU lists fail with
  the hint. Off macOS, `e-cores` is rejected.
- Measured on an Apple M4 (4 P + 6 E cores, macOS 27.2) with a CPU-bound C loop
  (300M iterations, 8 independent adds/xors), `-N --runs 10 --warmup 2`, debug
  build of perfratio. The machine was busy (load average around 25 from
  parallel builds), so the minimum is the most useful number:

  | `--affinity` | min | median | user time |
  |---|---|---|---|
  | (none) | 560 ms | 779 ms | 403 ms |
  | `p-cores` | 487 ms | 638 ms | 398 ms |
  | `e-cores` | 1145 ms | 1857 ms | 449 ms |

  `e-cores` takes about 2.1x the wall time of `p-cores`, which matches the
  E-core/P-core speed gap. `p-cores` and the default are close because
  default-QoS work already prefers P-cores when they are free.
