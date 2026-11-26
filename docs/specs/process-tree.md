# Spec: process-tree cancellation, leftover detection and shared-metric attribution (#309)

**Status:** Shipped in 1.1 (schemaVersion 1.19).

## Problem / Motivation
References: #309, #291 (macOS/Windows parity), `timeout.md`, `ctrlc-handling.md`, `concurrency.md`,
`rapl-psys-double-counting.md`.

With concurrent runs three failures bias results without any sign:
1. A Ctrl-C or `--timeout` that kills only the direct child leaves its descendants running. A
   command that forks a sleeping child, or daemonizes (`setsid`, double fork), keeps using the CPU
   while the next runs are measured.
2. Descendants that outlive the measured command do work outside the measured window.
3. Package energy (RAPL) and system counters cover every process of the machine. Dividing them per
   concurrent run counts the same joules several times.

## Contract

### Every measured process leads its own process tree
- **Unix:** each process is started as the leader of a new process group (`setpgid(0, 0)`), always,
  not only with `--timeout`/`--until`. The terminal no longer delivers Ctrl-C to it, so perfratio
  forwards it (below).
- **Windows:** each process runs in a Job Object of its own (as before, for accounting).

### Cancellation
- **First Ctrl-C** (also SIGTERM, SIGHUP): the tree of every run in flight, every instance of a
  `--concurrency` batch included, gets what the terminal would have sent (SIGINT; on Windows the Job
  Object is terminated). Trees that are still alive after 1 s get SIGKILL. The interrupted run is
  discarded: a partial batch never enters the statistics (`ctrlc-handling.md`); the output says
  `(interrupted after N of M runs)` and the JSON has `interrupted: true`. `--cleanup` still runs.
- **Second Ctrl-C:** SIGKILL to every tree in flight (Job Objects terminated), exit 130.
- **`--timeout`** kills the group of the run (`timeout.md`); with `--concurrency` that is every
  instance's group.
- A run that ends with an error (e.g. a failed spawn of another instance) kills the trees it started.

### Leftover detection
When the main process of a run has exited and was reaped:
- **Unix:** a signal to its group succeeds if a descendant is still there (a group id is not reused
  while the group has members, so this cannot hit another process). The group is killed.
- **Linux:** perfratio sets `PR_SET_CHILD_SUBREAPER` when it first starts a measured process.
  Descendants that left the group (`setsid`, double fork) are handed to perfratio when their parent
  exits. After a run, while no other measured process is running or being spawned, every child of
  perfratio that is not in perfratio's own process group is killed and reaped. Children in
  perfratio's own group are its helpers and are left alone.
- **Windows:** the number of processes still in the Job Object (`ActiveProcesses`) is read after the
  main process exited; if it is not 0 the job is terminated.
- **macOS:** only descendants that stay in the group are found (no subreaper there; tracked in #291).

What is found is reported:
- the benchmark prints a warning: `Processes started by the command were still running when it
  exited ...`;
- the JSON result gets `leftover_processes: true` (only when true), also when `--prepare` or
  `--conclude` left processes behind.

The runs stay in the statistics: the flag tells the reader that their numbers may include or miss the
work of the leftover. After a timeout or a matched `--until` the tree is killed on purpose, so it is
not flagged.

Limitations: with `--concurrency` the orphans of a batch are collected when the last instance of the
batch ends and are attributed to the batch (the flag is per result, not per instance). A descendant
that exits before the check is only found if it is still a zombie of perfratio (Linux) or in the group.
Detection is done after the timer stopped and does not change the measured time; it adds a few
microseconds (one `kill`) per run, plus a read of `/proc/self/task/*/children` on Linux.
PID reuse cannot misattribute a run: processes are only signalled through the group of a leader that
was not reaped yet, or as children of perfratio.

### Shared-metric attribution
- Package energy and other system-wide counters are **never** reported as a per-instance value.
- `energy_per_instance_joules` (1.5, `mean / N`) is no longer written (1.19). The field stays in the
  schema as optional, for files of earlier versions.
- With `--energy` each result has `energy_scope`: `batch` with `--concurrency N`, N > 1 (the energy
  of the whole batch over the makespan), else `system` (the package counter, which covers the whole
  machine; it is the energy of the command only if the machine is otherwise idle, see
  `--energy-baseline`). `process_tree` is reserved for values that are measured for the process tree
  alone (CPU time and memory of a run are, through `wait4` and the Job Object).
- Hardware counters (`--counters`) of a batch are the sum over the instances (each instance has its
  own counters), so they are `process_tree` values summed, not a shared counter.

## JSON (additive, `schemaVersion` 1.18 to 1.19)
`leftover_processes`, `energy_scope`; `energy_per_instance_joules` is no longer written. See
`output-schema-contract.md`.

## Test plan
- `tests/process_tree_tests.rs` (Unix; the daemonizing test needs Linux and `setsid`): `--timeout`
  and Ctrl-C leave no descendant (command forks a sleeping child), also for every instance of a
  `--concurrency` batch; a background child is killed and flagged; a `setsid` child is found through
  the subreaper; a command without children is not flagged; an interrupted batch is not in the
  statistics (N exit codes per run, `instance_times` aligned).
- Unit: `/proc/<pid>/stat` parsing; `energy_scope` rule; run id scopes.
- Windows and macOS equivalents: #291.
