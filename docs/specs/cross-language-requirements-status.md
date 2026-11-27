# Spec: Status of the cross-language requirements (#240)

## Problem / Motivation
References: #240, `BenchCommons/catalog-algorithms#11`, `docs/CROSS_LANGUAGE.md`,
`docs/specs/output-schema-contract.md`.

Issue #240 lists what `catalog-algorithms` and `tools-benchmark` need from perfratio to measure
one algorithm in many languages. Its progress is spread over issue comments, PRs and specs. This
document is the single checklist against `master` (`ef49a5e`, `schemaVersion` 1.36) and the open
PRs #393 to #407. It adds no feature and changes no contract.

Legend: **Master** = shipped on `master`. **Open PR** = only in an open PR. **Open** = not done.

## Contract
Documentation only. The JSON export (`schemaVersion`, `docs/schema/perfratio-export.schema.json`)
is not changed by this document. Open PRs #393 to #400 and #402 to #405 each bump
`schemaVersion` to 1.37. The second to merge must renumber (see their descriptions), so consumers
must read `schemaVersion` and accept any 1.x, as `docs/CROSS_LANGUAGE.md` says.

## Status per requirement

| # | Requirement (issue #240) | State | Flag | JSON field | Test / evidence |
|---|---|---|---|---|---|
| 1 | Versioned JSON schema, no legacy `joulex` key (#225) | **Master** (#262, #332, #390) | none (always on) | `schemaVersion` (first key), `perfratio.measurements` | `tests/export_metadata_tests.rs` (layout snapshot, measurements), `tests/json_schema_tests.rs` (about 40 flag combinations against `docs/schema/perfratio-export.schema.json`), `tests/json_schema_compat_tests.rs` (per-version archive), `tests/doc_versions_tests.rs` |
| 1a | `.product` `CONTRACTS.md` links the spec | **Open**, other repo, tracked in #408 | n/a | n/a | n/a |
| 2a | Rank by a chosen metric (#48) | **Master** (#226) | `--metric wall\|cpu\|user\|system\|energy\|memory` (also `custom:<name>`, `region:<name>`) | `metric`, `primary_metric` | `tests/metric_tests.rs`, `tests/metric_direction_tests.rs` |
| 2b | Multi-dimensional delta table (#212) | **Master** (#274, #328) | `--delta-table` | Console only, no JSON field | `tests/delta_table_tests.rs`, `docs/specs/delta-table.md` |
| 3a | Energy: idle baseline (#59) | **Master** (#275) | `--energy-baseline DURATION` | `idle_watts`, `mean_net_energy_joules` | `tests/energy_baseline_tests.rs` (the `bare_metal_*` test is ignored without RAPL) |
| 3b | Energy: RAPL domains (#59) | **Master** (#299) | `--energy-domains` | `energy_domains_joules` | `tests/energy_domains_tests.rs`, `docs/specs/rapl-domains.md` |
| 3c | PMU RAPL backend without root sysfs (#223) | **Master** (#272), unvalidated on hardware | `--energy-backend auto\|sysfs\|perf` | `perfratio.energy_backend`, `measurements.energy`, `energy_unavailable_reason` | Unit tests in `src/energy/mod.rs` (`classifies_unavailable_reasons`), `tests/export_metadata_tests.rs::measurements_energy_requested_is_measured_or_unavailable_with_reason`, `docs/specs/perf-rapl-backend.md`. No test reads a real power PMU. |
| 3d | Energy on macOS and Windows (#291) | **Master** (#295) | `--energy-backend ioreport\|emi` | same fields, new reasons | `docs/specs/energy-macos-windows.md` |
| 3e | Energy validated on bare-metal Linux | **Open**, tracked in #337 | `-E`, `--energy-backend perf` | n/a | `cargo test -- --ignored` and `PERFRATIO_TEST_PERF=1 cargo test` on real hardware. CI covers time and memory only. |
| 3f | Energy in other domains: GPU, estimated attribution, external meter | **Open PR** #401 (#380), #402 (#381, #382) | `--gpu`, attribution and meter flags per those PRs | additive fields, schema 1.37 | Not on `master`. Not part of #240's list, listed because they affect cross-language energy comparison. |
| 4 | Short runs for fast languages (#216) | **Master** (#269) | `--duration DURATION` (at least 3 runs; conflicts with `--runs`, `--target-precision`) | duration fields, absent without the flag | `tests/duration_tests.rs`, `docs/specs/duration.md` |
| 5 | Distribution: binaries or crates.io, pinned version (#93) | **Partly done**, #93 open | n/a | n/a | Release v1.0.0 has archives, `.deb`, SBOM and `SHA256SUMS` (#273, `docs/RELEASING.md`). Not done: crates.io (`CARGO_REGISTRY_TOKEN`), Homebrew tap, winget. README says crates.io and Homebrew are planned. Needs maintainer credentials. |
| 6 | Optional library crate for a Rust driver (#92) | **Master** (#277), pre-stable | none | none (JSON unchanged) | `docs/specs/library-crate.md`, `src/api.rs`. #92 is closed. |
| 7a | Docs: recommended flags | **Master** (#280, #285) | `-N`, `--warmup auto`, `--first-run=separate`, `--output-metric`, `--label`, `--export-json` | `first_run`, `custom_metrics`, `perfratio.labels` | `docs/CROSS_LANGUAGE.md`, `tests/warmup_tests.rs` (`auto_warmup_*`), `tests/first_run_tests.rs`, `tests/output_metric_tests.rs`, `tests/custom_metric_tests.rs`, `tests/export_metadata_tests.rs::json_contains_run_metadata_and_labels` |
| 7b | Separate startup from algorithm time | **Master** | `--output-metric NAME=REGEX`, `--region[=NAME]` | `custom_metrics`, `results[].regions` | `tests/output_metric_tests.rs`, `tests/region_tests.rs`, `docs/specs/region-protocol.md`. Region SDKs on `master`: C/C++, Rust, Python (`sdk/`, `tests/sdk_tests.rs`). |
| 7c | Region SDKs for more languages | **Open PR** #399 (JS/TS, Go, Java, Kotlin) | `--region` | `results[].regions` | #372. Any other language can print a value and use `--output-metric`. |

## Open PRs that touch cross-language runs
None of #393 to #407 closes an item of #240. Relevant ones:
- #393 (#375) `--evidence DIR`, `--verify-evidence`: reproducible bundle of one result. Own versioned manifest, no `schemaVersion` bump.
- #399 (#370, #372) `--persistent` and region SDKs for JS/TS, Go, Java, Kotlin (`results[].mode`, `iterations`).
- #400 (#367, #369) statistical power and multi-metric stopping (`results[].power`, `precision_metrics`).
- #396 (#368) `--ci-method hierarchical`, `--paired` (`results[].hierarchy`, `paired`).
- #401, #402, #404 GPU, attribution and memory bandwidth/NUMA energy and memory metrics.
- #406 `--verdict-json`: a separate document with its own `verdictVersion`, no change to the export.
- #394, #397, #398, #403, #405: counters, roofline, Pareto, collectors, `--optimize` search.
- #395 (dependency bump) and #407 (test fix) are unrelated.

## Requirements found without an issue
- The `.product` `CONTRACTS.md` link: now #408.
- `docs/CROSS_LANGUAGE.md` links `region-sdk.md`, but the file is `docs/specs/region-sdks.md` (and the guide sits in `docs/`, so the link is broken). A one-line docs fix, not a requirement, so no issue. Fix it with the next edit of that file.
- `docs/specs/output-schema-contract.md` says energy was tested only against a fake powercap tree. This is the open validation in #337, not a new item.

## Still open
1. `.product` `CONTRACTS.md` link (#408, other repository).
2. Energy validation on bare-metal Linux, including the perf backend (#337).
3. Publication to crates.io and Homebrew, winget (#93).
4. Region SDKs for more languages (#399 / #372, open PR).

#240 stays open until items 1 to 3 are done.
