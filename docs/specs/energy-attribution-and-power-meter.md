# Spec: Estimated energy attribution and external power meter (#381, #382)

**Issues:** #381 (process and container attribution, estimated), #382 (external power meter).
**Related:** #364 (parent), #317 (`metric-descriptors.md`, `kind: estimated`), #337,
`energy-accuracy.md`, `rapl-domains.md`, `output-schema-contract.md`, `cgroup-thermal.md`.
**JSON:** `schemaVersion` 1.37, additive only. The open pull requests #393 to #400 also use 1.37,
so whichever merges second resolves the version by rebase (renumber to the next free MINOR,
move `docs/schema/1.37/` and the constants). The fields of the two changes do not overlap.

## Problem / Motivation

### #381
RAPL measures a package, not a process. With `--concurrency`, `--jobs`, or any other work on the
host, part of the measured energy is not the benchmark's. Scaphandre and Kepler split host
energy by CPU-time share. Users want the same number next to the measurement, clearly marked as a
model output.

### #382
Wall-plug energy is the reference for validating RAPL and the other backends. No built-in backend
reads an external meter (smart plug, Shelly over HTTP, a bench instrument behind a script).

## Contract

### 1. `--energy-attribution <MODEL>` (requires `--energy`)

`MODEL`:

| Value | `model` in JSON | `scope` | Share |
|:--|:--|:--|:--|
| `cpu-time` | `cpu-time-share` | `process` | (user + system CPU time of the command) / (busy CPU time of the host during the window) |
| `cgroup` | `cgroup-cpu-share` | `cgroup` | (CPU time of the own cgroup, descendants included) / (busy CPU time of the host during the window) |

`estimated = window energy x min(share, 1)`, per run.

- Window energy is the value the sampler returned for the whole window, before the alignment to
  the command lifetime (`align_to_child`), because the host CPU delta covers the same window.
- The command CPU time is the one the executor measured, before `--subtract`.
- Busy host time is read from the `cpu` line of `/proc/stat`: `user + nice + system + irq +
  softirq + steal`. `idle`, `iowait` and `guest` (already in `user`) are not busy. Ticks are
  converted with `sysconf(_SC_CLK_TCK)`.
- Cgroup time is `usage_usec` of `cpu.stat` (v2) or `cpuacct.usage` (v1, when `cpuacct` shares
  the `cpu` directory) of the own cgroup. Inside a container this is the container.
- The measured energy is not changed. `mean_energy_joules`, `energy_joules` and the net energy
  keep their meaning (`energy-accuracy.md`).
- `cycle-share` (cycles of the command over cycles of the host) is **not** in this change. It
  needs a system-wide cycle counter (`CAP_PERFMON` or `perf_event_paranoid` <= -1) that cannot be
  validated without that hardware. The model type is open (`model` is an open enum), so adding it
  is MINOR.

#### Assumptions and error
- Idle power is part of the window energy, so it is part of the attributed share. On a quiet host
  the command gets close to 100% of the package energy, idle power included, as in Scaphandre.
  Use `--energy-baseline` for the net energy of the whole run.
- Power is not proportional to CPU time (frequency, SIMD width, memory traffic, SMT sharing).
  The estimate is an order of magnitude for the split between processes, not a measurement.
- `/proc/stat` counts 10 ms ticks. With less than 50 ms of host busy time in a window the share is
  coarse: a warning counts those runs.
- Shares above 1 (tick quantization, CPUs of a cgroup with a larger quota than the host window)
  are clamped to 1, counted in `clamped_runs`, and warned about.
- Work outside the benchmark that does not use CPU (GPU, disk, DRAM traffic of other processes)
  stays in the other 1 - share.
- With `--energy-backend meter` the same split is applied to the energy of the machine behind the
  meter.
- A run without a window energy or without a readable host delta has no estimate (never 0).

#### Degradation
Where `/proc/stat` (or, for `cgroup`, the cgroup CPU counter) cannot be read (macOS, Windows,
restricted containers), the run continues and prints once on stderr: `Warning:
'--energy-attribution ...' needs /proc/stat ... it is unavailable here, so no estimated energy is
reported. The measured energy is unaffected.` No `energy_attribution` is exported.

#### Console
```
Energy (mean):  1.250 J    [Power: 12.50 W]
Energy (estimated):  0.312 J    [cpu-time-share, process scope, share 25.0%]
```

#### JSON (new in 1.37): `results[].energy_attribution`
```json
"energy_attribution": {
  "kind": "estimated",
  "model": "cpu-time-share",
  "scope": "process",
  "samples": 3,
  "clamped_runs": 0,
  "mean_share": 0.25,
  "mean_joules": 0.3125,
  "joules": [0.3125, 0.3125, 0.3125]
}
```
- `kind` is the constant `estimated`. A consumer must never treat these values as measured.
- `joules` is present only when every run has an estimate (same rule as `energy_joules`).
- `model` and `scope` are open enums.
- `perfratio.metrics[]` gets a descriptor `energy.attributed` (`unit` J, `kind` `estimated`,
  `scope` `process`, `fields` `["energy_attribution"]`), listed only when the field is present.

### 2. `--power-meter <COMMAND>` (requires `--energy`)

Selects the energy backend `meter` (also `--energy-backend meter`, `PERFRATIO_ENERGY_BACKEND=meter`
with `--power-meter`). Giving `--power-meter` together with another explicit backend is an error;
`--energy-backend meter` without `--power-meter` is an error. Works on every OS.

`COMMAND` runs through the shell (`sh -c`, `cmd /C` on Windows) and prints one number on stdout.
Stderr is ignored, the command gets no stdin, at most 4 KiB of stdout is read, and a command that
runs longer than 5 s is killed and counts as a failed reading.

| `--power-meter-mode` | The command prints | Window energy |
|:--|:--|:--|
| `power` (default) | instantaneous power: `12.5`, `12.5W`, `800 mW`, `0.1 kW` | integral of the readings over the window |
| `counter` | cumulative energy: `3.2`, `3.2 J`, `10 kJ`, `1.5 Wh`, `800 mWh`, `0.1 kWh` | reading after the run minus reading before it |

Units without a suffix are W and J. Negative, NaN, infinite or unparsable output is an error. A
Shelly, Yokogawa or serial meter is read by a script that prints the number
(`curl -s http://plug/rpc/Switch.GetStatus?id=0 | jq .apower`, `yokogawa-read.sh`).

#### Alignment to the run windows
- `power` mode: a poller thread runs the command every `--power-meter-interval` (default 500 ms)
  from the start of the process. Each reading is stamped with the middle of its execution. The
  window energy is the integral over `[start, stop]` of a zero-order hold: each reading holds until
  the next one, the first reading holds before itself, the last holds after itself. The existing
  `align_to_child` scaling then applies as for RAPL.
- If the last reading is older than `3 x interval + 1 s` at the end of a window (the meter stopped
  answering), that run has no energy sample (never an old value).
- `counter` mode reads before and after every run, outside the timed command. The command latency
  is added to the window, which `align_to_child` partly corrects.
- Error: in `power` mode up to one interval of staleness per window edge (a run of 200 ms with a
  500 ms interval is dominated by the hold). Use an interval well below the run time, or `counter`
  mode with a meter that integrates in hardware.

#### What the value means
The value is what the meter sees, normally the **whole machine** (wall power), not a package. It
includes the PSU loss, the display, fans and every other process. Do not compare it with RAPL
numbers without that in mind. This is the point: it is the reference to validate RAPL against.

#### Degradation
If the first reading fails (no such command, non-zero exit, timeout, unparsable output), perfratio
prints once `Warning: the external power meter cannot be read: <reason>`, runs the benchmark
without energy, and exports `perfratio.measurements.energy = "unavailable"` with
`energy_unavailable_reason` `meter_unreadable`. A readable meter that fails later only loses the
affected runs (`energy_samples` below the run count).

#### Secrets
The command can carry credentials (URL token, password). It is never exported: the `--power-meter`
value in `perfratio.command_line` is replaced by `<redacted>`. It does appear in the process list
of the machine while it runs, as any command line does.

#### JSON (new values in 1.37)
- `perfratio.energy_backend`: `external_meter` (open enum).
- `perfratio.measurements.energy_unavailable_reason`: `meter_not_configured`, `meter_unreadable`
  (open enum).
- No new field. `perfratio.metrics[]` descriptors of the energy group take `source`
  `external_meter`.

## Versioning
`SCHEMA_VERSION` 1.36 -> 1.37 (MINOR, additive). `docs/schema/1.37/perfratio-export.schema.json`
is the frozen copy. Consumers that ignore unknown keys (the contract) keep working. A consumer
that reads `energy_backend` as a closed enum must accept `external_meter`.

## Tests
- Unit, no hardware: share model, `/proc/stat` parser on a fixture
  (`tests/fixtures/proc_stat_busy.txt`), the Linux source on a fixture tree, cgroup v1/v2 CPU
  usage, meter reading parser, zero-order-hold integration, power and counter samplers with a
  mocked reader, command reader on shell commands.
- Integration: `--debug-mode` simulates host CPU and energy (`tests/energy_attribution_tests.rs`);
  shell scripts play the meter on Unix (`tests/power_meter_tests.rs`).
- Not covered: real RAPL, a real meter, real `/proc/stat` under load (CI covers time and memory
  only; energy needs bare-metal Linux).

## Out of scope
- `cycle-share` model (needs a system-wide cycle counter).
- Per-process attribution of other processes on the host, Kubernetes pod attribution.
- Native Shelly/Yokogawa/serial protocols: the command is the adapter.
- Attribution of `--energy-domains` values, of net energy, or per `--concurrency` instance.
