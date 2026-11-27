# Spec: Machine-readable verdicts (`--verdict-json`) and an MCP server for agents (#386)

**Issue:** #386. **Status:** `--verdict-json` and the MCP server (`mcp-server.md`) are implemented. Parent: #365. Related: #325 (budgets), #318,
#323 (effect size and noise threshold). Extends `compare-baseline.md` and `budgets.md`.

## Problem / Motivation
An agent that optimizes code runs a benchmark, compares it with a baseline and must decide:
keep the change, revert it, or measure more. Today it has to parse terminal tables or the large
measurement export and redo the statistics. It needs one small document with an answer per metric
(improved, regressed, unchanged, inconclusive), the size of the change with its uncertainty, a
trade-off call when one metric got better and another worse, and the result of the budgets.

## Contract

### CLI
```
--verdict-json <FILE>     requires --compare; writes the verdict document
```
The file is written after the measurement and the budgets are evaluated, also when a budget
fails (exit code 3 is unchanged) and also with `--style none`. Nothing is added to the terminal.
It does not change the measurement export or its `schemaVersion`.

### Document (`verdictVersion` 1.0)
`docs/schema/verdict.schema.json` is the schema; `tests/verdict_tests.rs` validates real output
against a strict copy of it. `verdictVersion` is the first key. Its own versioning follows the
export's rules (`output-schema-contract.md`): an added key or value of an open enum is MINOR,
removing or changing the meaning of one is MAJOR; consumers ignore unknown keys.

```jsonc
{
  "verdictVersion": "1.0",
  "tool": { "name": "perfratio", "version": "1.0.0" },
  "baseline": "baseline.json",
  "settings": { "estimator": "mean", "confidence_level": 0.95, "significance_level": 0.05,
                "noise_threshold": 0.01, "equivalence_margin": 0.05 /* optional */ },
  "verdict": "trade_off",
  "summary": { "improved": 0, "regressed": 0, "trade_off": 1, "unchanged": 1,
               "inconclusive": 0, "not_evaluated": 0 },
  "gate": { "verdict": "fail", "failures": 1, "on_unavailable": "warn",
            "other_budgets": [] },                      // only with budgets
  "commands": [
    { "command": "app", "verdict": "trade_off",
      "metrics": [
        { "metric": "wall", "direction": "lower_is_better", "assessment": "improved",
          "baseline": 0.1, "current": 0.05, "change": -0.5, "ci_low": -0.52, "ci_high": -0.48,
          "p_value": 0.0001, "runs": { "baseline": 5, "current": 5 } },
        { "metric": "memory", "assessment": "regressed", "...": "..." } ],
      "budgets": [ /* entries of perfratio.compare.budgets[] for this command */ ] } ],
  "new": [], "removed": []
}
```

**Metrics.** For every command present in the baseline and the current run: `wall`, `cpu`,
`memory`, `energy`, `instructions`, `cycles`, and each custom metric present on both sides.
A metric with no data on either side is omitted. With data on one side only it is listed as
`not_evaluated` with `reason` `unavailable_in_baseline` or `unavailable_in_current`; a value is
never read as 0. Other `not_evaluated` reasons: `baseline_not_positive`, `not_finite`.

**Assessment** of one metric (open enum), with the estimator, significance level and noise
threshold of the run (`--estimator`, `--significance-level`, `--noise-threshold`, default floor 1%,
the same floor as `--compare`'s classification, #323):
1. `equivalent`: with `--equivalence-margin` and the mean, the TOST p-value is below the
   significance level.
2. `unchanged`: `|change|` is below the floor and either `--noise-threshold` was given, or the
   difference is significant, or the whole confidence interval of `change` lies inside the floor.
3. `improved` or `regressed`: the difference is significant (Welch/bootstrap test of `--compare`,
   p < significance level) and `|change|` is at least the floor. The sign is read with the
   `direction` of the metric (`lower_is_better` or `higher_is_better`, the latter for custom metrics
   declared `higher`).
4. `inconclusive`: otherwise. `reason`: `too_few_runs` (fewer than 3 runs on a side), `no_samples`
   (no per-run values, e.g. an imported baseline without them) or `not_significant`. More runs
   or a lower-variance setup is the remedy, not a conclusion that nothing changed.

`change` is `current / baseline - 1` of the estimator. `ci_low` and `ci_high` are the interval of
`change` at `confidence_level` (Welch for the mean, bootstrap otherwise), the uncertainty an agent
should look at before acting. `p_value` of `wall` is the one of `--compare` (it accounts for
`--subtract`).

**Verdict of a command** (and `verdict` of the document, over all metrics of all commands):
`trade_off` if some metric `improved` and some `regressed`; else `improved` if some improved;
else `regressed` if some regressed; else `inconclusive` if some is inconclusive; else `unchanged`
(`equivalent` counts as unchanged); `not_evaluated` if nothing could be assessed. An improvement
and an `inconclusive` metric together read `improved`: the verdict is what is established, and the
inconclusive metric stays visible in `metrics`. With several commands, `trade_off` also covers one
command improving and another regressing; read `commands[]` to tell them apart.

**Gate.** `gate` is the result of `--budget`, `--budget-file` and `--fail-if-regressed`, i.e.
what decides exit code 3: `verdict` (`pass`, `fail`, `not_evaluated`), the number of `failures` and
`on_unavailable`. The budgets of each command are repeated under `commands[].budgets`; budgets of
commands that are not in the comparison (absolute budgets of a new command) are in
`gate.other_budgets`. The verdict says what changed, the gate says whether the policy accepts it;
they are independent (a regression within a 10% budget is `regressed` with a passing gate).

## Edge cases
- A new command (no baseline) is in `new`, a missing one in `removed`; neither has a verdict.
- No common command: `verdict` is `not_evaluated`, `commands` is empty.
- `--import-json` results and a baseline without per-run samples: `p_value` is absent and the
  assessment `inconclusive` (`no_samples`).
- Several `--compare`d runs of one command name use the first match, as `--compare` does.

## Test plan (implemented)
`tests/verdict_tests.rs` on `--compare` fixtures made with `--debug-mode`: unchanged, improved
(change, interval, p-value), regressed, trade-off (faster, more memory), a failing budget
(exit code 3, file still written, budgets per command), a passing gate, budgets of an added
command, too few runs, a metric missing in the baseline (`--energy` only in the current run),
`--noise-threshold`, `--equivalence-margin`, `--verdict-json` without `--compare`, first key, and
validation against `docs/schema/verdict.schema.json` (strict copy).

## MCP server
The server is specified in `mcp-server.md` (binary `perfratio-mcp`, tools `capabilities`,
`run_benchmark` and `compare`, which returns this document). It answers the open questions of the
previous version of this section: scope and trust model, no new dependency, second binary of this
crate, protocol revision 2026-07-28 with the 2025-11-25 handshake.
