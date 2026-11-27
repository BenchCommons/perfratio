# Spec: Statistical power, minimum detectable effect and runs needed (#367)

**Status:** Implemented. JSON `schemaVersion` 1.37.

## Problem / Motivation
A non-significant comparison can mean "no difference" or "too few runs". Cohen (1988) and
#323 (`effect-size-equivalence.md`) give the vocabulary; this spec adds it per command:
what the runs made can detect, and how many runs a given effect needs.

## Contract

### CLI
| Option | Meaning | Default |
|---|---|---|
| `--power <P>` | probability of detecting the effect, `0 < P < 1` | 0.8 |
| `--target-effect <PCT>` | smallest change of the mean to detect, as a percentage of the mean (`3` or `3%`) | off |

Either option adds a `Power:` line and the `results[].power` object. The level of the test is
`--significance-level`. Out-of-range values fail before any benchmark runs.

`--target-effect` is also a stopping rule when neither `--runs` nor `--duration` is given: a
command runs until `runs >= runs needed`, but never before `--min-runs` or in the middle of an
`--aggregate-parameter-runs` cycle, and never after `--max-runs` or `--max-benchmarking-time`.
It combines with `--target-precision`: the runs stop when both are met. With `--runs` or
`--duration` it only reports (planning aid).

### Model
The one of `statsmodels.stats.power.TTestIndPower` with `ratio = 1`: this command is compared
with an identical one (same standard deviation `s`, same number of runs `n`) by a two-sided
equal-variance t-test at level `alpha`. For a true difference of `d = Δ / s`:

`power = 1 - F(t) + F(-t)`, `F` the noncentral t CDF with `2n - 2` degrees of freedom and
noncentrality `d sqrt(n/2)`, `t` the central t quantile at `1 - alpha/2`.

- `minimum_detectable_effect = d_min s / mean`: the `d` that gives the requested power at the
  runs made (relative to the mean, 0.05 = 5%).
- `runs_needed`: the smallest whole `n` whose power at `d = target_effect * mean / s` reaches the
  requested power (at least 2). `met` is `runs >= runs_needed`.

`mean` and `s` are those of the measured wall-clock times (without an excluded first run). A
constant sample (`s = 0`) detects any change: the minimum detectable effect is 0 and 2 runs are
needed.

### Assumptions
Independent runs; approximately normal means (large `n` or light tails); the same variance and
run count in the compared command. Heavy tails, drift and autocorrelation (see `--quality`)
break the model, as they break the t-test itself. The runs needed use the standard deviation
seen so far, which is itself an estimate: it is noisy with few runs, and stopping on it is a
data-dependent rule (see `sequential-stopping.md` for what that does to an interval).

### JSON (`results[].power`, new in 1.37, MINOR)
`significance_level`, `power`, `runs`, `minimum_detectable_effect` (number or `null` without 2
runs and a positive mean), `stopping_rule` (bool); with `--target-effect`: `target_effect`,
`runs_needed`, `met`. `effect.minimum_detectable_effect` of #323 is a different quantity (the
two-sample comparison of two measured commands, normal approximation).

## Implementation
`src/stats/power.rs`: noncentral t CDF by Simpson integration over the chi distribution
(about 1e-10), `power`, `solve_runs` / `runs_needed` (bracket from the normal approximation, then
bisection), `minimum_detectable_d`, and `enough_runs`, the stop check (a run count below the
normal approximation fails without evaluating the power). `BenchmarkRunner::needs_more_runs`
calls it.

## Tests
- Unit: runs needed (15 cases) and minimum detectable effect (15) and power (5) against
  statsmodels 0.15.0 (`solve_runs` to 1e-6 relative; the minimum detectable effect to 1e-5,
  statsmodels' own root finder stops at a power error of 1e-6, see the test); normal CDF against
  SciPy; seeded simulation of the stopping rule (mean stopping run near the planned 64 runs).
- Integration (`tests/power_tests.rs`, `--debug-mode`): JSON object, report only with `--runs`,
  stopping at `--min-runs`, the `Power:` line, invalid values. The JSON schema test has a case.
