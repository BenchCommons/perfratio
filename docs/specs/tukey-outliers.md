# Spec: Tukey's fences outlier detection with counts and percentages (#214)

## Problem / Motivation
References: #214, `andrewrk/poop#6`, `andrewrk/poop#16`.

The outlier warning is a single qualitative sentence based on the modified Z-score of the wall
time. It does not say whether 1 of 100 runs or 20 of 100 runs are affected, and it ignores
memory and energy. Tukey's fences (IQR) are the common non-parametric rule and are easy to
explain.

## Contract
- **Method.** `--outlier-method <iqr|modified-z>`, default `iqr`.
  - `iqr`: Q1 and Q3 by linear interpolation (numpy default, `stats::summary::percentile`),
    IQR = Q3 - Q1, fences `[Q1 - 1.5·IQR, Q3 + 1.5·IQR]`. A value is an outlier if it is
    strictly outside the fences.
  - `modified-z`: the existing rule, `|z| > OUTLIER_THRESHOLD`, reported as the equivalent
    interval `median ± OUTLIER_THRESHOLD · MAD`.
  - `--discard-outliers` is unchanged and always uses the modified Z-score.
  - The "slow initial run" and "inflated variance" warnings are unchanged.
- **Series.** Wall time, peak memory (bytes) and energy (Joules, only if measured for every
  run).
- **Edge cases.** Fewer than 4 runs, or any non-finite value: no statistics (`outliers` is
  omitted for time; the per-metric entry is omitted). IQR = 0: the fences collapse to the
  quartile and any different value is an outlier. Identical samples: 0 outliers.
- **Terminal.** For time, if count > 0:
  `Statistical outliers were detected: 3 of 50 runs (6.0%) are outside [11.2 ms … 13.6 ms] (iqr).`
  At 10% or more the text adds that the result is probably unreliable and suggests `--warmup`,
  `--prepare` or CPU isolation. For memory and energy a warning appears only at 10% or more.
  All of these respect `--suppress-outlier-warnings`.
- **JSON (additive, optional).** New field `outliers` on each result, omitted when absent:
  ```json
  "outliers": {
    "method": "iqr",
    "count": 3, "percent": 6.0,
    "lower_fence": 0.0112, "upper_fence": 0.0136,
    "memory_usage_byte": { "count": 0, "percent": 0.0, "lower_fence": 1.0e6, "upper_fence": 2.0e6 },
    "energy_joules": { "count": 0, "percent": 0.0, "lower_fence": 1.0, "upper_fence": 2.0 }
  }
  ```
  `count`, `percent`, `lower_fence`, `upper_fence` at the top level describe the wall time
  (seconds). `memory_usage_byte` and `energy_joules` are present only if measured.
  **Versioning:** purely additive optional field, so a minor bump of the output schema
  version (#240); consumers that ignore unknown fields are unaffected.
- **Not included.** CPU counters and custom metrics are not analysed yet; the nested layout
  allows adding them later.
