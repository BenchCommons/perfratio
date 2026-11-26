# Spec: PMU counter multiplexing detection and scaling (#219)

## Problem / Motivation
References: #219, #49, #213, #217, `hardware-counters.md`, `perf-stat(1)`, `perf_event_open(2)`, `andrewrk/poop`.

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
- JSON: the export shape is the one in `hardware-counters.md` (per-run `u64` arrays
  aligned with `times`, `counters_summary`, `counters_multiplexed_runs`). This module
  supplies the values for it: a per-run entry holds `value_scaled`; a run counts in
  `counters_multiplexed_runs` when any of its counters was scaled; a counter that was
  never scheduled on the PMU (`time_running == 0`) is `null` in that run, matching the
  failed-counter rule there. `CounterScaling` (`time_enabled`, `time_running`, `scaled`,
  `coverage`) is internal. schemaVersion 1.10 exports the lowest coverage per affected
  counter as `results[].counters_coverage` (`{ "cycles": 0.742 }`, absent when nothing
  was multiplexed). Per-run `time_enabled`/`time_running` stay internal: the scaled
  value per run is already in `counters`.

## Implementation
`src/pmu_scaling.rs` holds the pure math, the annotation/warning text and the
`CounterScaling` serializable fields, with unit tests using synthetic records.

## Status / not covered
- The `--counters` flag and the `perf_event_open` reader (#49) are not on `master`.
  `master` has `src/counters.rs` (derived metrics, #213) and `src/pmu.rs` (preflight);
  `hardware-counters.md` plans the reader and multiplex scaling in
  `src/counters/linux.rs`, which should call this module (set `READ_FORMAT` in
  `perf_event_attr`, decode with `RawReading::from_bytes`, scale with `ScaledCounter`).
- Nothing here was run against a real PMU; the machine used was macOS.
  Multiplexing on hardware (e.g. `--counters` with more events than counters) is
  unverified.
