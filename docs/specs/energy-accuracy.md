# Spec: Energy accuracy (#59)

## Problem / Motivation
References: #59, #106 (`rapl-psys-double-counting.md`), #223, #240 (`output-schema-contract.md`),
`Gabriella439/bench#39`, `sharkdp/hyperfine#92`.

`--energy` reports gross package energy. Idle and background power is included, so a 100 ms
command on a 4 W idle laptop is charged about 0.4 J even if it does nothing. Commands with
different durations are biased toward "faster = less energy". Runs shorter than about 10 ms
also give mostly RAPL quantization noise, and nothing warns about it.

## Scope of this change
Delivered:
1. `--energy-baseline <DURATION>` (requires `--energy`): measures idle power once per process
   before the first benchmark, then reports net energy per run
   `net = max(gross - P_idle * time_real, 0)`. Gross energy stays reported and exported
   unchanged. `0` (also `0s`, `0ms`) disables it; it is handled before duration parsing,
   which rejects non-positive values. Negative or invalid values are an error.
2. A warning when `--energy-baseline` is set but the idle measurement fails (no RAPL,
   unreadable `energy_uj`), so the missing net fields are explained instead of silently
   dropped, and a warning when the net energy of any run is clamped to 0 (idle estimate
   above the gross energy), with the count of clamped runs.
3. A warning when `--energy` is used and the mean run time is below 10 ms.

Delivered later in `rapl-domains.md` (#59): RAPL domain breakdown (`--energy-domains`), overhead
alignment of the energy window, the permission message with the udev hint, and the
`#[ignore]` bare-metal test. The list below is the original deferral note.

Deferred, commented on #59 (not done here):
- RAPL domain breakdown (`--energy-domains`, `energy_domains` export): changes the
  `EnergySampler` trait and needs real multi-domain hardware to validate. It must keep the
  #106 rule: the total is the sum of `package-*` domains only; `core`, `uncore` and `dram`
  are sub-zones of a package and `psys` overlaps it, so they are never added to the total.
- Overhead alignment: subtracting the shell's energy as its time is subtracted
  (`ShellExecutor::calibrate`). Needs measurement on RAPL hardware to check the calibration.
- Permission-denied message with the udev rule, and the README udev example.
- Default of 2 s when `--energy` is set. The baseline is opt-in here, so existing runs do not
  gain a 2 s delay.

## Contract
CLI: `--energy-baseline <DURATION>`, same duration syntax as `--timeout`.

Console, with a baseline:
```
Energy (mean):  1.284 ± 0.041 J   net 0.902 J    [Power: 10.40 W, idle 3.10 W]
```

JSON (additive, optional fields, omitted when absent; part of `schemaVersion` 1.1, listed in
`output-schema-contract.md`):

| Field | Unit | Present when |
|---|---|---|
| `idle_watts` | W | `--energy-baseline` and the idle measurement succeeded |
| `mean_net_energy_joules` | J | same, and at least one run has an energy sample |

`mean_energy_joules` and `mean_watts` keep their meaning (gross). CSV is unchanged.

Net energy uses `time_real` of each run, the same time as `mean_watts`. If the idle window
yields no reading (non-Linux, unreadable RAPL), the fields are omitted.

## Verification
- Unit tests: idle power math, mock-sampler idle measurement, net clamp at 0, runner net mean.
- Not run on real hardware: macOS dev machine has no RAPL. The `sleep 0.2` net-energy check
  (`#[ignore]`, Linux with readable `energy_uj`) is still to be done.
