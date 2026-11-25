# Spec: Statistical significance indicators (#215)

## Problem / Motivation
References: #215, `andrewrk/poop#7`, `andrewrk/poop#18`.
A summary such as "1.01 ± 0.02 times faster" invites the reader to treat noise as a speedup,
and a small but real regression can be dismissed. The summary should say whether a
difference is distinguishable from noise.

## Contract

### Decision rule
For a benchmark compared with the reference (the fastest result, or `--reference`):
- `p` is the two-sided p-value of the existing bootstrap Welch test (`src/stats/deep.rs`,
  the test behind `--deep-stats` and `--compare`, including `--subtract` baseline variance).
- `Δ = mean / reference_mean − 1`.
- `significant_speedup`: `p < 0.05` and `Δ ≤ −1%`.
- `significant_regression`: `p < 0.05` and `Δ ≥ +1%`.
- `noise`: everything else (a significant but smaller than 1% change is noise).
- Not assessed (no marker, no JSON fields): the reference itself, timed-out results,
  results without per-run times, fewer than 3 runs, or an undefined test.
- Two constant samples with different means count as `p = 0` (as in `--compare`).

The 0.05 level and the 1% effect size are fixed constants (`src/stats/significance.rs`).
A configurable threshold is not part of this change.

### Terminal summary
A marker is appended to each non-reference line of both summary layouts (`--sort=mean-time`
and `--sort=command`):

| Class | Symbols (color output) | Plain text |
|---|---|---|
| significant speedup | `⚡` green | `[+]` |
| significant regression | `🔻` red | `[-]` |
| noise | `~` dim | `[~]` |

```text
    1.76 ± 0.65 times faster than sleep 0.02 (…)  🔻 significant regression (p < 0.001)
    1.04 ± 0.30 times faster than sleep 0.01 (…)  ~ indistinguishable from noise (p = 0.626)
    1.64 ± 0.30 times faster than sleep 0.02 (…)  [-] significant regression (p < 0.001)
```

Symbols are used only when colors are enabled. `NO_COLOR`, non-TTY stdout, dumb terminals,
`--style=basic|nocolor` use the plain text markers. `--style=none` prints no summary.
Mind that without `--reference` the reference is the fastest command, so the others
can only be `regression` or `noise`.

### `--compare`
Not changed: the comparison table already has its own significance column
(`~`, `faster`, `slower`, `REGRESSION`) and uses the same p-value.

### JSON export (versioned)
Each `results[]` entry other than the reference gains two optional fields:
- `"significance"`: `"significant_speedup"` | `"significant_regression"` | `"noise"`
- `"p_value"`: number

Omitted when not assessed (see above). This is an **additive** change: schema
`MINOR` bump (`1.0` → `1.1`) once `schemaVersion` from `output-schema-contract.md` (#240)
is implemented; consumers must ignore unknown keys. `master` does not emit `schemaVersion` yet.

## Tests
- Unit: `classify` for clear speedup, clear regression, small effect with tiny p, large effect
  with high p, boundary, NaN; label rendering; JSON names (`src/stats/significance.rs`).
- Integration (`tests/significance_tests.rs`): plain markers on a pipe and with `NO_COLOR`,
  speedup against `--reference`, JSON fields.
