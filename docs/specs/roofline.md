# Spec: `--roofline` arithmetic intensity and placement (#379)

## Problem / Motivation
Users cannot tell whether a kernel is compute- or memory-bound. The roofline
model (Williams, Waterman, Patterson, CACM 2009) answers it from two numbers:
FLOPs and bytes moved.

## Contract

### CLI
```
--roofline                      place every command on the roofline (Linux, x86_64)
--roofline-peaks GFLOPS,GBPS    the roofs; or "observed" (default)
--roofline-precision single|double   element size of packed FP counts (default double)
```
`--roofline` enables `--simd` and the `cache-misses` counter (at the default
`--scope`); it conflicts with `--import-json`. The other two flags need `--roofline`.

### Inputs
* **FLOPs**: the `--simd` buckets (#211). One count of `scalar_fp` is 1 FLOP;
  a packed bucket of width W counts `W / element_bits` FLOPs per count (double:
  2, 4, 8; single: 4, 8, 16 for 128, 256, 512 bit). `--simd` cannot tell
  single and double apart, hence `--roofline-precision`. Intel counts an FMA
  twice, which is right. AMD counts uops, so an FMA uop is one FLOP: a lower
  bound. Integer SIMD is not counted.
* **Bytes**: `cache-misses` (last-level misses) times 64 B (`cache_line_bytes`).
  This is the documented fallback while the bandwidth counters of #378 are
  absent (`bytes_source: "llc_misses"`). It misses hardware prefetches and
  write-backs, so bytes are a lower bound and the arithmetic intensity an
  upper bound. When #378 lands, `bytes_source` gains a value; consumers must
  treat it as an open enum.
* **Time**: wall time of the run.
* A run counts only when FLOPs, bytes and a positive time are all known.

### Result per command
`arithmetic_intensity = Σ FLOPs / Σ bytes` and `gflops`, `gbytes_per_s` over
the runs used (totals, not a mean of ratios).

| `status` | Meaning |
|---|---|
| `ok` | intensity defined |
| `no_flops` | no floating-point arithmetic retired |
| `no_traffic` | no cache miss counted: the intensity is unbounded, no `bound` |
| `unavailable` | `reason`: `simd_unavailable`, `cache_misses_unavailable`, `no_run_times`, `no_complete_run` |

### Placement
With peaks `P` (GFLOP/s) and `B` (GB/s): ridge point `P / B`,
`attainable_gflops = min(P, intensity × B)`, `bound = memory` below the ridge
and `compute` at or above it, `efficiency = gflops / attainable_gflops`
(unclamped: it exceeds 1 when the peaks are too low or the misses undercount).

Peaks are `configured` (`--roofline-peaks 48,20`) or `observed`: the best
GFLOP/s and the best GB/s over the commands with status `ok`. Observed peaks
are lower bounds of the real ceilings and need at least two such commands;
otherwise `peaks` is absent and `peaks_note` says so (a single command would
sit on its own roof).

### JSON (MINOR, schemaVersion 1.39)
Top-level optional `roofline` object: `precision`, `bytes_source`,
`cache_line_bytes`, `peaks?` (`gflops`, `gbytes_per_s`, `source`),
`ridge_point?`, `peaks_note?`, `results[]` (`command`, `parameters?`,
`status`, `reason?`, `runs_used`, `flops_per_run?`, `bytes_per_run?`,
`arithmetic_intensity?`, `gflops?`, `gbytes_per_s?`, `bound?`,
`attainable_gflops?`, `efficiency?`). Undefined numbers are omitted, never
`null`, `NaN` or infinite. Schema: `roofline` in `docs/schema/perfratio-export.schema.json`.

### Terminal
```
Roofline (double precision; memory traffic = last-level cache misses × 64 B, a lower bound):
  ./k: AI 4.00 FLOP/B · 4.00 GFLOP/s · 1.00 GB/s · memory-bound, 10% of the attainable 40.0 GFLOP/s
  Peaks (configured): 100 GFLOP/s, 10.0 GB/s; ridge point 10.0 FLOP/B
```

## Tests
Synthetic numbers for intensity, ridge, both bounds, observed peaks and the
degenerate cases (`src/roofline.rs`); `--debug-mode` CLI and JSON (`tests/roofline_tests.rs`),
schema validation (`tests/json_schema_tests.rs`). Not run on hardware: the
real `--simd` and `cache-misses` readings and how closely LLC misses track
traffic (#337).

## References
Williams et al., CACM 2009; #211, #378, #337
