# Specification: Per-metric regression budgets (`--budget`)

**Issue:** #325. **Status:** shipped in 1.1 (`schemaVersion` 1.29 or later). Extends `compare-baseline.md`.

## Problem / Motivation
`--compare baseline.json --fail-if-regressed 5%` applies one relative threshold to wall time and
treats "higher" as worse. A real gate needs a budget per metric with a direction: wall +5%, peak
memory +10%, energy +3%, instructions +1%, throughput not more than 2% lower, and an absolute cap
(memory under 256 MiB).

## CLI
```
--budget <METRIC><op><VALUE>   repeatable
--budget-file <FILE>           repeatable; TOML or JSON
--on-unavailable fail|warn|skip   default warn
```
`METRIC` is any metric of `--metric` (`wall`, `cpu`, `user`, `system`, `energy`, `memory`,
`instructions`, `cycles`, `custom:<name>`), optionally with a statistic (`wall.p95`, `memory.max`, as
in `--pareto`), or `throughput` (instances per second of a `--concurrency` batch, the only metric
where higher is better today).

| Budget | Meaning |
|---|---|
| `wall=+5%`, `wall<=+5%` | relative: `current / baseline - 1` must be at most +5% |
| `throughput=-2%`, `throughput>=-2%` | relative: the change must be at least -2% (higher is better, so a lower bound) |
| `memory=10%` | unsigned: the tolerated worsening, i.e. `+10%` for lower-is-better and `-10%` for higher-is-better metrics |
| `wall=-10%` | the run must be at least 10% faster than the baseline |
| `memory<=256MiB`, `wall<=50ms`, `energy<=3J`, `throughput>=100` | absolute: the value itself, units as for `--constraint` |

`=` takes the operator from the direction of the metric; `<=` and `>=` are literal. Strict `<`
and `>` are rejected. A relative budget needs `--compare`; an absolute budget does not.
`--fail-if-regressed PCT` is `--budget wall=+PCT`, with its output and exit code unchanged.

`--budget-file`: a TOML file with `budgets = ["wall=+5%", "memory<=256MiB"]` (comments and a
multi-line array are fine; no other TOML is read), or a JSON array of strings or `{"budgets": [...]}`.
The format follows the extension (`.toml`, `.json`; otherwise a leading `[` or `{` means JSON).
File budgets and `--budget` options are all applied.

## Evaluation
For every compared command (relative) or every result (absolute):
- A value that is missing in the baseline or in the current run (unavailable metric, no counters,
  no energy backend) makes the budget `not_evaluated`. It is never read as 0 and never a silent pass.
  `--on-unavailable` decides: `fail` counts it as failed, `warn` (default) prints a warning and leaves
  the exit code, `skip` leaves it out of the terminal and Markdown reports (the JSON still lists it).
- A baseline of 0 or less gives no ratio: `not_evaluated` (`baseline_not_positive`).
- Relative budgets and noise: a change inside the limit is `pass`. A change beyond it fails only if
  the difference is significant (the Welch test of `--compare`, level `--significance-level`,
  estimator `--estimator`); otherwise it is `within_noise` and passes. With fewer than 3 runs per
  side the test is undefined: `not_evaluated` (`too_few_runs`). Without per-run samples (`throughput`)
  the point estimate decides. The dedicated noise threshold and equivalence margin of #323 are not in
  yet; when they are, they replace this test.
- Absolute budgets are hard caps on the value (the `--estimator` centre for a plain metric, the named
  statistic otherwise) and are not softened by noise.
- Exit code 3 if any budget fails (any `not_evaluated` too with `--on-unavailable fail`); message
  `N budget(s) failed ('--budget')`. The terminal, the JSON and the Markdown are written first.

## Output
Terminal: a `Budgets: pass|fail|not_evaluated` table with command, metric, baseline, current,
change, budget and verdict, and a warning per budget that was not evaluated. The same table is
appended to `--export-diff-markdown`. The table is printed for explicit budgets only, so a plain
`--fail-if-regressed` run looks as before.

## Contract (JSON, additive, `schemaVersion` 1.28 to 1.29)
`perfratio.compare`, present when a budget or `--fail-if-regressed` is given:
```json
{ "baseline": "base.json", "on_unavailable": "warn", "verdict": "fail",
  "budgets": [ { "command": "app", "metric": "wall", "kind": "relative", "op": "<=",
                 "limit": 0.05, "budget": "wall=+5%", "baseline": 0.1, "current": 0.2,
                 "change": 1.0, "p_value": 0.0, "verdict": "fail" } ] }
```
- `verdict`: `pass`, `fail`, or `not_evaluated` (nothing could be evaluated).
- Entry `kind`: `relative` (`limit` is a fraction, 0.05 = +5%) or `absolute` (`limit` in seconds, bytes,
  joules or counts). `op` is `<=` or `>=`. `baseline`, `current`, `change`, `p_value`: omitted when
  unavailable or not applicable, never 0. Entry `verdict`: `pass`, `fail`, `within_noise`,
  `not_evaluated` (with `reason`: `unavailable_in_baseline`, `unavailable_in_current`,
  `baseline_not_positive`, `too_few_runs`; open enum).
- The `compare` block carries no environment comparison. The environment check against the baseline (#314, merged) is separate: it is a stderr warning, or an error with `--require-compatible-env` (`environment-fingerprint.md` §2.4).

## Tests
`src/budget.rs` unit tests (parsing, table of verdicts by direction, noise, absolute, each
`--on-unavailable` value, files); `tests/budget_tests.rs` (exit codes, absolute without a baseline,
JSON block, Markdown, files).
