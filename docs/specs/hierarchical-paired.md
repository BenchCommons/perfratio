# Spec: Hierarchical and paired experiments (#368)

**Status:** Implemented. JSON `schemaVersion` 1.37.

## Problem / Motivation
References: #368, #322, #324, #306, #53, #310; Kalibera and Jones, "Rigorous benchmarking in
reasonable time", ISMM 2013.

Repeated measurements that share a process, a worker or a batch are not independent
observations. Runs on one `--jobs` worker share its CPUs and caches; the instances of one
`--concurrency` batch contend with each other; the repetitions of a region inside one process
share its warm state. A confidence interval that treats every measurement as independent is too
narrow, and the more measurements are taken inside each cluster the more it misleads: they
shrink the interval without reducing the between-cluster variance. Kalibera and Jones describe
the remedy: model the levels of the experiment, estimate the variance at each level and resample
at each level.

A comparison of commands also gains from pairing. In an interleaved schedule (#324) round `i`
runs every command once, so the commands of one round share the drift of the machine (thermal
state, frequency, a noisy neighbour). The differences within a round are less variable than the
raw times.

## Contract

### CLI
| Option | Meaning | Default |
|---|---|---|
| `--ci-method <flat\|hierarchical>` | which interval of the mean is the headline one: `flat` treats every run as independent, `hierarchical` is the two-level bootstrap below | `flat` |
| `--paired` | paired analysis of every command against the reference. Needs `--schedule round-robin`, `randomized` or `balanced`; any other schedule fails before a benchmark runs | off |

The defaults change no existing number. `--ci-method` selects the interval printed in the
summary of a run and `results[].hierarchy.mean_ci.method`; it does not change
`--target-precision`, the significance tests or the delta table, which stay flat. A plain
sequence of runs has no hierarchy: there `--ci-method hierarchical` prints nothing and the JSON
has no `hierarchy`.

### The sample hierarchy
Levels, outermost first: `worker` (`--jobs`), `run` (one process each), `instance`
(`--concurrency`), `repetition` (start/stop pairs of a `--region` inside a run). `results[].hierarchy`
lists the levels present and records the structure:
- `workers`: the worker slot of every run, aligned with `times` (`null` for the initial
  measurement, which runs on the main thread).
- `repetitions`: for every region that has more than one pair in some run, the pairs per run
  (the same numbers as `regions[].count_per_run`). The region time of a run is the sum over its
  pairs, so the variance of the repetitions inside a run is not observable and no variance
  is estimated for that level.
- The analysis clusters: with `--concurrency` the runs (their instances are the units), otherwise
  with `--jobs` the workers (their runs are the units). With both, the runs are the clusters and
  the worker only appears in `workers`. `cluster_level`, `clusters` and `units` give their number.

### Variance per level (`hierarchy.variance`)
The one-way random effects estimators on the wall times, for clusters of unequal size with
effective size `n0 = (N - Σ n_i² / N) / (g - 1)`:
`within = MSW`, `between = max(0, (MSB - MSW) / n0)`, `icc = between / (between + within)`,
`design_effect = 1 + (N / g - 1) * icc` and `effective_n = N / design_effect`. Variances are in
seconds squared. Omitted with fewer than two clusters. A constant sample (variance below
floating-point noise) has `icc = 0`.

### Intervals (`hierarchy.mean_ci`)
Both are written, at `--confidence-level`; `method` and `lower`/`upper` are the one selected by
`--ci-method`.
- `flat`: Student's t over all units.
- `hierarchical`: a percentile bootstrap that draws the clusters with replacement and then the
  units inside every drawn cluster with replacement (`--bootstrap-resamples`, `--seed`; a fixed
  SplitMix64 stream, so a seed gives the same interval on every platform). The distribution is
  widened around the sample mean by `t(g - 1) / z`, the ratio of the Student and the normal
  quantile at the level: the percentile interval of a mean of `g` clusters has the width of a
  normal interval, too narrow for few clusters (a factor 1.12 for 12 clusters).

### Paired analysis (`results[].paired`)
Round `i` of the reference and of a command form a pair (index `i` of `times`; the initial
measurement is round 0). For `d_i = command_i - reference_i` over the common rounds:
`mean_difference = mean(d)`, `difference_ci` Student's t at `--confidence-level` with `n - 1`
degrees of freedom, `p_value` the two-sided paired t-test, `relative_change` and its interval
the same divided by `mean(reference)`. `correlation` is the Pearson correlation of the two
commands across the rounds and `variance_reduction = 1 - var(d) / (var(a) + var(b))`, the share
of the unpaired variance that pairing removed. The analysis is absent for the reference, with
fewer than three common rounds, and when a run was omitted (`--omit-failed-runs`: the rounds no
longer line up). The significance markers and `p_value` of the summary are unchanged: they stay
Welch's test; the paired result is a separate report ("Paired by round" lines after the summary).

### JSON (additive, `schemaVersion` 1.36 to 1.37)
`results[].hierarchy`, `results[].paired`, `perfratio.statistics.ci_method` (always) and
`perfratio.statistics.paired` (only `true`). `docs/schema/perfratio-export.schema.json` and
`docs/schema/1.37/` describe them. The issue named `statistics.method`; `method` of
`perfratio.statistics.correction` already exists, so the new key is `ci_method`.

## Tests
- `src/stats/hierarchical.rs`: variance components against hand computation, Student interval,
  deterministic bootstrap, paired test against the closed form of the t(4) distribution,
  and a seeded Monte Carlo: 500 samples of nested data (between and within standard deviation 1,
  true mean 0) for 6x10, 12x10 and 30x5 clusters x units. The flat interval covers the true mean
  in 55-72% of the samples, the hierarchical one in 91-98% (nominal 95%); with no nesting both
  are near nominal.
- `tests/hierarchical_tests.rs`: the hierarchy of `--jobs` and `--concurrency` (`--debug-mode`),
  `--paired` end to end and its validation, JSON schema validity.
