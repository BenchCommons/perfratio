# Spec: memory bandwidth and NUMA metrics (#378)

**Status:** Implemented (schemaVersion 1.37, additive). The fixture tests run in CI. Not run on
hardware: the uncore counters and a real `numa_maps` sample need bare-metal Linux (bare-metal
check in #337).

## Problem / Motivation
References: #378, #49 (parent), #337, LIKWID, hwloc, `perf-list(1)` (`uncore_imc`),
`numa(7)` (`/proc/<pid>/numa_maps`).

A memory-bound algorithm is limited by DRAM bandwidth and by where its pages sit, not by cache
misses alone. `--counters` shows misses but not the bytes that cross the memory controller, and
nothing records on which NUMA node the pages of the benchmarked process live. Two runs of one
program can differ by 30% only because the allocator put the data on the remote node. perfratio
reports the three facts a reader needs to explain such a gap: bandwidth, placement and topology.

## Contract

### Activation
`--membw` (no value). It is independent of `--counters`. Without the flag nothing is opened and no
field is added, except `perfratio.environment.numa_topology`, which is part of the environment
block on every Linux export. `--import-json` ignores the flag.

### Bandwidth (Intel uncore IMC)
- Discovery: every `/sys/bus/event_source/devices/uncore_imc*` PMU (not `*free_running*`) with
  `type`, `cpumask`, `format/event`, `format/umask`, `events/cas_count_read` and
  `events/cas_count_write` with their `.scale` and `.unit`. The `config` value is built from
  the `format/` bit ranges, so no event number is hard coded.
- Counting: one system-wide event per PMU and direction (`pid = -1`, `cpu` = first CPU of
  `cpumask`), opened once at start-up and read right before the run and right after the child
  is reaped. Counters are 48 bits wide; the difference is taken modulo 2^48.
- Bytes: `count * scale * unit` (a CAS count is a 64-byte transfer). Read and write are summed
  over all PMUs (channels and sockets).
- **Scope is the machine, not the process.** Uncore counters have no per-process filter, so the
  value includes every other process and the kernel during the run. The terminal and the schema
  say so. Quiet the machine (`--check-system`, `--cooldown`) before trusting it.
- `bytes_per_second = (read + write) / wall time of the run`.
- Timed-out runs and runs of `--concurrency N > 1` have no reading (`null`): the system-wide
  delta of overlapping instances cannot be split.
- Warmup runs are not counted.

### NUMA placement (`numa_maps`)
- A sampler thread reads `/proc/<pid>/numa_maps` of the spawned child every 20 ms from the
  moment it exists, sums `N<node>=<pages>` times `kernelpagesize_kB` (4 kB if absent) per node,
  and keeps the sample with the largest total. That is peak resident memory per node, not
  allocation history.
- Runs shorter than the first successful read have `null`. Without `-N` / `--shell=none` the pid
  is the shell, so use one of them to sample the command.
- Not sampled while `--region-scope` is on: both need the single spawn hook.
- `mean_node_fraction` is the per-run share of each node averaged over the sampled runs;
  `dominant_node` is the node with the largest mean share. The spec does not label a node
  "local" or "remote": that needs the CPU the process ran on (use `--affinity` and
  `numa_topology.cpus`).

### Topology
`perfratio.environment.numa_topology`: from `/sys/devices/system/node/node<N>/`:
`cpulist`, `meminfo` (`MemTotal`), `distance` (ACPI SLIT). Each field is omitted when its file
is missing; the array is omitted when there are no node directories. It is **not** in the
environment fingerprint, so the fingerprint of existing files is unchanged.

### Degradation
One warning on stderr and a reason in the export; the run never fails.

| Situation | `measurements.membw` | reason |
| --- | --- | --- |
| not Linux | `unavailable` | `unsupported_platform` |
| no `uncore_imc` PMU (VM, Arm, old CPU) | `unavailable` | `no_uncore_imc` |
| AMD `amd_df` present | `unavailable` | `amd_df_unsupported` |
| `perf_event_open` denied | `unavailable` | `perf_paranoid` |
| other open error | `unavailable` | `other` |

`measurements.numa` is `unavailable` with `unsupported_platform` (not Linux) or `no_numa_maps`
(kernel without `CONFIG_NUMA`). One source can work without the other.

AMD Data Fabric events differ by family and are not mapped in this version; an AMD host gets
the NUMA part and the topology only.

### Terminal
```
  Memory:             3.0 GB/s (read 2.0 GB/s, write 1.0 GB/s); NUMA node0 80%, node1 20%
```

### JSON (schemaVersion 1.37, additive: MINOR)
Per result (all optional, present with `--membw` when there is data):
```jsonc
"membw": {
  "source": "uncore_imc",
  "read_bytes": [2000000000, null, ...],      // aligned with times
  "write_bytes": [1000000000, null, ...],
  "bytes_per_second": [3.0e9, null, ...]
},
"membw_summary": { "read_bytes_per_second": {...}, "write_bytes_per_second": {...},
                   "bytes_per_second": {...} },        // mean, stddev?, median, min, max
"numa": {
  "node_bytes": [{"0": 83886080, "1": 20971520}, null, ...],
  "mean_node_fraction": {"0": 0.8, "1": 0.2},
  "dominant_node": 0
}
```
`perfratio.measurements`: `membw`, `membw_unavailable_reason?`, `numa`, `numa_unavailable_reason?`
(`not_requested` without the flag). `perfratio.environment.numa_topology`:
```jsonc
[{ "node": 0, "cpus": "0-15,32-47", "memory_bytes": 67108864000, "distances": [10, 21] }, ...]
```
`perfratio.metrics[]` gains `membw.read`, `membw.write`, `membw.total` (`bytes/s`, scope `system`,
direction `neutral`) and `numa.node_bytes` (`bytes`, scope `process`).

Versioning: all fields are new and optional, so this is MINOR: **1.36 to 1.37**. Other open pull
requests (#393 to #400) also use 1.37; whichever merges second rebases and takes the next number,
and the schema snapshot `docs/schema/1.37/` and the version line in `output-schema-contract.md`
are the places to renumber. `tools-benchmark` reads any 1.x and ignores unknown keys. With
`--debug-mode` the values are synthetic (`source: "mock"`).

## Edge cases
- **Multi-socket:** one PMU per channel and socket; all are summed. Per-socket values are not
  exported.
- **Hybrid or client CPUs without `uncore_imc`:** `no_uncore_imc`. Client parts expose
  `uncore_imc_free_running_*` events, which are not used.
- **Counter wrap:** handled by the 48-bit modular difference; two wraps in one run are not
  detectable (a 64 B x 2^48 transfer is 18 PB, not reachable in a run).
- **Short runs:** no NUMA sample (`null`). Bandwidth of a run of a few milliseconds is dominated
  by startup and other processes.
- **Huge address spaces:** reading `numa_maps` walks page tables and can cost milliseconds per
  read; the 20 ms interval bounds the overhead but it is not zero. Do not combine with a
  timing comparison against runs without `--membw`.
- **Containers:** `/sys/bus/event_source` is usually readable but `perf_event_open` is denied by
  seccomp: `perf_paranoid`.

## Test plan
- Unit (`src/membw.rs`): fixture trees for Intel IMC discovery (two PMUs, free-running skipped,
  cpumask, scale and unit), no PMU, AMD DF, incomplete PMU; event config from format terms;
  48-bit wrap; `numa_maps` parsing (huge pages, interleave, no node tokens); node fractions with
  missing samples; rate formatting.
- Unit (`src/environment.rs`): one- and two-node topology fixtures, no NUMA, JSON round trip,
  fingerprint unchanged.
- Integration (`tests/membw_tests.rs`, `--debug-mode`): export fields and values, absence without
  the flag, terminal line, help text; real mode degrades without failing.
- Schema (`tests/json_schema_tests.rs`, `metric_descriptor_tests.rs`): the `membw` case validates
  against the strict schema and has descriptors.
- Hardware (#337, not CI): compare `bytes_per_second` with `perf stat -a -e
  uncore_imc/cas_count_read/,uncore_imc/cas_count_write/` and with `likwid-perfctr -g MEM`;
  a pinned streaming benchmark on node 0 versus `numactl --membind=1` must move `numa.dominant_node`.

## References
#378, #49, #337, #240; LIKWID (`MEM` group), hwloc (`lstopo`), `perf_event_open(2)`, `numa(7)`,
Linux `arch/x86/events/intel/uncore_snb.c` and `uncore_snbep.c` (IMC events).
