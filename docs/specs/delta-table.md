# Spec: Multi-dimensional delta table — `--delta-table` (#212)

## Problem / Motivation
References: #212, #240, `andrewrk/poop`, #226 (`--metric`, in flight).

The summary only compares wall time. Memory, energy, power and CPU time are
printed per command, so the user compares them by hand. `--delta-table` prints
one row per measured dimension, comparing each command with the reference.

## Contract
```
--delta-table   After the summary, print a delta table per non-reference command.
                With --export-markdown, append the same tables to the file.
```
- Reference: the `--reference` command, else the fastest (same rule as the summary).
  With fewer than two results nothing is printed. `--style none` hides the terminal table.
- Columns: `Measurement | <reference> (mean ± σ) | <command> (mean ± σ) | Delta (%) | p-val`, then a marker.
- Dimensions (module `src/delta_table.rs`): wall time, peak RSS, energy, average power,
  CPU time (user), CPU time (system). Lower is better in all of them.
  Counter rows (instructions, cycles, cache and branch misses, context switches, CPU migrations, page faults) and the effective CPU frequency (#222, no verdict) appear when both commands were run with `--counters`.
- **Missing dimensions:** a row appears only if both commands have the value. No `--energy`,
  no RAPL, or an imported result without memory gives no row, never an error or a zero.
  Mean-only values (average power, energy without per-run samples, CPU time of imported
  results) show the delta with no σ, no confidence interval, `-` as p-value and no marker.
  A reference mean of 0 shows `n/a` as delta.
- **Delta:** `(mean_command − mean_reference) / mean_reference`.
- **Confidence interval and p-value (one test):** Welch's t-test on the per-run samples
  (needs ≥ 2 runs per side). Standard error `sqrt(s₁²/n₁ + s₂²/n₂)`, Welch-Satterthwaite
  degrees of freedom (fractional), Student's t distribution. The p-value is the two-sided tail
  probability of `t = Δ / SE`; the interval is `Δ ± t₀.₉₇₅(df) · SE`. Both use the same
  statistic and df, so `p < 0.05` exactly when the interval excludes 0 (no bootstrap, so
  the result is deterministic). Two constant samples have no variance: p = 0 if the values
  differ, 1 if equal, interval width 0.
- **Relative interval:** the half-width is divided by the reference mean. This is not a
  ratio (delta-method) interval: the uncertainty of the reference mean in the denominator is
  ignored, which is a good approximation when the reference's relative standard error is
  small. Shown as `-26.0% ± 1.7%`.
- **Failed runs:** `--filter-failed` removes failing commands before the terminal table
  and the Markdown export are built, so both show the same rows.
- **Marker:** `⚡` significant improvement (p < 0.05, delta < 0), `🔻` significant regression,
  `~` not significant, none without a p-value. In the terminal the delta cell is green/red
  for the first two; colors follow `NO_COLOR` and `--style`.
- Markdown: `### Delta: <command> vs reference <reference>` and a table with the same columns.
  Other exporters are unchanged.

## JSON contract (#240)
No change. `--delta-table` only reads existing fields, so `schemaVersion` is untouched.

## Composition with `--metric` (#226)
The module and its single scheduler entry point (`Scheduler::print_delta_table`) do not
touch `src/metric.rs`. A future metric selection can filter `Dimension::ALL`.

## Tests
Unit and snapshot tests in `src/delta_table.rs` (terminal, Markdown, wall time with
mean-only CPU time, missing and mean-only dimensions, zero reference, CI and p-value
against SciPy reference values, p/CI agreement, constant samples);
`tests/delta_table_tests.rs` (CLI, Markdown export, `--filter-failed` in both outputs).
