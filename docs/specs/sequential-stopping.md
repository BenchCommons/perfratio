# Specification: Sequentially valid stopping for `--target-precision`

**Issue:** #322. **Status:** shipped in 1.1. Extends `target-precision.md`.

## Problem / Motivation
`--target-precision PCT` checks the 95% Student-t interval of the mean after every run and stops
the first time its half-width is at most PCT of the mean. That interval is valid for one sample
size chosen in advance. Choosing the moment to stop from the data ("peeking") makes the real error
rate higher than 5%: the first interval that happens to be narrow is the one with a small sample
deviation, which is also the one most likely to be wrong.

## Coverage study
Seeded Monte Carlo (`src/stats/sequential_sim.rs`, seed 322). Each stream of run times has a known
mean. Both rules see the same stream and stop at the first run from `min runs` on at which the
relative half-width of their 95% interval is at most 10%, within 3000 runs. Two error rates:
**stop miss**, the share of streams whose interval at the stopping run misses the true mean (what a
user of `--target-precision` gets, nominal 5%), and **ever miss**, the share in which the interval
misses the true mean at any run from `min runs` to 3000 (the time-uniform error). 4000 streams per
row, so the standard error of a rate near 5% is 0.35 points. `n` is the mean stopping run count;
`cost` is `n(sequential) / n(fixed)`.

| distribution | min runs | fixed stop miss | fixed ever miss | fixed n | seq stop miss | seq ever miss | seq n | cost |
|---|--:|--:|--:|--:|--:|--:|--:|--:|
| normal (1, 0.3) | 3 | 8.3% | 59.8% | 34.2 | 1.7% | 9.6% | 80.9 | 2.37x |
| | 5 | 7.0% | 58.0% | 34.8 | 0.9% | 6.9% | 81.8 | 2.35x |
| | 10 | 6.2% | 54.9% | 35.2 | 0.6% | 5.3% | 82.0 | 2.33x |
| | 30 | 5.1% | 49.0% | 37.2 | 0.5% | 4.0% | 82.0 | 2.20x |
| log-normal (sigma 0.5) | 3 | 7.0% | 61.8% | 105.7 | 0.6% | 17.4% | 265.5 | 2.51x |
| | 5 | 6.3% | 59.6% | 106.3 | 0.3% | 13.7% | 266.4 | 2.51x |
| | 10 | 6.2% | 56.1% | 106.5 | 0.3% | 10.9% | 266.6 | 2.50x |
| | 30 | 6.2% | 49.6% | 106.5 | 0.3% | 6.8% | 266.6 | 2.50x |
| log-normal (sigma 1) | 3 | 7.1% | 72.1% | 616.4 | 0.5% | 36.7% | 1845.8 | 2.99x |
| | 10 | 7.0% | 64.0% | 617.5 | 0.4% | 25.4% | 1847.9 | 2.99x |
| | 30 | 7.0% | 55.2% | 617.5 | 0.4% | 16.7% | 1847.9 | 2.99x |
| bimodal (80% 1.0, 20% 2.0) | 3 | 48.1% | 76.5% | 26.7 | 37.6% | 52.6% | 67.7 | 2.53x |
| | 5 | 37.1% | 68.2% | 32.1 | 29.7% | 37.2% | 76.1 | 2.37x |
| | 10 | 17.9% | 59.0% | 40.9 | 12.8% | 17.6% | 93.3 | 2.28x |
| | 30 | 7.4% | 51.0% | 44.8 | 2.3% | 7.8% | 103.1 | 2.30x |
| autocorrelated (phi 0.5) | 3 | 31.8% | 98.1% | 30.2 | 11.8% | 68.7% | 76.7 | 2.54x |
| | 10 | 28.8% | 97.3% | 32.0 | 10.1% | 63.8% | 78.4 | 2.45x |
| | 30 | 26.1% | 95.8% | 36.5 | 9.8% | 58.2% | 78.7 | 2.15x |

(The `--ignored` test `coverage_study` prints the full grid, including min runs 5 for the last two
distributions.)

Findings:
- **The fixed rule undercovers.** At the stopping run its 95% interval misses the true mean in 6%
  to 8% of independent streams with the default `--min-runs` of 10 or fewer, and at some run of the
  sequence it misses in 49% to 72% of streams. Peeking costs more with fewer minimum runs.
- **Dependence and multimodality make it much worse.** Autocorrelated runs (phi 0.5) miss in 26%
  to 32% of streams at the stopping run, because the interval assumes independent runs. A fast
  and a slow mode (20% slow) misses in 18% to 48% when fewer than 30 runs may stop it, because a
  short prefix may not contain the slow mode at all.
- **The confidence sequence covers at the stopping run** for independent data: at most 1.7% miss
  (normal), 0.6% (log-normal), 2.3% (bimodal with 30 minimum runs). It is conservative at the
  stopping run, because it holds for every run count, not only the one it stopped at.
- **Its time-uniform error is about 5% for normal data** (4.0% to 9.6%) and above 5% for skewed
  data (log-normal 7% to 37%): with the sample standard deviation plugged in the guarantee is
  asymptotic, and the skew slows the approach. The stopping run is nevertheless covered.
- **It does not repair dependence or a missing mode**: autocorrelated runs still miss in about 10%
  and bimodal data with few minimum runs in 13% to 38%. No stopping rule fixes data that are not
  independent samples of one distribution; use `--check-system`, the drift and interference checks
  of `--quality`, and a larger `--min-runs`.
- **Cost:** 2.2x to 3.0x the runs of the fixed rule for the same target. The boundary at its tuned
  run count is 1.5x wider than the fixed interval, and the run count to reach a half-width grows
  with the square of the width. Tuning the boundary for 10, 30, 100 or 300 runs changes the
  normal-data cost between 2.3x and 2.6x; 100 was kept (`tuning_study`, ignored).

The fixed-rule error is material, so the sequential rule is implemented as an option. The default
stays `fixed`.

## CLI
```
--target-precision <PCT> --target-precision-method fixed|sequential    (default: fixed)
```
`--target-precision-method` needs `--target-precision`. `sequential` is defined for the mean: it
cannot be combined with `--estimator` other than `mean` or with `--subtract` (the uncertainty of the
subtracted baseline is not part of the sequence). The `Range` line says
`(±1.0% @95% confidence sequence, target 1%)`.

## The rule
After `n` runs with mean `m` and sample standard deviation `s`, at confidence `1 - alpha`
(`--confidence-level`, 0.95 by default):

```
half-width(n) = s * sqrt( 2 (n rho² + 1) / (n² rho²) * ln( sqrt(n rho² + 1) / alpha ) )
```
This is the normal-mixture confidence sequence of Robbins (1970), written as in Howard, Ramdas,
McAuliffe and Sekhon (2021) and, with `s` plugged in, as the asymptotic confidence
sequence of Waudby-Smith, Arbour, Sinha, Kennedy and Ramdas (2021). `rho²` is the solution `x` of
`ln((x + 1) / alpha²) = x` divided by `n0`, which makes the boundary narrowest at `n0` = 100 runs
(`stats::sequential::TUNED_AT_RUNS`). The runs stop when `half-width / m` is at most the target,
within `--min-runs`, `--max-runs` and `--max-benchmarking-time` as before. Stopping on the
relative width is allowed: the interval holds for every stopping time.

Assumptions:
1. The runs are independent draws from one distribution. Drift, thermal ramps, caching between
   runs and periodic interference break it, as they break the fixed interval (the study shows 10%
   misses for phi = 0.5).
2. The variance is finite. With heavy tails the sample standard deviation converges slowly and the
   guarantee is approximate for small `n`.
3. The guarantee is asymptotic in `n` because `s` is estimated. For a few runs, skewed data or
   several modes it is approximate (see the table).

## JSON (additive, `schemaVersion` MINOR)
`results[].precision` gains:
- `stopping_rule`: `fixed_ci` or `confidence_sequence`;
- `achieved_half_width`: the relative half-width of that rule's interval at the last run, the same
  value as `reached`. `confidence` is now the `--confidence-level` of the run (0.95 by default).

`met`, `reached` and `target` keep their meaning. `docs/schema/perfratio-export.schema.json`
describes them.

## Not included
Multi-metric stopping (time, memory and energy each reaching a precision) is a follow-up.

## Tests
- `src/stats/sequential.rs`: the boundary function (the equation for `rho²`, a hand-computed value,
  shrinking width, wider than the fixed interval, narrowest at the tuned run count, edge cases).
- `src/stats/sequential_sim.rs`: seeded, 400 streams per row in CI: stop miss of the sequential
  rule within alpha plus three standard errors for normal, log-normal and (30 minimum runs) bimodal
  data; time-uniform error within alpha for normal data; the fixed rule loses coverage with few
  minimum runs; reproducibility. The long study: `cargo test --release sequential_sim::coverage_study
  -- --ignored --nocapture` (`PERFRATIO_SIM_REPLICATES` sets the stream count).
- `tests/target_precision_tests.rs`: the exported rule and half-width, `fixed` equal to the
  default, the option's requirements, and more runs than the fixed rule on a noisy command.

## References
- Howard, Ramdas, McAuliffe, Sekhon, "Time-uniform, nonparametric, nonasymptotic confidence
  sequences", Annals of Statistics 49(2), 2021. https://arxiv.org/abs/1810.08240
- Waudby-Smith, Arbour, Sinha, Kennedy, Ramdas, "Time-uniform central limit theory and asymptotic
  confidence sequences", Annals of Statistics 52(6), 2024. https://arxiv.org/abs/2103.06476
- Robbins, "Statistical methods related to the law of the iterated logarithm", Annals of
  Mathematical Statistics 41(5), 1970.
- Kalibera and Jones, "Rigorous Benchmarking in Reasonable Time", ISMM 2013.
  https://kar.kent.ac.uk/33611/
