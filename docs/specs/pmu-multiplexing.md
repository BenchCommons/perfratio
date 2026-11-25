# Spec: PMU counter multiplexing detection and scaling (#219)

## Problem / Motivation
References: #219, #49, #217, `perf-stat(1)`, `perf_event_open(2)`, `andrewrk/poop`.

When more hardware events are requested than the PMU has counters, the kernel
time-shares them (multiplexing). A raw read then only covers the time the event
was on the PMU and silently undercounts. `perf stat` avoids this by reading
`time_enabled` and `time_running` and scaling.

## Contract
- Counters are opened with `read_format = PERF_FORMAT_TOTAL_TIME_ENABLED |
  PERF_FORMAT_TOTAL_TIME_RUNNING` (`pmu_scaling::READ_FORMAT`). `read(2)` returns
  `{value, time_enabled, time_running}` (`RawReading`, 24 bytes, native endian).
- `value_scaled = value * time_enabled / time_running` when `0 < time_running <
  time_enabled` (integer, `u128` intermediate, saturating at `u64::MAX`).
  Otherwise the raw value is reported.
- `coverage = time_running / time_enabled`, clamped to `0..=1`; `1.0` if the
  event was never enabled.
- `time_running == 0` while enabled: the event never got a PMU slot. The value is
  not scaled (nothing to extrapolate from), `scaled` is false, `coverage` is 0.0
  and the terminal shows `[not counted: event never scheduled on the PMU]`.
- Terminal: scaled counters get `[scaled: counted 74.2% of the time]`, and one
  warning per benchmark lists the lowest coverage per affected counter.
- JSON (additive, per exported counter object): `time_enabled` (ns),
  `time_running` (ns), `scaled` (bool), `coverage` (0..1). Existing fields keep
  their meaning; `value` fields hold the scaled count. This is an additive
  change: bump the `schemaVersion` minor (see `output-schema-contract.md`) when
  the counter export from #49 lands.

## Implementation
`src/pmu_scaling.rs` holds the pure math, the annotation/warning text and the
`CounterScaling` serializable fields, with unit tests using synthetic records.

## Status / not covered
- `--counters` (#49) is not on `master`, so this change adds the building block
  only. Wiring (setting `read_format` in `perf_event_attr`, reading the fd, adding
  `CounterScaling` to the export and the terminal line) belongs with #49.
- Nothing here was run against a real PMU; the machine used was macOS.
  Multiplexing on hardware (e.g. `--counters` with more events than counters) is
  unverified.
