# Benchmark regression gate in CI

perfratio can compare a run with a baseline exported earlier and fail when a
benchmark got **significantly** slower:

```sh
perfratio --export-json baseline.json 'target/release/app input.txt'        # on main
perfratio --compare baseline.json --fail-if-regressed 5% \
       --export-diff-markdown diff.md 'target/release/app input.txt'     # on the PR
```

```
Comparison with baseline.json
  Command       Baseline        Current    Change  Significance
  app      28.2 ms ± 2.6  50.1 ms ± 2.7  +77.5% ▲  p < 0.001     REGRESSION
```

- Commands are matched by their displayed name (after parameter substitution, or
  the `-n` name). Unmatched commands are listed as `new` / `removed`.
- A benchmark regresses if it is slower than the baseline by more than the
  threshold **and** the difference is statistically significant (bootstrap Welch
  test on the per-run times, p < 0.05). Noise alone does not fail the gate. Both
  exports need per-run times (every perfratio JSON export has them), and at least 3
  runs each.
- Exit codes: `0` ok, `3` regression or failed budget, `5` measurement quality worse than `--fail-on-quality`. Any other non-zero code is an ordinary error.
- The exports and the comparison table are written before perfratio exits with 3.

## Per-metric budgets

`--budget` gates several metrics at once, each with its own limit (see
`docs/specs/budgets.md`). Relative budgets compare with the `--compare` baseline and fail only
if the difference is also significant; absolute budgets cap the value itself:

```sh
perfratio --warmup 3 --compare baseline.json \
       --budget 'wall=+5%' --budget 'memory=+10%' --budget 'instructions=+1%' \
       --budget 'memory<=256MiB' --on-unavailable fail \
       --export-diff-markdown diff.md -n app 'target/release/app input.txt'
```

Budgets checked into the repository (`perf-budgets.toml`):

```toml
budgets = ["wall=+5%", "memory=+10%", "memory<=256MiB"]
```

`--budget-file perf-budgets.toml` reads them. A metric that is missing in one of the runs
(for example `energy` or `instructions` on a runner without them) is `not evaluated`: with the
default `--on-unavailable warn` the gate prints a warning, with `fail` it fails, with `skip` it
is left out of the report. It is never counted as 0 or as a pass.

## GitHub Actions

```yaml
name: Benchmarks
on:
  pull_request:
jobs:
  bench:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      # Baseline: the target branch, built and measured on the same runner, so
      # that differences between runner machines don't count as regressions
      - run: |
          git worktree add ../base "origin/${{ github.base_ref }}"
          (cd ../base && cargo build --release)
          perfratio --warmup 3 --export-json baseline.json -n app '../base/target/release/app input.txt'

      - run: cargo build --release
      - run: |
          perfratio --warmup 3 --compare baseline.json --fail-if-regressed 5% \
                 --export-diff-markdown diff.md -n app 'target/release/app input.txt'
          # or, with several budgets from a checked-in file:
          #   perfratio --warmup 3 --compare baseline.json --budget-file perf-budgets.toml \
          #          --export-diff-markdown diff.md -n app 'target/release/app input.txt'

      - if: always()
        run: cat diff.md >> "$GITHUB_STEP_SUMMARY"
```

Measuring both versions **in the same job** matters: shared CI runners differ
from each other by far more than 5%. A baseline stored from an earlier run is
only meaningful on dedicated benchmark machines (see `--check-system=strict`).
The `-n app` name makes both runs match even though the command paths differ.
