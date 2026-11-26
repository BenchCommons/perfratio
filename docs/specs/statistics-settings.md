# Spec: Selectable estimator, confidence and significance levels, bootstrap settings (#320)

**Status:** Implemented. JSON `schemaVersion` 1.21.

## Problem / Motivation
References: #320, #82, #50, #215, #240; megafine `--estimator`, benchstat, criterion.rs
(`confidence_level`, `significance_level`, `nresamples`).

Rankings, ratios and tests were based on the mean, and the 95% level and the 5000 bootstrap
resamples were constants. For skewed timings (GC pauses, page-cache effects, noisy neighbors) the
median or a percentile is often the better central value, and CI users need the other levels.

## Contract

### CLI
| Option | Meaning | Default |
|---|---|---|
| `--estimator <mean\|median\|pNN>` | central value for ranking, relative speed, significance, the delta table, `--compare`, `--raw-ratios` and `--target-precision`. `pNN` is a percentile, `0 < NN < 100`, fractions allowed (`p99.9`) | `mean` |
| `--confidence-level <L>` | level of intervals, `0 < L < 1` | `0.95` |
| `--significance-level <A>` | alpha of tests and significance markers, `0 < A < 1` | `0.05` |
| `--bootstrap-resamples <N>` | resamples of bootstrap intervals and tests, `N >= 100` | `5000` |
| `--seed <N>` (alias `--bootstrap-seed`) | seed of the resampling. The same option seeds `--parameter-sample` | fixed |

Out-of-range values fail before any benchmark runs, naming the option.

### What the estimator changes
- **Ranking and relative speed** use the estimator of the metric's per-run samples. A result
  without per-run samples (imported JSON without `times`) has no median or percentile: it is
  treated as having no data for the metric (a stored wall-time median is used for `median`).
  It is never replaced by its mean.
- **Ratio uncertainty.** For the mean, the existing `± σ` (error propagation) stays. For other
  estimators the ratio has no such formula: the summary shows a bootstrap interval
  `[low, high]` at the confidence level, and the JSON has `relative_speed_ci`. The
  command-ordered summary layout (`--sort=command`) shows no uncertainty for them.
- **Significance (#215) and `--compare`.** The change is `estimator(other) / estimator(reference) - 1`.
  The p-value is Welch's bootstrap test for the mean, and for other estimators a bootstrap test of
  the difference of the estimates (null distribution from resampling both groups out of the pooled
  sample). The `--subtract` baseline variance only enters the mean's test. The minimum effect of
  1% stays fixed.
- **Delta table.** Shows the estimator in the column headers. For the mean, Welch's t-test and
  a Student t interval at the confidence level; otherwise the bootstrap test and the bootstrap
  interval of the difference. Dimensions without per-run samples are omitted for a median or
  percentile.
- **`--target-precision`.** The mean keeps Student's t interval, at `--confidence-level`. A median or
  percentile uses a bootstrap interval of the estimator (cost: `--bootstrap-resamples` resamples
  after each run, so lower it for very fast commands).
- **`--deep-stats`** reports its intervals at the confidence level and prints the level; the
  significance wording uses the stricter of 1% and the chosen alpha.
- **Measurement quality (#318)** reports its precision check for the estimator and level.

### When to use which estimator
The mean is the default and the right choice when the cost of the total matters. Prefer the
median when a few slow runs (GC, a cold cache) should not decide the ranking, and a percentile
(`p95`) for tail latency. Percentiles need many runs: with fewer than about 20 runs `p95` is
the largest or second-largest value and its bootstrap interval is wide. The median of few runs
is noisy but unbiased. The quality report flags small samples.

### JSON (additive, `schemaVersion` 1.20 → 1.21)
- `perfratio.statistics = { estimator, confidence_level, significance_level,
  bootstrap_resamples, bootstrap_seed }`, always present.
- `results[].relative_speed_ci = [low, high]` for estimators other than the mean.
- `relative_speed`, `significance` and `p_value` were computed with `statistics.estimator`.
- With the defaults all other numbers are identical to 1.18.

## Tests
- `src/stats/dist.rs`: Student t quantile and CDF against SciPy 1.18.1.
- `src/stats/settings.rs`: parsing, validation, estimates against numpy.
- `src/stats/precision.rs`: the interval at 90% and 99% against SciPy.
- `src/stats/deep.rs`: interval monotonic in the level, seed determinism.
- `tests/estimator_tests.rs`: a heavy-tailed fixture that the mean and the median rank
  differently, default parity, validation, JSON.
