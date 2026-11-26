# Specification: Workload validation (`--expect-output-sha256`, `--validate`)

**Issue:** [#316](https://github.com/BenchCommons/perfratio/issues/316)
**Status:** Completed. JSON `schemaVersion` 1.17.
**Related:** #240 (output contract), `omit-failed-runs.md`, `omit-failed-runs-alignment.md`,
catalog-algorithms#11 (what "correct" means is defined there; this is the generic hook).

## 1. Problem / Motivation

An implementation can look fast because it did less work: wrong output, dead-code elimination,
an early exit that still returns 0. perfratio treats a non-zero exit code as a failure, but it
could not tell whether the program produced the right result. For cross-language comparisons the
catalog defines correctness. perfratio only needs a generic way to check and record it.

## 2. Contract

### 2.1 Options

| Option | Meaning |
|:--|:--|
| `--expect-output-sha256 <HEX>` (alias `--expect-output-hash`) | The SHA-256 of the stdout of each run must equal `HEX` (64 hex digits, case-insensitive). |
| `--validate <CMD>` | `CMD` runs after each run through `sh -c` (`cmd.exe /C` on Windows). It gets the run's stdout on stdin and `PERFRATIO_RUN_ID` (the same number as `PERFRATIO_ITERATION`) in the environment. A non-zero exit code marks the run invalid. |
| `--validate-first-only` | Check only the first timed run, for long deterministic workloads. |
| `--on-invalid <fail\|omit\|keep>` | Abort (default), drop the run from the statistics, or keep it flagged. |

Both checks can be combined; a run is valid only if both pass. Numeric tolerance and structure
checks belong in the validator, not in perfratio.
`--validate-first-only` and `--on-invalid` need `--expect-output-sha256` or `--validate`.

### 2.2 Semantics

- **Invalid is not failed.** A run that exits non-zero keeps its exit-code semantics
  (`--ignore-failure`, `--omit-failed-runs` are unchanged) and is not validated. Validation applies to
  runs whose command exited 0. Warmup, setup, prepare, conclude and cleanup runs are never validated.
- **Outside the timed window.** Stdout is captured while the command runs (the same capture
  `--output-metric` uses). Hashing and the validator run after the run's time, memory and energy
  were taken, so they are in none of the measured series. The validator also runs before the next
  `--prepare`, outside every window.
- **`fail`:** the benchmark aborts with a message naming the iteration, the command, and the expected and
  actual hash (or the validator's exit code and the tail of its stderr).
- **`omit`:** invalid runs are removed from every per-run series and from the statistics with the
  same index alignment as `--omit-failed-runs` (`omitted-failed-runs-alignment`): `times`,
  `exit_codes` and the other series have equal length, and `--export-runs` shows the gap in the
  iteration numbers. Omitting can hide a failing implementation (survivorship bias), so the count is
  always printed (`N invalid runs omitted` in the runs line and a warning) and exported
  (`validation.runs_omitted`, `validation.omitted_runs`). If every run is invalid the benchmark fails.
- **`keep`:** the run stays in the statistics; a warning and `validation.runs_invalid` flag it.
  The exit status of perfratio does not change; use `--fail-on-quality` (#318) to gate on it.
- The delta table (`--delta-table`) and `--compare` print a warning line for a result with
  invalid runs.

### 2.3 Output policy

Stdout must be captured, so validation cannot be combined with `--output`, `--show-output`,
`--show-output-on-failure`, `--until` or `--concurrency` (rejected at startup). At most 16 MiB of
stdout per run can be validated; more fails the benchmark with a message
(`--validate-first-only` or a smaller workload avoids it). `--debug-mode` simulates stdout as the
words after the fifth field of `sleep <s> <user> <system> <memory> <words>` (empty by default), so
tests are deterministic without a shell.

### 2.4 JSON (`schemaVersion` 1.17, additive)

`results[].validation`, present only with the options above:

```json
"validation": {
  "mode": "output_sha256",
  "on_invalid": "omit",
  "first_only": false,
  "runs_checked": 5,
  "runs_invalid": 1,
  "runs_omitted": 1,
  "omitted_runs": [2],
  "output_sha256": "ba7816bf…"
}
```

`mode` is `output_sha256`, `command` or `output_sha256+command`. `output_sha256` is the hash of the
first checked run (useful to learn the value for `--expect-output-sha256`). `runs_checked` counts
the runs that were checked. Schema: `docs/schema/perfratio-export.schema.json`.

## 3. Implementation

- `src/validation.rs`: SHA-256 (FIPS 180-4, checked against the NIST vectors), `ValidationConfig`,
  `ValidationSummary`, the validator process.
- `src/benchmark/executor.rs`: keeps the stdout of a validated run in `TimingResult::stdout`.
- `src/benchmark/mod.rs`: `BenchmarkRunner::validate_run`, omit handling in `finalize`.

## 4. Tests

Unit: SHA-256 vectors, hash mismatch message, size limit, which iterations are checked, validator
stdin and `PERFRATIO_RUN_ID`. Integration (`tests/validation_tests.rs`): pass, wrong hash, a result that
flips mid-run with `fail`, `omit` and `keep`, first-only, a validator that sleeps (excluded from the
mean), failed commands not validated, the schema (`tests/json_schema_tests.rs`).
