# Spec: `--complexity <VAR>` empirical complexity fit (#374)

**Status:** Shipped, `schemaVersion` 1.33. Parent: #303; builds on parameter scans (#327).

## Problem / Motivation
A parameter scan (`-P n ...`) gives a curve but no model of how the cost grows. Google Benchmark
reports a best-fit complexity class; perfratio does the same for time, memory, energy and
instructions.

## Contract

### CLI
`--complexity VAR` (conflicts with `--optimize` and `--import-json`). Every command must contain
`{VAR}`; the values come from `-P`, `-L` or `--parameter-file`. Values must be positive numbers.
Results are grouped by command template and by the other parameters; each group needs at least
3 distinct values, else perfratio warns and reports no fit for it.

### Method
For each metric, the group's central values `y(n)` (mean by default, `--estimator` applies) are
fitted with `y = c·g(n)` by least squares, one coefficient and no offset, for
`g` = 1, ln n, n, n ln n, n², n³ (O(1), O(log n), O(n), O(n log n), O(n^2), O(n^3)).
The best model has the lowest RMS error; a steeper model displaces a simpler one only if its RMS
is lower by more than a relative 1e-9. `rms_relative` is the RMS divided by the mean of `y`;
`r2` is `1 - SSres/SStot` (can be negative for a no-offset model).
Metrics: `wall` always, `memory`, `energy`, `instructions` when every point has them.

This is an **empirical fit of a few noisy points, not a proof** of the asymptotic complexity:
lower-order terms and the measured range of `n` decide which model wins.

### JSON (MINOR, 1.33)
Optional top-level `complexity`: array with one entry per group:
`parameter`, `command` (with `{VAR}`), `fixed` (other parameters), `values`, `metrics.<name>`
(`unit`, `best`, `coefficient`, `rms`, `rms_relative`, `r2`, `models[]` with the same numbers for all
six models) and `note`. Schema: `docs/schema/perfratio-export.schema.json`.

### Tests
Unit tests in `src/complexity.rs` with synthetic data for each model, noise and grouping;
`tests/complexity_tests.rs` end to end with `--debug-mode` and the schema.
