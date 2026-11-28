# Spec: temperature sampling and `--cooldown until-temp` on macOS and Windows (#412)

**Issue:** [#412](https://github.com/BenchCommons/perfratio/issues/412), part of the
parity matrix of #291 (`platform-parity.md`).
**Status:** macOS done (coarse thermal pressure level). Windows not implemented
(documented limit). JSON `schemaVersion` 1.48.
**Related:** `cgroup-thermal.md` (#315), `check-system.md`, `output-schema-contract.md` (#240).

## Problem / Motivation
`src/thermal.rs` reads Linux sysfs only. On macOS and Windows `--thermal` sampled
nothing and `--cooldown until-temp=C` warned that no sensor can be read. On macOS,
`--check-system` reported the `pmset -g therm` CPU speed limit, which is not a
temperature and stays at 100 on Apple Silicon.

## Decision
- **macOS.** No public API gives a CPU temperature without root: SMC keys and
  `powermetrics` need root, and the IOHID temperature sensors are private API.
  The public no-root source is the system thermal pressure level, the
  `com.apple.system.thermalpressurelevel` notification state of
  `<notify.h>` (`notify_register_check` + `notify_get_state`, libSystem). Its
  values are `kOSThermalPressureLevel*` of `<libkern/OSThermalNotification.h>`:
  0 nominal, 1 moderate, 2 heavy, 3 trapping, 4 sleeping.
  `ProcessInfo.thermalState` maps from the same source. perfratio reads this level.
- **Windows.** Not implemented. `MSAcpi_ThermalZoneTemperature` (WMI, `root\WMI`)
  is missing on most desktop and laptop firmware and usually needs administrator
  rights. Vendor APIs are per-vendor. perfratio keeps the clean degradation:
  `--thermal` records nothing and `--cooldown until-temp` warns once and does not wait.
- **Linux.** Unchanged.

## Contract
### `interference.thermal` vs `interference.thermal_pressure`
`interference.thermal` keeps its meaning: degrees Celsius, Linux only. A coarse
level is not a temperature, so it goes in a separate, additive object (JSON
`schemaVersion` 1.47 → 1.48, MINOR):

```json
"interference": {
  "thermal_pressure": { "start": "nominal", "end": "moderate", "max": "moderate" }
}
```

- Only with `--thermal`, sampled on the same background thread as the
  temperature (once per second, never on the benchmark thread). `start` is the
  level before the first run, `end` the last one read, `max` the highest.
- Omitted when the host reports no level (Linux, Windows, a fixture root in tests).
- The levels are a closed enum: adding one is a MAJOR change.
- `environment_stability` warns when `max` is above `nominal` ("the OS reported
  'heavy' thermal pressure during the benchmark"). Otherwise it adds the fact
  "thermal pressure nominal".

### `--check-system` (macOS)
Adds a `Thermal pressure` check: `ok` at `nominal`, `warn` above it. The
`pmset -g therm` speed-limit check stays.

### `--cooldown until-temp` (macOS, Windows)
Unchanged behaviour: warn once and do not wait. The warning now says why: on macOS
there is no CPU temperature without root and `--thermal` records the pressure
level; on Windows the sensors are not read yet. A level-based cooldown
(`until-nominal`) was left out: nobody asked for it yet.

## Verification
- macOS 27.2 (M4), no root: `notify_get_state` returns a level (seen at 1,
  moderate, under parallel builds). Unit tests in `thermal.rs`, `system_check.rs`;
  `tests/cgroup_thermal_tests.rs::macos_thermal_records_the_pressure_level` runs
  the real executor with `--thermal` and checks the exported levels.
- JSON Schema: `docs/schema/perfratio-export.schema.json` and the 1.48 snapshot.
- Windows: no code path added; nothing to verify beyond the existing warning.
- Linux: `read_pressure_level` returns `None`; existing tests cover the sysfs path.
