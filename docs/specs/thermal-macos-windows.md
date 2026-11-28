# Spec: temperature sampling and `--cooldown until-temp` on macOS and Windows (#412)

**Issue:** [#412](https://github.com/BenchCommons/perfratio/issues/412), part of the
parity matrix of #291 (`platform-parity.md`).
**Status:** macOS done (coarse thermal pressure level). Windows done through the
`Thermal Zone Information` performance counter; it degrades with one warning on hosts
without the counter. JSON `schemaVersion` stays 1.48 (no bump, see "Schema decision").
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
- **Windows.** `MSAcpi_ThermalZoneTemperature` (WMI, `root\WMI`) is missing on most
  desktop and laptop firmware and usually needs administrator rights, and vendor APIs
  are per-vendor, so neither is used. perfratio reads the performance counter
  `\Thermal Zone Information(*)\Temperature` through PDH (`pdh.dll`: `PdhOpenQueryW`,
  `PdhAddEnglishCounterW`, `PdhCollectQueryData`, `PdhGetFormattedCounterArrayW`). It
  needs no administrator rights. The value is the ACPI thermal zone temperature in
  Kelvin, one instance per zone; perfratio converts to °C (`K - 273.15`), drops values
  outside 0 to 150 °C, and uses the hottest zone, as on Linux. The windows-sys crate
  (0.59, already a dependency) gets the feature `Win32_System_Performance`; no new crate.
  An ACPI zone is not always the CPU die: many firmwares expose a board or chassis zone
  that moves slowly. The value is a drift indicator, not a core temperature.
  Without the counter or without instances (many desktops, VMs, CI runners) nothing is
  sampled: `--thermal` prints one warning after the benchmark and `interference.thermal`
  is absent; `--cooldown until-temp` warns once and does not wait. A temperature of 0
  is never reported.
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
  Windows has temperatures, not a level, so it never emits it.
- The levels are a closed enum: adding one is a MAJOR change.
- `environment_stability` warns when `max` is above `nominal` ("the OS reported
  'heavy' thermal pressure during the benchmark"). Otherwise it adds the fact
  "thermal pressure nominal".

### `--check-system` (macOS)
Adds a `Thermal pressure` check: `ok` at `nominal`, `warn` above it. The
`pmset -g therm` speed-limit check stays.

### `--cooldown until-temp` (macOS, Windows)
macOS: warn once and do not wait; there is no CPU temperature without root and
`--thermal` records the pressure level. Windows: waits on the hottest thermal zone
like Linux; with no instance of the counter it warns once and does not wait. A level-based cooldown
(`until-nominal`) was left out: nobody asked for it yet.

## Schema decision (Windows, #412)
`interference.thermal` keeps its name, type, unit (°C) and meaning (hottest sensor),
and it was already optional. Windows only starts to emit it. Per
`output-schema-contract.md` §1 no row of the compatibility table applies: nothing is
added, removed, retyped or made guaranteed, and the JSON schema (and its frozen 1.48
snapshot) never said "Linux only". So `schemaVersion` stays 1.48 and `docs/schema/` is
unchanged. Consumers must already treat the field as optional. What changes is where it
appears: `thermal-macos-windows.md` and `platform-parity.md` now list Windows.

## Verification
- macOS 27.2 (M4), no root: `notify_get_state` returns a level (seen at 1,
  moderate, under parallel builds). Unit tests in `thermal.rs`, `system_check.rs`;
  `tests/cgroup_thermal_tests.rs::macos_thermal_records_the_pressure_level` runs
  the real executor with `--thermal` and checks the exported levels.
- JSON Schema: `docs/schema/perfratio-export.schema.json` and the 1.48 snapshot.
- Windows: the code does not compile on the macOS development host; it is built and
  run by the Windows jobs of CI. Unit tests in `thermal.rs` (Kelvin to °C, implausible
  values, fixture root) run everywhere; `thermal::tests::windows_zone_counter_is_absent_or_plausible`
  and `tests/cgroup_thermal_tests.rs::windows_thermal_records_a_temperature_or_warns`
  run on Windows and accept both outcomes. What the runner showed: see the PR.
- Linux: `read_pressure_level` returns `None`; existing tests cover the sysfs path.
