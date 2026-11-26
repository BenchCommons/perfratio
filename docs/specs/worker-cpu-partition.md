# Spec: `--pin-workers` and `--reserve-cpus`, CPU partitioning of the `--jobs` workers (#307)

**Status:** Shipped in 1.1 (schemaVersion 1.24). Depends on `worker-pool.md` (#306); reuses the
`--affinity` plumbing (`affinity.md`). Parent tracker: #53.

## Problem / Motivation
References: #307, #306, #63 (`--affinity`), #291 (macOS), `megafine` (`--no-pin`, `--pin-reserved`),
pyperf CPU isolation.

With `--jobs N` several runs execute at the same time. Without partitioning, two workers can
share a core, and perfratio's own threads (progress bar, output parsing, counter reads) can steal
time from a measured run. `--affinity` pins the whole benchmark to a CPU set but does not
divide that set among workers.

## Contract
```
--pin-workers[=<auto|none>]   Give every worker its own CPUs (default with --jobs > 1: auto).
--reserve-cpus <N|LIST>       Keep CPUs for perfratio's own threads; workers never run there.
```
Both need `--jobs` > 1 (error otherwise). `--reserve-cpus` needs pinned workers
(`--pin-workers=none` is an error with it).

### Partition
Pure function `util::cpu_partition::partition(topology, reserve, jobs)`:
1. The allowed CPUs are the `--affinity` list, or the CPUs of the process (Linux
   `sched_getaffinity`, so `taskset` and cgroup cpusets are respected; Windows
   `GetProcessAffinityMask`; other: all CPUs).
2. `--reserve-cpus <LIST>` reserves exactly these CPUs (each must be allowed).
   `--reserve-cpus <N>` reserves the `N` highest allowed CPUs **and their SMT siblings**
   (a worker on the sibling of the runner's CPU would share its core). A bare number is a count;
   write `0-0` for CPU 0.
3. Usable = allowed minus reserved. Fewer usable CPUs than jobs is an error that names both
   numbers (under `--pin-workers=none` it is only the warning of #306).
4. Physical cores come from the SMT sibling groups (Linux:
   `/sys/devices/system/cpu/cpuN/topology/thread_siblings_list`; elsewhere every CPU is a core,
   so Windows cannot tell siblings apart). **One physical core per worker**, lowest cores first,
   with all its usable hardware threads together; remaining cores stay idle.
5. With fewer cores than workers, every worker gets one hardware thread (first threads of all
   cores, then second threads) and `shared_siblings` is set: some workers share a core.

Worker sets are disjoint and never contain reserved CPUs (unit tests over several topologies).

### Where it applies
- A worker's CPUs replace `--affinity` for the benchmarked command and for `--prepare` /
  `--conclude` of that run. Warmup, the initial measurement, `--setup` and `--cleanup` are not
  run by workers and keep the global `--affinity`.
- While the pool runs, perfratio's own threads (main thread and the worker threads) are pinned to
  the reserved CPUs (Linux `sched_setaffinity` on the thread, Windows `SetThreadAffinityMask`);
  the previous affinity is restored afterwards. Children inherit the thread's mask on Linux until
  the explicit worker mask is applied between fork and exec.

| Platform | Mechanism |
|---|---|
| Linux | `sched_setaffinity` in the child before exec (as `--affinity`) |
| Windows | `SetProcessAffinityMask` on the suspended process; CPUs 0-63 (one processor group) |
| macOS | no API to pin to CPUs. An explicit `--pin-workers` / `--reserve-cpus` warns and the workers are not pinned. `--affinity p-cores|e-cores` (#298) still applies to all workers as a core type preference |
| other | warns as on macOS |

The default (`auto`) never warns on a platform without pinning, because nothing was asked for.
`--debug-mode` starts no processes, so the default does not partition there; an explicit flag does.

### `--check-system`
Adds "Worker CPUs": ok with the partition, or a warning when `shared_siblings` is set (more
workers than physical cores), with the hint to use fewer jobs or `--pin-workers=none`.

### JSON (additive, MINOR bump to 1.24)
```jsonc
"perfratio": { "execution": { "jobs": 2, "schedule_policy": "saturate", "contended": true,
  "cpu_partition": { "workers": [[0,1],[2,3]], "reserved": [15], "shared_siblings": true } } }
```
`reserved` is omitted when empty and `shared_siblings` when false. `cpu_partition` is absent when
the workers were not pinned.

## Test plan / verification
- Unit (`src/util/cpu_partition.rs`): no SMT, SMT with adjacent and distant siblings, restricted
  affinity, fewer CPUs than jobs, reserved count and list, disjointness and exclusion of reserved
  CPUs over many topologies, JSON shape.
- Integration (`tests/cpu_partition_tests.rs`): option errors, JSON, the "more jobs than CPUs"
  error, the macOS warning. On Linux a real run prints `Cpus_allowed_list` from every worker's
  command and compares it with the exported partition and the reserved CPUs.
- `--pin-workers=none` is added to the real-process tests of `--jobs` so that they do not depend
  on the CPU count of the runner.
- Not run on hardware with SMT or on Windows by the author; the partition function covers the
  topologies, CI covers the platform calls.

## Not done
- SMT topology on Windows and macOS (`GetLogicalProcessorInformationEx`); NUMA awareness; more
  than 64 CPUs on Windows.
- A reserved-CPU count that picks the lowest CPUs (CPU 0 often handles interrupts); the highest
  are taken to match the example of #307.
