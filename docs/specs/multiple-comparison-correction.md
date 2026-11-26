# Spec: Multiple-comparison correction (#321)

**Status:** Implemented. JSON `schemaVersion` 1.22.

## Problem / Motivation
References: #321, #215, #240; Holm (1979); Benjamini and Hochberg (1995); benchstat.

The significance markers of #215 run one test per command. At alpha = 0.05, a scan of 100
commands yields about 5 "significant" differences by chance. `--correction` adjusts the
p-values of the family so that the markers keep their meaning.

## Contract

### CLI
| Option | Meaning | Default |
|---|---|---|
| `--correction <none\|bonferroni\|holm\|bh>` | adjustment of the p-values behind the markers | `none` (output unchanged) |
| `--comparison-family <reference\|all-pairs>` | `reference`: every command against the reference (family size = number of assessed commands). `all-pairs`: every pair of commands; the marker of a command still compares it with the reference but uses the p-value adjusted over all pairs. Needs `--correction` | `reference` |

- `bonferroni`: `min(1, m p)`. `holm`: step-down, controls the family-wise error rate (the
  chance of any false positive), never less powerful than Bonferroni. `bh`: Benjamini-Hochberg
  step-up, controls the false discovery rate (the expected share of false positives among the
  reported differences); more powerful, appropriate for large scans where a few false
  positives are acceptable.
- Comparisons without a p-value (timed out, fewer than 3 runs, no per-run times) are not part
  of the family and do not enlarge `m`.
- A marker uses the adjusted p-value against `--significance-level`; the 1% minimum effect
  of #215 still applies.
- Terminal: a line `Multiple-comparison correction: holm over 49 comparisons (reference
  family), alpha = 0.05` under the summary title, and each marker shows
  `(p = 0.012, adjusted p = 0.048)`.
- The family is the set of commands of the current invocation, imported results included.
  Results merged from several files were not measured together, so a family built from them
  carries that caveat. The `--compare` table has its own column and is not corrected.
- Without `--reference` the reference is the fastest command, selected from the same data it is
  tested on, so the comparisons are biased towards small p-values even for equal commands;
  fix the reference for a clean false-positive rate.

### JSON (additive, `schemaVersion` 1.21 -> 1.22)
- `perfratio.statistics.correction = {method, family, family_size, alpha}`, only with
  `--correction` other than `none`.
- `results[].p_value_adjusted`, only with a correction; `significance` is decided on it.
- Without `--correction` nothing changes except the version.

## Tests
- `src/stats/correction.rs`: Bonferroni, Holm and BH against `statsmodels.multipletests`
  (statsmodels 0.15.0, SciPy 1.18.1) for a 4-, a 5- (p = 0, ties) and a 25-vector, a single
  comparison, NaN, monotonicity and ordering.
- `tests/correction_tests.rs`: 50 commands from one distribution (seeded generator): false
  positives without correction, none with `holm`, `bonferroni` and `bh`; markers and JSON agree;
  a real shift survives; family sizes; default output unchanged.
- `tests/json_schema_tests.rs`: an export with `--correction` and `--comparison-family` validates.
