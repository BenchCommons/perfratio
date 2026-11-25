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
  Hardware counters (instructions, cycles, …) are not collected yet, so they have no row.
- **Missing dimensions:** a row appears only if both commands have the value. No `--energy`,
  no RAPL, or an imported result without memory gives no row, never an error or a zero.
  Mean-only values (average power, energy without per-run samples, CPU time of imported
  results) show the delta with no σ, no confidence interval, `-` as p-value and no marker.
  A reference mean of 0 shows `n/a` as delta.
- **Delta:** `(mean_command − mean_reference) / mean_reference`. Confidence interval: the
  95% half-width of the difference of means with pooled variance and Student's t with
  n₁ + n₂ − 2 degrees of freedom, divided by the reference mean (the reference mean's own
  uncertainty is ignored). Shown as `-26.0% ± 1.7%`.
- **p-value:** `stats::deep::compare_samples`, as in `--compare` (needs ≥ 3 runs per side;
  constant, different samples give p = 0).
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
mean-only CPU time, missing and mean-only dimensions, zero reference);
`tests/delta_table_tests.rs` (CLI and Markdown export).
