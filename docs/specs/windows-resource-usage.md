# Spec: `--resource-usage` on Windows (#291)

## Problem / Motivation
`--resource-usage` was Unix only (`wait4` rusage). Windows users got "not available". The goal
of #291 is the same feature on every OS. Windows has no per-process equivalent for some
counters, so the contract must say which ones are unavailable instead of reporting 0.

## Contract
The counters of a run cover the benchmarked process and everything it started, taken from the
job object perfratio already uses for CPU time (`JobObjectBasicAndIoAccountingInformation`).

| JSON column | Unix | Windows |
|---|---|---|
| `voluntary_ctx_switches` | `ru_nvcsw` | unavailable (omitted) |
| `involuntary_ctx_switches` | `ru_nivcsw` | unavailable (omitted) |
| `minor_faults` | `ru_minflt` | `TotalPageFaultCount` of the job: **all** page faults, soft and hard |
| `major_faults` | `ru_majflt` | unavailable (omitted); Windows does not split faults |
| `block_input_ops` | `ru_inblock` | `ReadOperationCount`: read operations issued (file, pipe, console), not device blocks |
| `block_output_ops` | `ru_oublock` | `WriteOperationCount`: write operations issued |

- Unavailable columns are **omitted** from `results[].resources` (never `0`) and shown as `n/a`
  in the `Resources (mean):` line.
- If job accounting cannot be queried, perfratio falls back to the main process only:
  `GetProcessMemoryInfo` (page faults) and `GetProcessIoCounters` (read/write operations).
- Peak memory on Windows comes from `GetProcessMemoryInfo` `PeakWorkingSetSize` of the main
  process (children of an intermediate shell are not included).
- Schema: columns of `resources` became individually optional: `SCHEMA_VERSION` 1.2 → 1.3
  (MINOR, see `output-schema-contract.md`). Unix output is unchanged.

## `--check-system` on Windows
Checks: power source, power plan, CPU load (#206) and the new **CPU frequency limit**
(`CallNtPowerInformation(ProcessorInformation)`: warn if any CPU's `MhzLimit` < `MaxMhz`, i.e. a
reduced maximum processor state or throttling). `perfratio.system.cpu_model` now comes from the
registry `ProcessorNameString` (falls back to `PROCESSOR_IDENTIFIER`). Turbo boost status has no
cheap Windows API and is not checked.

## Tests
Unit: `ResourceCounters::from_windows`, series omission and JSON, frequency limit check.
The Windows API calls are compiled and exercised by the Windows CI job only; they were not run
on real hardware by the author.

Refs: #291.
