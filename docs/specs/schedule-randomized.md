# Specification: Seeded randomized and balanced benchmark order

**Issue:** #324. **Status:** shipped in 1.1 (`schemaVersion` 1.25). Builds on `round-robin-schedule.md`.

## Problem / Motivation
`--schedule round-robin` removes the drift between commands, but its order is fixed
(A B C, A B C, ...). Command A always follows C and C always follows B, so cache, thermal and turbo
state left by one command is charged to the same successor in every round. Google Benchmark
(random interleaving) and pyperf (independent worker processes) break that with a random order.
A result is only reproducible if that order is seeded.

## CLI
```
--schedule randomized   interleaved; a fresh seeded shuffle of the commands in every round
--schedule balanced     interleaved; round r starts at command r mod N (cyclic Latin square)
--seed <N>              seeds the shuffle (the same option as for '--parameter-sample' and the bootstrap)
```
Defaults are unchanged (`grouped`). `--round-robin` and `--schedule round-robin|interleaved` keep
the fixed order.

- Both policies use the interleaved execution of round-robin, including its warmup and its
  restrictions on `--setup`/`--cleanup` (`--allow-setup-with-round-robin` lifts them) and the
  rejection with `--optimize`.
- A round runs every command once. The initial measurement of every command is round 0, the timed
  runs start at round 1. Warmup rounds use the same policy with their own random stream, so the
  warmup does not change the measured order.
- `randomized`: Fisher-Yates shuffle driven by SplitMix64, seeded from `(seed, stream, round)`.
  The generator is fixed in this repository, so a seed gives the same order on every platform and
  version of 1.x. Only `--seed` is needed to replay it.
- `balanced`: over any N consecutive rounds every command takes every position exactly once; over
  any number of rounds the position counts of two commands differ by at most one.
- With `--jobs` > 1 only the worker-pool policies apply (`saturate`, `exclusive`, `fair`; `worker-pool.md`):
  `randomized` and `balanced` are rejected there, so the dispatch order of the pool is not randomized
  and no `order` is exported for it.
- With a single command nothing is interleaved: `schedule` is exported, `order` is not.
- With `--target-precision` or `--duration`, a command that has enough runs drops out of its
  rounds; the others keep the order of the policy.

## Contract (JSON, additive, `schemaVersion` 1.24 to 1.25)
`perfratio.schedule`, written by every export. It is not part of `perfratio.execution`
(`worker-pool.md`): the 1.22 schema requires `jobs`, `schedule_policy` and `contended` there, so
a file with only the schedule would break consumers of 1.22 (checked by
`tests/json_schema_compat_tests.rs`).
- `mode`: `grouped`, `round-robin`, `randomized` or `balanced` (open enum).
- `seed`: unsigned 64-bit integer, only with `randomized`.
- `order`: only for the interleaved schedules. The `results[]` index of every measured run in
  execution order, so that an analysis can test for position and neighbour effects. Warmup runs are
  not listed. `results[]` includes imported results first, so the index is not the command-line
  position when `--import-json` is used.

`docs/schema/perfratio-export.schema.json` and the archived `docs/schema/1.25/` describe it.

## Tests
- `src/benchmark/order.rs`: pure unit tests for determinism, seed sensitivity, permutation, uniform
  first position, stream separation, equal counts per round, balanced position counts.
- `tests/schedule_order_tests.rs`: exported order for two seeds (`--debug-mode`), balanced
  rotation, round-robin and grouped.
