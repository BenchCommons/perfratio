# Spec: Pareto frontier and constraint-aware comparison (#326)

## Problem / Motivation
References: #326, #212 (`delta-table.md`), #317, #320, #323.

Ranking implementations on wall time alone hides trade-offs: the fastest command may use
the most energy or memory. The delta table shows every dimension, but the reader still has
to find the commands that no other command beats everywhere. `--pareto` reports that set
(the Pareto frontier) without inventing a ranking across incomparable dimensions.
`--constraint` removes commands that break a limit, and `--objective` picks one winner.

## Contract
```
--pareto <METRICS>            Comma-separated metrics, e.g. wall,energy,memory.
--constraint '<METRIC> <op> <VALUE>'   Repeatable. <op> is <=, <, >= or >.
--objective <minimize|maximize>:<METRIC>
--noise-threshold <PERCENT>   Default: strict comparison. Also the noise threshold of the significance markers (effect-size-equivalence.md, #323).
```
- **Metric:** any `--metric` name (`wall`, `cpu`, `user`, `system`, `energy`, `memory`,
  `instructions`, `cycles`, `custom:<name>`), optionally followed by a statistic of the
  per-run samples: `.mean` (default), `.median`, `.min`, `.max`, `.pNN` (0 to 100, linear
  interpolation as in `stats::summary::percentile`). Example: `wall.p95`.
- **Value:** a number with an optional unit. Time (`ns us µs ms s min`, bare number =
  seconds), memory (`B KB KiB MB MiB GB GiB`, bare = bytes), energy (`uJ mJ J kJ`, bare =
  joules). Other metrics take a plain number.
- **Candidates:** commands that have data for every metric used by `--pareto`,
  `--constraint` and `--objective`, and satisfy every constraint. Others are `excluded`
  with the reason `metric_unavailable` (naming the metric) or `constraint_violated`
  (naming the constraint). An unavailable metric is never read as 0.
- **Direction:** from the metric. All current metrics are lower-is-better (a table in
  `src/pareto.rs` until the descriptors of #317 exist); `--objective maximize:<METRIC>`
  flips the direction of that metric for the frontier too. The analysis core takes explicit
  directions and is tested with a higher-is-better axis.
- **Dominance:** `a` dominates `b` if `a` is not worse on any metric and better on at least
  one. Equal points do not dominate each other, so both stay on the frontier. The frontier
  is the set of candidates that nothing dominates.
- **Noise:** with `--noise-threshold P`, "better" and "worse" need a relative difference
  above `P`% of the larger absolute value; smaller differences are ties. A command that is
  strictly dominated but not beyond the threshold stays on the frontier and is reported as
  `not dominated (within noise)` (`within_noise` in JSON). The default (no flag) compares the
  estimator values strictly. The estimator is the mean until #320 adds `--estimator`; the
  statistical overlap test of #323 is not used, only this relative threshold.
- **Objective:** the candidate with the best value of the objective metric (ties: the first
  command). `best` is `null` when no command is a candidate.
- **Commands:** results are compared in the order of the summary, after `--filter-failed`.
  Indices in JSON refer to `results[]`.
- **Terminal:** a section after the summary (`Command | metrics... | Status`), hidden by
  `--style none`. Status is `frontier`, `dominated by <commands>`, `not dominated (within
  noise)`, `excluded: ...`, with `, best objective` appended for the objective winner. The
  delta table is unchanged; the Status column carries the marker instead.
- **Markdown:** the same table under `### Pareto frontier over ...`, appended to the
  `--export-markdown` file. **HTML:** a `Pareto frontier` section after the summary with the
  frontier rows in bold.

## JSON contract (#240)
Additive, `schemaVersion` 1.16. The top-level object `pareto` (next to `optimize`, not
inside `perfratio`, which is written before the run) is present with `--pareto`,
`--constraint` or `--objective`:
```json
"pareto": {
  "metrics": [{ "metric": "wall", "direction": "lower_is_better" }],
  "constraints": ["memory <= 256MiB"],
  "noise_threshold": 0.05,
  "frontier": [0, 2],
  "dominated": [{ "command_index": 1, "by": [0] }],
  "within_noise": [],
  "excluded": [{ "command_index": 3, "reason": "constraint_violated", "detail": "memory <= 256MiB" }],
  "objective": { "direction": "lower_is_better", "metric": "energy", "best": 2, "value": 0.9 }
}
```
`constraints`, `noise_threshold`, `within_noise` and `objective` are omitted when empty or
unused. `value` is in base units (seconds, bytes, joules). Schema:
`docs/schema/perfratio-export.schema.json`.

## Tests
Unit tests in `src/pareto.rs`: a fixed five-implementation fixture (time, energy, memory),
direction, ties, noise, constraints, unavailable metrics, objective, parsing, 500 commands.
`tests/pareto_tests.rs` (CLI, exports, validation) and the `pareto` case of
`tests/json_schema_tests.rs`.
