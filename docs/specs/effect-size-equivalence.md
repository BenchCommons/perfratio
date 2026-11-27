# Spec: Effect size, noise threshold and equivalence margin (TOST) (#323)

**Status:** Implemented. JSON `schemaVersion` 1.24.

## Problem / Motivation
References: #323, #215, #61, #320, #321, `significance-indicators.md`; Schuirmann (1987);
criterion.rs noise threshold; benchstat.

A significant difference can be irrelevant (0.3% with 10 000 runs), and "not significant" does
not mean "equivalent". The markers of #215 answer "is there a difference", not "is it big
enough to matter" or "are the two within an acceptable margin".

## Contract

### CLI
| Option | Meaning | Default |
|---|---|---|
| `--noise-threshold <PCT>` | changes below PCT of the reference are `within_noise` whatever the p-value. Replaces the fixed 1% floor of the markers of #215. With `--pareto` it keeps its earlier meaning (dominance needs a difference above PCT) | off |
| `--equivalence-margin <PCT>` | TOST at `--significance-level`: `equivalent` when the change is shown to lie within ±PCT of the reference, `0 < PCT < 100` | off |

Values are percentages (`5` or `5%`); out-of-range values fail before any benchmark runs.
Nothing changes in the output without these options.

### Quantities (relative to the reference, for the active `--estimator`)
- `relative_change = estimate(other) / estimate(reference) - 1`.
- Mean: `ci_low` and `ci_high` are the Welch interval of the difference of the means at
  `--confidence-level`, divided by the reference mean (the uncertainty of the reference mean in
  the denominator and `--subtract` baseline variance are not propagated).
- TOST (mean): with margin `δ = PCT * mean(reference)`, `p = max(P(T > (d + δ)/se), P(T < (d - δ)/se))`
  with Welch's degrees of freedom; equivalent when `p < alpha`. It equals
  `statsmodels.stats.weightstats.ttost_ind(other, reference, -δ, δ, usevar='unequal')`. Two
  constant samples are equivalent iff `|d| < δ`.
- Other estimators: bootstrap interval of the difference of the estimates (`--seed`,
  `--bootstrap-resamples`); equivalent when the `1 - 2 alpha` interval lies inside `(-δ, δ)`,
  which is the TOST at `alpha`. There is no p-value.
- `standardized.cohens_d = (mean_other - mean_ref) / s_pooled`; `standardized.common_language
  = U / (n m)`, the probability that a run of `other` is slower than a run of the reference
  (ties count half), U the Mann-Whitney statistic.
- `minimum_detectable_effect = (z(1 - alpha/2) + z(0.8)) * se / mean(reference)`: the smallest
  change detected with 80% power at the current runs and variance (normal approximation).
- `runs_needed` (inconclusive with a margin, observed change inside the margin):
  `n = (s_r² + s_o²) (z(1 - alpha) + z(1 - (1 - 0.8)/2))² / (δ - |d|)²` runs per command
  (Chow, Shao and Wang). Not given when `|d| >= δ`.

### Classification
In this order, with `p` the p-value of the difference (adjusted if `--correction` is active)
and `floor = --noise-threshold` or, if unset, 1%:
1. `equivalent`: the TOST succeeds.
2. `within_noise`: `|change| < floor`, and either `--noise-threshold` is set or `p < alpha`.
3. `faster` / `slower`: `p < alpha`.
4. `inconclusive`: otherwise; neither a difference nor equivalence is shown, more runs are needed.

`significance` (#215) is unchanged except that `--noise-threshold` replaces the 1% floor.

### Terminal
The marker set of #215 gains (plain text for `NO_COLOR`, pipes and dumb terminals; the symbol
is used with colors):

| Class | Symbol | Plain text |
|---|---|---|
| equivalent | `≈` | `[=] equivalent within ±5% (+3.1%, TOST p = 0.032)` |
| inconclusive | `?` | `[?] inconclusive, more runs needed (…, about 120 runs per command would settle it)` |
| within noise | `~` | `[~] within noise (+0.4%, threshold 1%)` |

### `--compare`
`--fail-if-regressed` does not fail for a change classified `within_noise` or `equivalent`
(baseline = reference). The table verdict shows `equivalent`, `inconclusive` or `within noise`.

### JSON (additive, `schemaVersion` 1.23 -> 1.24)
`results[].effect`, `classification`, `equivalence_p_value` and
`perfratio.statistics.noise_threshold` / `equivalence_margin`, only with the options.
See `output-schema-contract.md`.

## Tests
- `src/stats/effect.rs`: TOST p-values for four margins against statsmodels 0.15.0
  `ttost_ind`; Welch interval, normal quantiles and the Mann-Whitney statistic against SciPy
  1.18.1; Cohen's d, power and minimum detectable effect against the formulas evaluated with
  SciPy's `norm.ppf`; classification rules; interval monotonic in the level.
- `tests/effect_tests.rs`: classification, run count, JSON, markers, the median, `--compare`.
- `tests/json_schema_tests.rs`: an export with both options validates.
