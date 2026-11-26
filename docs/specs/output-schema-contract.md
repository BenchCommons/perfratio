# Spec: Versioned JSON Output Schema Contract (`schemaVersion`) (#240)

## Status
Shipped except the `.product` link (schemaVersion 1.29; 1.29 declares the unit and direction of `custom.<name>` descriptors with `--output-metric NAME[unit,higher]=REGEX` and reads them from `--import-json` (#317, `metric-descriptors.md`); 1.28 adds `perfratio.environment`, `perfratio.environment_fingerprint` and `perfratio.meta` from #314 (`environment-fingerprint.md`); 1.27 adds `perfratio.calibration` and `results[].quality.below_resolution` from #313 (`measurement-floor.md`); 1.26 adds `results[].precision.stopping_rule` and `achieved_half_width` from #322 (`sequential-stopping.md`); 1.25 adds `perfratio.schedule` from #324 (`schedule-randomized.md`); 1.24 adds `perfratio.execution.cpu_partition` (#307, `worker-cpu-partition.md`); 1.23 adds `results[].regions[]`, `perfratio.region_protocol`, `measurements.region` and `region:<name>` metric values from #310 (`region-protocol.md`); 1.22 adds `perfratio.execution` and `results[].workers_used` / `contended` for `--jobs` (#306, `worker-pool.md`); 1.21 adds `perfratio.statistics` and `results[].relative_speed_ci` from #320 (`statistics-settings.md`); 1.20 adds `perfratio.metrics[]`, the metric descriptors of #317 (`metric-descriptors.md`); 1.19 adds `results[].run_ids`, `instance_run_ids`, `leftover_processes` and `energy_scope` from #308 and #309 (`run-identity.md`, `process-tree.md`) and stops writing `energy_per_instance_joules`; 1.18 adds `results[].quality` from #318 (`measurement-quality.md`); 1.17 adds `results[].validation` from #316 (`workload-validation.md`); 1.16 adds the optional `pareto` object from #326 (`pareto.md`); 1.15 adds `optimize.reevaluated` and fractional `optimize` values with `-D` (#98); 1.14 adds `--simd` fields from #211 (`simd-breakdown.md`); 1.13 adds the software events and the effective frequency fields of #220 and #222; 1.12 adds `--topdown` fields from #221 (`topdown-tma.md`); 1.11 adds `results[].energy_samples` (#240) and the JSON Schema file `docs/schema/perfratio-export.schema.json`, validated by `tests/json_schema_tests.rs`; 1.10 adds `results[].counters_coverage` (#219); 1.9 adds the `:u`/`:k` counter key suffixes, `counters_scope` values `kernel` and `perfratio.measurements.counters_requested_scope` from #218 (`counter-scope-modifiers.md`); 1.8 adds `--counters` fields from #49; 1.7 adds `results[].energy_domains_joules` (#59, `rapl-domains.md`); 1.6 adds the optional `optimize` object from #98; 1.5 adds the `--concurrency` fields from #53; 1.4 adds the macOS/Windows energy backends and reasons of `energy-macos-windows.md` (#291); 1.3 is 1.2 plus per-column `resources` availability from #291; 1.1 plus `custom:<name>` metric values from #90; 1.0 plus `metric`/`primary_metric` from #48 made 1.1): `schemaVersion` first key (`SCHEMA_VERSION` in
`src/export/json.rs`), `perfratio.measurements` (`src/export/metadata.rs`, reasons from
`src/energy/mod.rs::probe`; `memory` is always `measured`, `time` always `measured`),
`joulex` removed (§4), layout snapshot and contract tests in `tests/export_metadata_tests.rs`.
Not yet done: the `.product` `CONTRACTS.md` link (a change in that repository). `doc/understanding-output.md` has the
section "The JSON export". Energy status
was tested only through the probe against a fake powercap tree and on a host without RAPL
(macOS: `unsupported_platform`); real RAPL needs bare-metal Linux.

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
  `"1.1"`. It is a string, independent of the crate `version`.
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

### 2. Top-level layout (`schemaVersion` 1.1)
```jsonc
{
  "schemaVersion": "1.1",
  "metric": "wall",                                           // new in 1.1
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
      "resources": "not_requested",
      "counters": "measured",                                 // new in 1.8, #49
      "topdown": "measured"                                   // new in 1.12, #221
      "simd": "measured"                                      // new in 1.14, #211
    },
    "execution": { "jobs": 4, "schedule_policy": "saturate",   // only with --jobs > 1, new in 1.22, #306
                   "contended": true,
                   "cpu_partition": { "workers": [[0,1],[2,3]], "reserved": [15] } }   // pinned workers only, new in 1.24, #307
  },
  "interrupted": true,                                        // optional
  "optimize": { "var": "j", "best": 24, "...": "..." },       // optional, new in 1.6 (#98)
  "pareto": { "frontier": [0, 2], "...": "..." },             // optional, new in 1.16 (#326)
  "results": [ /* one entry per benchmarked command */ ]
}
```

- `metric` (string, guaranteed, **new in 1.1**, #48): metric used for ranking and relative
  speed, one of `wall`, `cpu`, `user`, `system`, `energy`, `memory`, or `custom:<name>` (**new in 1.2**,
  #90: a metric extracted with `--output-metric <name>=...`, lower is better, `<name>` matches
  `[A-Za-z0-9_]+`) (`--metric`, default `wall`; see `metric-target.md`). The set of built-in
  values is closed (a new built-in value is MAJOR); the `custom:` prefix is a pattern, so
  consumers must accept any `custom:<name>`. Its unit and direction are those of the `custom.<name>` descriptor in `perfratio.metrics[]` (**new in 1.29**); without a declaration it is unitless and lower is better.
- `perfratio` (object, guaranteed): run metadata, as `ExportMetadata` today.
- `perfratio.statistics` (object, **new in 1.21**, #320; written by every export of 1.21 and later, absent from older ones): `{estimator, confidence_level,
  significance_level, bootstrap_resamples, bootstrap_seed}`. `estimator` is `mean`, `median` or a
  percentile such as `p95` and is the central value behind `relative_speed`, `significance`,
  `p_value` and the delta table. With the defaults (`mean`, 0.95, 0.05, 5000 and the fixed
  default seed) every other number is unchanged. `bootstrap_seed` is an unsigned 64-bit integer
  (the default exceeds 2^53: parse it as an integer, not a double).
- `perfratio.execution` (object, **new in 1.22**, #306): only with `--jobs` > 1. `jobs` (integer), `schedule_policy` (`saturate`, `exclusive` or `fair`; open enum) and `contended` (always `true`) and, when the workers were pinned (**new in 1.24**, #307), `cpu_partition` = { `workers`: CPUs of each worker, disjoint, `reserved`?: CPUs kept for perfratio (omitted when none), `shared_siblings`?: `true` when there are more workers than physical cores }. Without `--jobs` the object is absent; without pinning (`--pin-workers=none`, macOS) `cpu_partition` is absent.
- `perfratio.schedule` (object, **new in 1.25**, #324; written by every export of 1.25 and later): `{mode, seed?, order?}`. `mode` is `grouped`, `round-robin`, `randomized`, `balanced` or, with `--jobs`, `saturate`, `exclusive` or `fair` (open enum). `seed` (unsigned 64-bit integer, the `--seed` value) only with `randomized`. `order` only for an interleaved schedule (`round-robin`, `randomized`, `balanced`) with at least two commands: the `results[]` index of each measured run in execution order, the initial measurement of every command being the first round; warmup runs are not listed. It is a separate object because `perfratio.execution` of 1.22 requires its `--jobs` keys.
- `perfratio.measurements` (object, guaranteed, **new**): one status per measurement group,
  host-level and identical for every result:
  - `"measured"`: requested and the backend works;
  - `"unavailable"`: requested, but the backend could not be used (the reason goes in
    `<group>_unavailable_reason`: `unsupported_platform`, `rapl_not_found`,
    `rapl_unreadable`, `unsupported_cpu`, `ioreport_not_found`, `ioreport_frozen`,
    `emi_not_found`, `emi_unreadable` (1.3), `other`; open enum; `perfratio.energy_backend` is
    `sysfs_powercap`, `perf_pmu`, `ioreport` or `windows_emi`);
  - `"not_requested"`: the flag was not given.
  Groups in 1.0: `time` (always `measured`), `memory`, `energy` (`-E/--energy`),
  `resources` (`--resource-usage`), `counters` (`--counters`, new in 1.8), `topdown` (`--topdown`, new in 1.12; `measured`, `unavailable` or
  `not_requested`, with `topdown_unavailable_reason` in `unsupported_platform`, `unsupported_cpu`,
  `unsupported_kernel`, `perf_paranoid`, `no_pmu`, `hybrid_cpu_unpinned`, `other`). New groups are MINOR.
  `counters` also has the value `"partial"` (some events could not be opened, listed in
  `counters_unavailable_events`) and the reasons `unsupported_platform`, `perf_paranoid`,
  `no_pmu`, `other` in `counters_unavailable_reason`. `counters_requested_scope` (**new in 1.9**,
  optional, string) is the scope that was asked for when it differs from `counters_scope`:
  `"all"` next to `"user"` means kernel counting was denied (`perf_event_paranoid >= 2`).
  `counters_unavailable_events` lists keys, so suffixed ones read `instructions:k`.
  `region` (`--region`, new in 1.23) has the values `measured`, `not_requested`, and
  `perfratio.region_protocol` (integer) is present when it is `measured`.
  `simd` (`--simd`, new in 1.14) has the values `measured`, `unavailable`, `not_requested`, and
  `simd_unavailable_reason` is one of `unsupported_platform`, `unsupported_cpu`,
  `perf_paranoid`, `no_pmu`, `hybrid_cpu_unpinned`, `other` (open enum).
- `interrupted` (bool, optional, only `true`): the run was stopped by Ctrl-C.
- `optimize` (object or array of objects, optional, **new in 1.6**, #98): present with
  `--optimize <VAR> <MIN> <MAX>`. One object per search (an array when other parameters
  produce several searches). Fields: `var`, `min`, `max`, `metric`, `algorithm`
  (`golden-section`, or `exhaustive` when the range fits into the budget), `budget`,
  `evaluations`, `best`, `best_value` (mean of `metric`), `best_command`, `evaluated` (values in
  evaluation order), `at_boundary`, `unimodal_warning`; optional `runner_up`, `p_value`,
  `significant` (best vs runner-up; omitted when not computable), `fixed` (values of the
  other parameters of this search), `reevaluated` (**new in 1.15**: the values that
  `--optimize-confirm` ran a second time; their samples are pooled into `best_value`, while
  `results[]` keeps the first run) and `interrupted` (only `true`). With `-D DELTA` the
  searched values are `MIN, MIN+DELTA, ...`; `min`, `max`, `best`, `runner_up`, `evaluated`
  and `reevaluated` are then JSON numbers that may be fractional (integers stay integers). Every evaluated value is
  also an ordinary `results[]` entry with `parameters: { "<var>": "<value>" }`, in
  evaluation order. See `optimize.md`.
- `pareto` (object, optional, **new in 1.16**, #326): present with `--pareto`, `--constraint`
  or `--objective`. Fields: `metrics` (`[{metric, direction}]`, direction `lower_is_better` or
  `higher_is_better`), `frontier` (indices into `results`), `dominated` (`[{command_index, by}]`),
  `excluded` (`[{command_index, reason, detail}]`, reason `metric_unavailable` or
  `constraint_violated`); optional `constraints` (as written), `noise_threshold` (fraction),
  `within_noise` (frontier members strictly dominated within the threshold) and `objective`
  (`{direction, metric, best, value}`, `best` null if no candidate). A command without data
  for a metric is excluded, never read as 0. See `pareto.md`.
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
| `primary_metric` { `metric`, `mean`, `stddev`?, `samples`? } (**new in 1.1**, #48; `metric` may be `custom:<name>` since 1.2) | metric unit (s, J, bytes; custom: as printed by the command) | optional (only with `--metric` other than `wall` and data for the metric) |
| `relative_speed` | ratio to fastest/reference (1.0 = baseline) | optional (omitted if not computable) |
| `relative_speed_stddev` | ratio | optional (omitted for the baseline; only with `statistics.estimator = "mean"`) |
| `relative_speed_ci` (**new in 1.21**, #320) | `[low, high]` ratio, bootstrap interval at `statistics.confidence_level` | optional (estimators other than the mean, not for the baseline) |
| `percentiles` { `p05`, `p25`, `p75`, `p95` } | s | optional |
| `geometric_mean` | s | optional (omitted if a run took 0 s) |
| `cpu_percent` | % | optional |
| `warmup_runs` | count | optional (`--warmup auto`) |
| `first_run` { `time`, `user`?, `system`?, `memory_usage_byte`?, `energy_joules`?, `exit_code`?, `warmup`? } | s / bytes / J | optional (`--first-run=separate`, see `first-run-policy.md`) |
| `mean_energy_joules` | J | only when `measurements.energy = "measured"` and ≥ 1 sample |
| `mean_watts` | W (`mean_energy_joules / mean`) | same as above |
| `energy_joules` | J[] | only when every run has a sample |
| `energy_samples` (**new in 1.11**, #240) | count | whenever an energy sampler was active (`--energy` and a usable backend): the number of runs that produced a sample, so `energy_samples < len(times)` shows partial coverage. Absent when energy was not requested or unavailable |
| `idle_watts` (**new in 1.1**, #59) | W | optional (`--energy-baseline` and the idle measurement succeeded) |
| `energy_domains_joules` (**new in 1.7**, #59) | object `{package,core,uncore,dram,psys: J per run[]}`, aligned with `times`; overlapping domains, not additive, `energy_joules` is the package total | optional (with `--energy-domains` on sysfs RAPL, when every run has the same domains) |
| `mean_net_energy_joules` (**new in 1.1**, #59) | J, per run `max(energy - idle_watts * time, 0)` | optional (with `idle_watts` and ≥ 1 energy sample) |
| `custom_metrics` | { name: (f64 \| null)[] }, one entry per run aligned with `times`; `null` = not extracted with `-i` | optional (`--output-metric`, see `custom-output-metrics.md`) |
| `custom_metrics_summary` | { name: { `mean`, `stddev`?, `median`, `min`, `max` } } | with `custom_metrics` |
| `parameters`, `per_run_parameters` | strings / stats | optional (`--parameter-*`) |
| `omitted_failed_runs`, `discarded_outliers` | indices | optional |
| `validation` (**new in 1.17**, #316) | object `{mode, on_invalid, first_only, runs_checked, runs_invalid, runs_omitted, omitted_runs?, output_sha256?}`, see `workload-validation.md` | only with `--expect-output-sha256` / `--validate`; `runs_invalid > 0` means the workload gave a wrong result |
| `quality` (**new in 1.18**, #318) | object `{grade, checks: [{id, metric?, status, detail?}], below_resolution?}` (`below_resolution`: new in 1.27, #313); `grade` is `comparable`, `with_caveats` or `unreliable`; `status` is `pass`, `warn`, `fail`, `not_applicable` or `unavailable`; check ids are an open enum, see `measurement-quality.md` | every measured result |
| `resources` { column: counts[] per run } (**columns individually optional since 1.3**, #291) | counts | when `measurements.resources = "measured"`; a column the OS cannot report is omitted (Windows has no `voluntary_ctx_switches`, `involuntary_ctx_switches`, `major_faults`), see `windows-resource-usage.md` |
| `concurrency` (**new in 1.5**, #53) | count | only with `--concurrency N`, N > 1. **When present, `times` holds the makespan of each run** (wall time until all N instances finished) and `exit_codes` holds N entries per run, row-major (run 0 instances 0..N-1, ...), i.e. `runs x N` entries instead of `runs` |
| `instance_times` (**new in 1.5**, #53) | s[][], one row of N per run aligned with `times`, instances in spawn order | with `concurrency` |
| `throughput` (**new in 1.5**, #53) | instances per second, `N / mean` | with `concurrency` (omitted for a zero mean) |
| `instance_percentiles` { `p50`, `p95`, `max` } (**new in 1.5**, #53) | s, over all `runs x N` instance times | with `concurrency` |
| `energy_per_instance_joules` (**new in 1.5**, #53; **no longer written since 1.19**, #309) | J, was `mean_energy_joules / N` | never: package energy is shared by the instances, so it is not attributed per instance; see `energy_scope`. Consumers must treat the field as optional (it still appears in files of 1.5 to 1.15) |
| `energy_scope` (**new in 1.19**, #309) | `"system"` or `"batch"` | with `mean_energy_joules`. `batch` with `--concurrency`: the value is the energy of the whole batch over the makespan; `system`: package counter of a single instance, which covers every process of the machine. Never `process_tree` |
| `run_ids` (**new in 1.19**, #308) | u64[], one per run, aligned with `times` | when runs were recorded. `PERFRATIO_RUN_ID` of the run; with `--concurrency` the id of instance 0, which `--prepare` and `--conclude` also see |
| `instance_run_ids` (**new in 1.19**, #308) | u64[][], one row of N per run aligned with `instance_times` | with `concurrency`. The position in the row is the `PERFRATIO_WORKER_ID` of the instance |
| `leftover_processes` (**new in 1.19**, #309) | `true` | only when true: in at least one run (or its prepare/conclude) processes started by the command were still running after it exited; they were killed. The runs stay in the statistics |
| `workers_used` (**new in 1.22**, #306) | count of workers that executed at least one timed run | only with `--jobs N`, N > 1 |
| `contended` (**new in 1.22**, #306) | `true`: the runs were executed in parallel and may have disturbed each other; a consumer should not compare them with sequential results | only with `--jobs N`, N > 1 |
| `counters` (**new in 1.8**, #49) | { event: (u64 \| null)[] } raw counts per run aligned with `times`; `null` = not readable; events `instructions`, `cycles`, `cache-references`, `cache-misses`, `branches`, `branch-misses` (open set) | when `measurements.counters` is `measured` or `partial` |
| `counters` events `cpu-migrations`, `context-switches`, `page-faults` (counts), `task-clock` (ns) (**new in 1.13**, #220) | counts | when requested by name or with `--counters=sw`; scheduler events are never counted when kernel scope is denied: then they are absent and `measurements.counters_unavailable_*` says `kernel_scope_denied` |
| `effective_frequency_hz` (**new in 1.13**, #222) | Hz (f64 \| null)[] aligned with `times`, `cycles` / time on CPU | when `cycles` was counted and a time source exists |
| `mean_effective_frequency_hz`, `stddev_effective_frequency_hz` (**new in 1.13**) | Hz, over the runs with at least 5 ms on CPU | with `effective_frequency_hz` (omitted when no run is long enough; the stddev with one eligible run) |
| `effective_frequency_basis` (**new in 1.13**) | `"task_clock"`, `"user_time"` or `"system_time"` (open enum) | with `effective_frequency_hz` |
| `counters_summary` (**new in 1.8**) | { event: { `mean`, `stddev`?, `median`, `min`, `max` } } | with `counters` |
| `counters_scope` (**new in 1.8**, `"kernel"` new in 1.9) | effective scope of unsuffixed event names: `"all"` (user + kernel), `"user"` (also when kernel counting was denied and `--scope` was implicit) or `"kernel"` (`--scope kernel`); `--scope split` reports `"all"`. A name with a `:u` / `:k` suffix (**new in 1.9**, #218) has that scope, e.g. `instructions:u`; the `split` total is `:u` + `:k` and is not stored | with `counters` |
| `counters_multiplexed_runs` (**new in 1.8**) | count of runs with a scaled counter | optional (> 0) |
| `counters_coverage` (**new in 1.10**, #219) | { event: number } lowest `time_running / time_enabled` (0..1) over the runs, only for events that were multiplexed or never scheduled | optional (with `counters_multiplexed_runs`) |
| `topdown` (**new in 1.12**, #221) | { `level`: 1, `source`: `intel_perf_metrics` \| `amd_pipeline_util`, `retiring`, `bad_speculation`, `frontend_bound`, `backend_bound`, `smt_contention` (AMD only): (0..1 \| null)[] } per run, aligned with `times`; `null` = run not measured. Hypothesis about the stall class, not a diagnosis | with `--topdown` |
| `topdown_summary` (**new in 1.12**) | { class: { `mean`, `stddev`?, `median`, `min`, `max` }, `bottleneck`: `backend_bound` \| `frontend_bound` \| `bad_speculation` \| `smt_contention` (open enum; retiring is never named) } | with `topdown` |
| `simd` (**new in 1.14**, #211) | { `unit` (`instructions` \| `uops`), `arch`, `scalar_fp`, `simd_128`, `simd_256`, `simd_512`?, `vectorization_ratio` }: per-run arrays aligned with `times`, `null` = not readable (or no FP arithmetic for the ratio); `simd_512` only on CPUs that have the bucket; Intel and AMD units are not comparable | when `measurements.simd` is `measured` |
| `simd_summary` (**new in 1.14**) | { bucket or `vectorization_ratio`: { `mean`, `stddev`?, `median`, `min`, `max` } } | with `simd` |
| `simd_scope` (**new in 1.14**) | `"all"`, `"user"` or `"kernel"`, like `counters_scope` | with `simd` |
| `regions` (**new in 1.23**, #310) | [ { `name`, `status` (`measured` \| `region_missing`), `mean`, `stddev`?, `min`, `max`, `count_per_run`, `times`, `sdk`? } ]: seconds inside each `--region` per run, `null` where a run did not report the region; `times` of the result keep measuring the whole process | with `--region` |
| `simd_scaling` (**new in 1.14**) | { bucket: { `scaled_runs`, `coverage` (lowest) } } for buckets scaled for multiplexing | optional |
| `efficiency` { `ipc`, `cpi`, `cache_miss_rate`, `branch_miss_rate` } (#213) | ratios | optional (with the needed counters) |
| `precision` (`stopping_rule` and `achieved_half_width` **new in 1.26**, #322) | `{target, reached, confidence, met, stopping_rule?, achieved_half_width?}`: `stopping_rule` is `fixed_ci` (default) or `confidence_sequence` (`--target-precision-method sequential`), open enum; `achieved_half_width` is the relative half-width of that rule's interval (equal to `reached`) | optional, with `--target-precision` |
| `diagnostics`, `baseline`, `shell`, `timed_out`, `timeout`, `runs_planned` | see `BenchmarkResult` | optional |

Rules:
- An unavailable or unrequested value is **omitted**, never written as `0`. `null` is used
  only where the table says so (`stddev`, `exit_codes[i]`).
- `concurrency` is the discriminator for the changed meaning of `times` and `exit_codes` (see the table). Without it, nothing changes. `user`/`system` are then sums over the instances of a run, `memory_usage_byte` is the maximum of any instance.
- `custom_metrics` units are whatever the command prints; perfratio does not interpret them.
  Names follow `[A-Za-z0-9_]+`. A consumer that needs units encodes them in the name
  (e.g. `algo_ns`) or in a `--label`.
- `metric` and `primary_metric` (#48), `idle_watts` and `mean_net_energy_joules` (#59) were added in 1.1 as MINOR additions. `custom:<name>` as a `metric` / `primary_metric.metric` value (#90) is a MINOR addition in 1.2: it is emitted only when the user passes `--metric custom:<name>`, so existing consumers are unaffected, and `samples` then contains the extracted values (no nulls, see `custom-output-metrics.md`). Fields added later by #59 (domains) and #216
  (`target_duration_seconds`, `total_sampling_seconds`) are MINOR additions to this schema
  and must be added to this table in the same PR.

### 4. Removing the legacy `joulex` key (#225)
**BREAKING for consumers:** the key is dropped in one step with no deprecation window, as
issue #240 requires. Tools reading `json["joulex"]` must switch to `json["perfratio"]`
(identical content). See `docs/MIGRATION.md`.

Shipped as a single step (decision of the #240 implementation, replacing the earlier
two-step plan): `schemaVersion` `"1.0"` is the first schema and it never documented
`"joulex"`, so the key is **not written** from the first release that emits `schemaVersion`.
Consumers of 1.x read only `"perfratio"`. Files that only have `"joulex"` (old exports)
remain importable via `--import-json` (covered by a test). `tests/export_metadata_tests.rs`
and `tests/check_system_tests.rs` assert the absence of `"joulex"`.

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
  first in `PerfratioSummary`; a `pub const SCHEMA_VERSION: &str = "1.1"`.
- `src/export/metadata.rs`: `ExportMetadata::measurements: Measurements`, filled from
  `Options` and the energy sampler's `is_available()` (`src/energy/mod.rs`).
  `LinuxRaplSampler::try_new` returns the reason it failed.
- `src/benchmark/benchmark_result.rs`: `energy_samples: Option<usize>`.
- `docs/schema/perfratio-export.schema.json` (JSON Schema 2020-12) describes the current
  `SCHEMA_VERSION` (its `schemaVersion` is a `const`); it is published with the release assets
  (#93) so images that pin a perfratio version can pin the schema too. It allows unknown keys,
  as §1 requires of consumers. `tests/json_schema_tests.rs` validates exports of about 40 flag
  combinations against a strict copy of it (no undeclared keys), checks that every schema
  property is produced by some case and that the `const` equals `SCHEMA_VERSION`. **A change to
  the export must update the schema in the same PR.**
- Per-version archive (#319): `docs/schema/<MAJOR.MINOR>/perfratio-export.schema.json` is a
  frozen copy of the schema of each `schemaVersion`. **A MINOR bump must add the new
  directory** and never edit an older one. `tests/json_schema_compat_tests.rs` checks that
  the current schema equals the archive of its version, that a fresh export validates against
  every older archived schema of the same MAJOR (additive changes only), and that the diff
  against the previous MINOR has no removed or renamed property, changed type, removed enum
  value or newly required key (those are MAJOR changes). The release workflow uploads
  `perfratio-export-<MAJOR.MINOR>.schema.json` to the GitHub release and includes it in
  `SHA256SUMS`. Not done: a `$schema` field in the export and `--print-schema`.
- The deprecation steps of §4, which cover only the JSON key of #225. The other #225
  items are not part of this spec.

## Acceptance Criteria
- [x] Every `--export-json` file starts with `"schemaVersion"` (currently `"1.11"`).
- [x] `docs/schema/perfratio-export.schema.json` exists, and `cargo test` validates exports
      against it (with and without `--energy`, `--output-metric`, `--first-run=separate`,
      `--parameter-scan`, Ctrl-C and more).
- [x] `perfratio.measurements` is present; `energy` is `"unavailable"` with a reason when
      `--energy` is given on a host without readable RAPL, and `"not_requested"` without
      `--energy`.
- [x] No result contains `0` in place of an unmeasured value (fields are omitted; the schema has no default values).
- [x] `energy_samples` makes partial energy coverage visible (1.11).
- [x] `"joulex"` is removed in one step (§4); old exports with only `"joulex"`
      still import.
- [x] `doc/understanding-output.md` documents the schema, units, compatibility rules and
      the recommended cross-language flags (section "The JSON export").
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
- Schema: the exports of `tests/json_schema_tests.rs` validate against
  `docs/schema/perfratio-export.schema.json`.
