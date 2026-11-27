# Spec: efficiency metrics, `--work N[unit]`, EDP and ED²P (#373)

**Status:** Shipped, `schemaVersion` 1.35. Parent: #364; builds on the metric descriptors (#317,
`metric-descriptors.md`) and `--pareto` (#326).

## Problem / Motivation
Energy alone does not compare implementations that do different amounts of work, and a
time/energy trade-off needs one figure of merit.

## Contract

### CLI
`--work N[unit]`: the amount of work one run does (`1000000`, `1e6`, `4096ops`; unit defaults to
`op`). `N` must be positive.

### Derived metrics
Computed for every run from the measured series, then summarized like `--output-metric` series
(`custom_metrics`, `custom_metrics_summary`). A value exists only when its inputs do.

| name | formula | unit | better | needs |
|---|---|---|---|---|
| `edp` | `E·t` | J*s | lower | energy |
| `ed2p` | `E·t²` | J*s^2 | lower | energy |
| `time_per_op` | `t/N` | s/unit | lower | `--work` |
| `ops_per_second` | `N/t` | unit/s | higher | `--work` |
| `energy_per_op` | `E/N` | J/unit | lower | `--work`, energy |
| `ops_per_joule` | `N/E` | unit/J | higher | `--work`, energy |
| `edp_per_op` | `(E/N)·(t/N)` | J*s/unit^2 | lower | `--work`, energy |
| `ed2p_per_op` | `(E/N)·(t/N)²` | J*s^2/unit^3 | lower | `--work`, energy |
| `instructions_per_op`, `cycles_per_op` | counter`/N` | counter/unit | lower | `--work`, `--counters` |

`E` is the energy of the run in J and `t` its wall time in s. `edp` and `ed2p` are added whenever
an energy reading exists; the others only with `--work`. `--import-json` with `--work` computes
the per-run values for the imported results that have the inputs and no series of that name.
`--output-metric` cannot use these names.

### Use
`--metric custom:<name>` (a missing input is an error; the energy ones enable `--energy`),
`--pareto custom:edp,wall`, `--budget 'custom:edp<=2'`. Direction and unit follow the descriptor.

### JSON (MINOR, 1.35)
No new keys. `perfratio.metrics[]` lists the derived metrics as `custom.<name>` with
`kind: "derived"` and `source: "perfratio.derived"`; the schema version marks the new values.

### Tests
Unit tests in `src/efficiency.rs`; `tests/efficiency_tests.rs` (with `--import-json` for the
energy paths and `--debug-mode` for time).
