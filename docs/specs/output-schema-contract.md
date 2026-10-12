# Spec: Versioned JSON Output Schema Contract (`schemaVersion`) (#240)

## Problem / Motivation
References: #240, #225, #48, #59, #92, #93, #212, #216, `BenchCommons/catalog-algorithms#11`,
`BenchCommons/.product` `CONTRACTS.md` ("Measurement output: perfratio JSON schema, versioned").

`BenchCommons/catalog-algorithms` will compare implementations of the same algorithm in
hundreds of languages. `benchmark-tool` runs perfratio inside each implementation's
container and reads the `--export-json` file. perfratio **owns** this measurement output
contract: benchmark-tool and the catalogs consume it and never redefine it.

The current export is not yet a contract:
1. **No version.** Nothing in the file tells a consumer which shape it has. A consumer pinned
   to one perfratio release cannot tell an additive change from a breaking one.
2. **Duplicate metadata key.** The run metadata is written twice, under `"perfratio"` and
   under the legacy `"joulex"` alias (`src/export/json.rs`, `PerfratioSummary::joulex`).
   #225 tracks its removal.
3. **Unavailable is silent.** When `--energy` is set but RAPL cannot be read (non-Linux,
   containers without `/sys/class/powercap`, root-only `energy_uj`), the energy fields are
   simply omitted (`skip_serializing_if = "Option::is_none"`). A consumer cannot tell
   "not requested" from "requested but unavailable" from "partially measured".
4. **No documented guarantees.** Which fields are always present, which are optional, and
   which units they use is only discoverable by reading `BenchmarkResult`.

## Current Shape (as of `master`, 2b8be84)
Serializer: `src/export/json.rs` (`PerfratioSummary`, `ResultEntry`). Metadata:
`src/export/metadata.rs` (`ExportMetadata`, `SystemInfo`), always attached by
`ExportManager::from_cli_arguments` (`src/export/mod.rs`). Per-result fields:
`src/benchmark/benchmark_result.rs` (`BenchmarkResult`, flattened into each entry).

```jsonc
{
  "perfratio": { /* ExportMetadata */ },
  "joulex":    { /* identical copy, legacy alias (#225) */ },
  "interrupted": true,          // only when Ctrl-C stopped the run
  "results": [ { /* BenchmarkResult + relative_speed[_stddev] */ } ]
}
```

`ExportMetadata`: `version` (crate version), `command_line` (argv, program reduced to its
file name), `started_at` (RFC 3339 UTC, second precision), `labels` (from `--label KEY=VALUE`,
omitted if empty), `system` { `os`, `arch`, `cpus`, `cpu_model`?, `kernel`? }.
No hostname or user name is exported, by design.

`BenchmarkResult` fields that are always serialized today: `command`, `mean`, `stddev`
(`null` with one run), `median`, `user`, `system`, `min`, `max`, `exit_codes`. Fields that
the live runner always fills (`BenchmarkRunner::finish`, `src/benchmark/mod.rs`) but that are
`Option` in the type: `times`, `user_times`, `system_times`, `memory_usage_byte`. Everything
else is optional and omitted when absent (see the table below).

Energy today: `mean_energy_joules` and `mean_watts` are present if **at least one** run
produced a sample; `energy_joules` (per run) is present only if **every** run produced one.

## Contract

### 1. Versioning
- The top-level object gains `"schemaVersion": "MAJOR.MINOR"` as its **first** key, e.g.
  `"1.0"`. It is a string, independent of the crate `version`.
- `schemaVersion` is the only camelCase key in the document. The name is fixed by
  `CONTRACTS.md` and #240 so consumers can find it without knowing the rest of the layout.
- A file **without** `schemaVersion` is the legacy shape (perfratio ≤ the release that
  introduces this spec, and joulex/hyperfine exports). `--import-json` keeps reading it.
- Compatibility rules:

| Change | Version bump |
|---|---|
| New optional field, new key in an object, new enum value in a field documented as open (e.g. `measurements.*` status, `unavailable_reason`) | MINOR |
| Field becomes guaranteed that was optional | MINOR |
| Field removed or renamed; unit or type changed; optional field that was guaranteed; meaning of a value changed; closed enum gains a value | MAJOR |

- Consumers must ignore unknown keys and must accept any `MINOR` of the `MAJOR` they support.
- perfratio never emits a `MAJOR` change without one release that emits a deprecation for it
  (§4 shows the pattern).

### 2. Top-level layout (`schemaVersion` 1.0)
```jsonc
{
  "schemaVersion": "1.0",
  "perfratio": {
    "version": "1.2.0",
    "command_line": ["perfratio", "--export-json", "out.json", "..."],
    "started_at": "2026-10-12T14:00:00Z",
    "labels": { "lang": "rust", "algorithm": "quicksort" },   // optional
    "system": { "os": "linux", "arch": "x86_64", "cpus": 16,
                "cpu_model": "...", "kernel": "6.8.0-45-generic" },
    "measurements": {
      "time": "measured",
      "memory": "measured",
      "energy": "unavailable",
      "energy_unavailable_reason": "rapl_unreadable",
      "resources": "not_requested"
    }
  },
  "interrupted": true,                                        // optional
  "results": [ /* one entry per benchmarked command */ ]
}
```

- `perfratio` (object, guaranteed): run metadata, as `ExportMetadata` today.
- `perfratio.measurements` (object, guaranteed, **new**): one status per measurement group,
  host-level and identical for every result:
  - `"measured"`: requested and the backend works;
  - `"unavailable"`: requested, but the backend could not be used (the reason goes in
    `<group>_unavailable_reason`: `unsupported_platform`, `rapl_not_found`,
    `rapl_unreadable`, `other`; open enum);
  - `"not_requested"`: the flag was not given.
  Groups in 1.0: `time` (always `measured`), `memory`, `energy` (`-E/--energy`),
  `resources` (`--resource-usage`). New groups are MINOR.
- `interrupted` (bool, optional, only `true`): the run was stopped by Ctrl-C.
- `results` (array, guaranteed, may be empty).

### 3. `results[]` fields and units
All times are **seconds** (`f64`, `crate::util::units::Second`), memory is **bytes**
(`u64`), energy is **joules**, power is **watts**. Per-run arrays are aligned with `times`
(index *i* is the same run) unless stated otherwise.

| Field | Type / unit | Presence |
|---|---|---|
| `command` | string | guaranteed |
| `mean`, `median`, `min`, `max` | s | guaranteed |
| `stddev` | s or `null` | guaranteed key; `null` with fewer than 2 runs |
| `user`, `system` | s (mean) | guaranteed |
| `times`, `user_times`, `system_times` | s[] | guaranteed (live run) |
| `memory_usage_byte` | bytes[] (peak RSS per run) | guaranteed when `measurements.memory = "measured"` |
| `exit_codes` | (int or `null`)[]; `null` = killed by a signal | guaranteed |
| `relative_speed` | ratio to fastest/reference (1.0 = baseline) | optional (omitted if not computable) |
| `relative_speed_stddev` | ratio | optional (omitted for the baseline) |
| `percentiles` { `p05`, `p25`, `p75`, `p95` } | s | optional |
| `geometric_mean` | s | optional (omitted if a run took 0 s) |
| `cpu_percent` | % | optional |
| `warmup_runs` | count | optional (`--warmup auto`) |
| `first_run` { `time`, `user`?, `system`?, `memory_usage_byte`?, `energy_joules`?, `exit_code`?, `warmup`? } | s / bytes / J | optional (`--first-run=separate`, see `first-run-policy.md`) |
| `mean_energy_joules` | J | only when `measurements.energy = "measured"` and ≥ 1 sample |
| `mean_watts` | W (`mean_energy_joules / mean`) | same as above |
| `energy_joules` | J[] | only when every run has a sample |
| `energy_samples` | count (**new**) | when `measurements.energy = "measured"`: runs with a sample, so partial coverage is visible |
| `custom_metrics` | { name: f64[] } | optional (`--output-metric`, see `custom-output-metrics.md`) |
| `custom_metrics_summary` | { name: { `mean`, `stddev`?, `median`, `min`, `max` } } | with `custom_metrics` |
| `parameters`, `per_run_parameters` | strings / stats | optional (`--parameter-*`) |
| `omitted_failed_runs`, `discarded_outliers` | indices | optional |
| `resources` | counts[] per run | when `measurements.resources = "measured"` |
| `diagnostics`, `precision`, `baseline`, `shell`, `timed_out`, `timeout`, `runs_planned` | see `BenchmarkResult` | optional |

Rules:
- An unavailable or unrequested value is **omitted**, never written as `0`. `null` is used
  only where the table says so (`stddev`, `exit_codes[i]`).
- `custom_metrics` units are whatever the command prints; perfratio does not interpret them.
  Names follow `[A-Za-z0-9_]+`. A consumer that needs units encodes them in the name
  (e.g. `algo_ns`) or in a `--label`.
- Fields added later by #48 (`metric`, `primary`), #59 (net energy, domains) and #216
  (`target_duration_seconds`, `total_sampling_seconds`) are MINOR additions to this schema
  and must be added to this table in the same PR.

### 4. Removing the legacy `joulex` key (#225)
1. **Deprecation release** (first release with `schemaVersion: "1.0"`): `"joulex"` is still
   written, identical to `"perfratio"`. The CHANGELOG and `doc/understanding-output.md` mark
   it deprecated. `schemaVersion` 1.0 documents only `"perfratio"`; consumers of 1.x must
   not read `"joulex"`.
2. **Next release**: `"joulex"` is no longer written. Removing a key that 1.0 never
   documented is not a schema change, so `schemaVersion` stays `1.x`. Files that only have
   `"joulex"` (old exports) remain importable via `--import-json`.
3. Tests that assert both keys (`tests/export_metadata_tests.rs`,
   `tests/check_system_tests.rs`) assert only `"perfratio"` and the absence of `"joulex"`.

### 5. Recommended flags for cross-language runs
Documented in `doc/understanding-output.md` (new section "Comparing implementations across
languages") and referenced from the benchmark-tool catalog contract:

```
perfratio -N --warmup auto --first-run=separate \
          --output-metric 'algo_ns=ALGO_NS=(\d+)' \
          --label lang=<lang> --label algorithm=<name> \
          --export-json result.json '<run command>'
```
- `-N` (no shell): the shell spawn would otherwise be charged to every language.
- `--warmup auto`: JIT and VM languages (JVM, .NET, V8, PyPy) reach steady state; AOT
  languages stop after a few runs.
- `--first-run=separate`: the cold start (runtime boot, class loading, page cache) is kept
  in `first_run` instead of being mixed into the statistics or thrown away.
- `--output-metric`: the program times only its algorithm and prints it; this separates
  algorithm time from process startup, which `times` always includes.
- `--label`: language, algorithm and implementation identity travel inside the file, so
  the consumer does not depend on file names.
- `-E/--energy` only on bare-metal Linux with readable RAPL; the consumer checks
  `perfratio.measurements.energy` before using energy fields.
- Fast implementations: `--min-runs` today, `--duration` once #216 lands.

## Implementation
- `src/export/json.rs`: add `schema_version: &'static str` serialized as `"schemaVersion"`
  first in `PerfratioSummary`; a `pub const SCHEMA_VERSION: &str = "1.0"`.
- `src/export/metadata.rs`: `ExportMetadata::measurements: Measurements`, filled from
  `Options` and the energy sampler's `is_available()` (`src/energy/mod.rs`).
  `LinuxRaplSampler::try_new` returns the reason it failed.
- `src/benchmark/benchmark_result.rs`: `energy_samples: Option<usize>`.
- `schema/perfratio-output.schema.json` (JSON Schema 2020-12) describing 1.0, published with
  the release assets (#93) so images that pin a perfratio version can pin the schema too.
- The deprecation steps of §4, which cover only the JSON key of #225. The other #225
  items are not part of this spec.

## Acceptance Criteria
- [ ] Every `--export-json` file starts with `"schemaVersion": "1.0"`.
- [ ] `schema/perfratio-output.schema.json` exists, and CI validates the export of the
      integration tests against it (with and without `--energy`, `--output-metric`,
      `--first-run=separate`, `--parameter-scan`, Ctrl-C).
- [ ] `perfratio.measurements` is present; `energy` is `"unavailable"` with a reason when
      `--energy` is given on a host without readable RAPL, and `"not_requested"` without
      `--energy`.
- [ ] No result contains `0` in place of an unmeasured value.
- [ ] `energy_samples` makes partial energy coverage visible.
- [ ] `"joulex"` follows the two-step removal of §4; old exports with only `"joulex"`
      still import.
- [ ] `doc/understanding-output.md` documents the schema, units, compatibility rules and
      the recommended cross-language flags.
- [ ] `BenchCommons/.product` `CONTRACTS.md` row "Measurement output" links this spec.

## Out of Scope
- Correctness checks of the algorithm output and per-language builds/Dockerfiles: they
  belong to the catalog contract owned by benchmark-tool (`benchmark-tool#29`).
- The published `results.json` schema (owned by benchmark-tool, consumed by the website).
- CSV, Markdown and other exports: only JSON is the machine contract.
- The features that will add fields (#48, #59, #212, #216, #223), the library crate (#92)
  and distribution (#93): they have their own issues and only must follow §1.
- The non-JSON leftovers of #225 (environment variables, completions, manpage, tests).

## Verification Plan
- Unit: `SCHEMA_VERSION` is serialized first; `Measurements` from options × sampler
  availability; `energy_samples` with full, partial and no samples.
- Integration (`tests/export_metadata_tests.rs`): `schemaVersion`, `measurements`, absence
  of `"joulex"` after step 2; import of a legacy file with only `"joulex"`.
- Schema: every JSON produced by the test suite validates against
  `schema/perfratio-output.schema.json`.
