# Spec: perf-stat parity harness and child-tree accounting for `--counters` (#312)

Part of the PMU validation of #49 and the bare-metal run of #337. No change to the JSON export.

## Problem / Motivation
`--counters` opens `perf_event_open` counters with `inherit` and `enable_on_exec`, so the
benchmarked process tree is counted and perfratio's own fork and exec work is not
(`hardware-counters.md`). Nothing proved that this stays true. It breaks silently when counters
are opened after exec, when children are created before the counters are enabled, with
daemonizing commands, short-lived children, multiplexing and PID reuse. poop (andrewrk/poop)
uses the same event setup and treats `perf stat` as the reference.

## Contract
`scripts/perf_parity.py` (Python 3 standard library) and the ignored Linux test
`tests/perf_parity_tests.rs::counters_match_perf_stat_and_count_the_child_tree`.

For each workload, `perf stat -x, -e instructions:u,cycles:u -- CMD` is run N times and
`perfratio -N --runs=N --warmup=1 --counters=instructions:u,cycles:u CMD` once. The medians
(`perf stat` over its runs, `counters_summary.<event>.median` for perfratio) are compared. User
scope (`:u`) on both sides keeps kernel and hypervisor activity out of the comparison.

| Workload | Command | Check |
|---|---|---|
| `loop` | `sh -c` shell loop of 300000 iterations (about 10^8 instructions) | perfratio within 5% of `perf stat` |
| `tree` | `sh -c` that starts K (default 4) background `sh -c LOOP` and waits | within 8% of `perf stat`, and perfratio within 8% of K x the `perf stat` count of `loop` (children counted via `inherit`) |
| `pipeline` | `seq 1 300000 \| tr 1 2 \| wc -l` | within 10% |
| `true` | `true` | perfratio within 500000 instructions of `perf stat` (the runner adds nothing) |
| `daemon` | a command that starts a background loop and exits | informational only: what is counted depends on timing |

A workload with fewer than 5 million instructions makes the check void and fails it. The
tolerances are a first estimate: adjust them from the first bare-metal run and record them here
(#337).

Exit status: `0` all checks passed, `1` a check failed, `77` skipped, `2` usage error. The
harness skips (never passes) when the host is not Linux, `perf` is missing, `perf stat` cannot
count `instructions:u` and `cycles:u` (VM without a PMU, `kernel.perf_event_paranoid` too high,
container), or the CPU is hybrid and `--pin <cpu>` was not given. `--pin` runs both tools under
`taskset -c <cpu>`. If `perf stat` works but perfratio reports counters as not `measured`, the
harness fails.

## How to run
```sh
cargo build --release
python3 scripts/perf_parity.py --perfratio target/release/perfratio --runs 10 --json parity.json
# or, through the test harness (fails on a skip unless PERFRATIO_PARITY_ALLOW_SKIP=1):
cargo test --locked --test perf_parity_tests -- --ignored --nocapture
```
`.github/workflows/perf-parity.yml` runs the test on demand (`workflow_dispatch`) on a
self-hosted runner labelled `self-hosted, linux, pmu`; it never runs on hosted CI. The logic
(parsing of `perf stat -x,`, tolerances, tree arithmetic) is covered everywhere by
`the_harness_logic_is_correct` and `perf_parity.py --self-test`.

## Known limitations
- Hybrid P/E-core CPUs: events map per core type, so both tools must run on one core (`--pin`).
- Multiplexing: with more events than counters `perf stat` scales and so does perfratio, but the
  results differ by the scaling error. The harness warns when `perf stat` counted an event for
  less than 99% of the run. Use an idle machine.
- VMs without a PMU and containers that block `perf_event_open`: skipped.
- Shell loops are deterministic to a few percent, not exactly. The tolerances absorb this; the
  harness does not prove exact equality.
- The daemonizing child is only reported.
- Results of a first bare-metal run are not recorded yet (#337).

## References
#312, #49, #218, #219, #337, `hardware-counters.md`; `perf-stat(1)`, `perf_event_open(2)`;
poop (`src/main.zig`).
