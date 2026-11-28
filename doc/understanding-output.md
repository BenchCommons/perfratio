# Understanding perfratio output

This page explains every field perfratio prints, how the numbers are computed, and how to get
reliable results. See the [README](../README.md) for installation and basic usage.

- [Anatomy of a benchmark report](#anatomy-of-a-benchmark-report)
- [Warnings](#warnings)
- [The summary and relative speed](#the-summary-and-relative-speed)
- [What runs when: setup, prepare, conclude, cleanup](#what-runs-when-setup-prepare-conclude-cleanup)
- [Benchmarking across git branches](#benchmarking-across-git-branches)
- [Reducing noise](#reducing-noise)
- [The JSON export](#the-json-export)

## Anatomy of a benchmark report

```
$ perfratio -N -w 2 -r 20 --deep-stats 'sleep 0.05' 'sleep 0.1'
Benchmark 1: sleep 0.05
  Time (mean ± σ):      58.1 ms ±   2.0 ms    [User: 1.2 ms, System: 1.5 ms, CPU: 5%, Peak Memory: 1.0 MB]
  Range (min … median … max):    55.0 ms …  57.8 ms …  62.0 ms    20 runs
  Bootstrap 95% CI:   [mean: 57.2 ms … 59.0 ms, median: 56.8 ms … 59.6 ms, σ: 1.5 ms … 2.4 ms]
  Percentiles:        [p05: 55.4 ms, p25: 56.4 ms, p75: 59.7 ms, p95: 61.2 ms (IQR 3.3 ms), geometric mean: 58.1 ms]
```

| Field | Meaning |
|---|---|
| `Time (mean ± σ)` | Arithmetic mean of the **wall-clock** time of all timed runs, and the sample **standard deviation** σ of those runs. σ describes the spread of *individual runs*; it is **not** the uncertainty of the mean (that is roughly σ/√n; see the bootstrap CI below). When perfratio runs commands through a shell, the calibrated shell start-up time is subtracted. |
| `Time (abs ≡)` | Shown instead of mean ± σ when only a single run was performed. |
| `User` / `System` | Mean CPU time per run spent in user mode and in the kernel, **summed over the process and all of its children**. They are CPU-time components and don't need to add up to the wall time (see below). |
| `CPU` | `(User + System) / mean wall time`. Above 100% means the command used several cores in parallel (e.g. 380% ≈ 3.8 cores busy); well below 100% means the command mostly waited (I/O, sleep, locks, network). |
| `Peak Memory` | Largest maximum resident set size (RSS) of a single run, as a human-readable size. On Unix it is measured per run for the benchmarked process and the processes it waited for (e.g. the command started by the shell); `--prepare`/`--setup` commands are not included. |
| `Range (min … median … max)` | Fastest, median and slowest timed run, and the number of runs (warmup runs are not counted). The median is more robust than the mean when a few runs were disturbed. |
| `Bootstrap 95% CI` | Only with `--deep-stats`: 95% confidence intervals for the **mean**, the **median** and **σ**, computed by bootstrapping (5,000 resamples). If you repeated the whole benchmark many times, about 95% of such intervals would contain the true value. |
| `Percentiles` | Only with `--deep-stats`: the 5th, 25th, 75th and 95th percentile of the run times (linear interpolation), the interquartile range `IQR = p75 − p25`, and the geometric mean. p95 shows how bad the slow runs get; the IQR is a spread measure that ignores outliers. The JSON export always contains `percentiles` and `geometric_mean`. |
| `Resources (mean)` | Only with `--resource-usage` (Unix): mean **voluntary** context switches (the process blocked: I/O, locks, sleep), **involuntary** ones (it was preempted: a busy machine or too many threads), **minor/major page faults** (major = needed disk I/O) and **block I/O operations**. Useful to explain *why* two commands differ. Per-run values are exported as `resources` in JSON. With the default shell the counters include the shell process; use `-N` to exclude it. |
| `Energy (mean)` / `Power` | Only with `-E/--energy` on Linux with readable RAPL counters: mean energy per run in joules, and average power (energy / mean time) in watts. The counters measure the **whole CPU package**, including idle and background power, not just the benchmarked process. If the counters are unavailable (other operating systems, missing permissions), perfratio prints `RAPL unprivileged/unavailable on host`; on most distributions `energy_uj` is root-only since CVE-2020-8694. |

### Why can `User + System` be lower or higher than the wall time?

- **Lower** (low `CPU %`): the process was waiting, e.g. `sleep`, disk or network I/O, lock
  contention, or waiting for another process (a server, a daemon) that does the actual work.
  CPU time spent in *other* processes is not attributed to the command.
- **Higher** (`CPU %` > 100%): several threads or child processes ran in parallel.
- **Near zero for very fast commands**: the OS accounts CPU time with limited resolution
  (clock ticks), so commands of a few milliseconds may show `0.0 ms` or rounded values.

## Warnings

| Warning | What it means / what to do |
|---|---|
| *Command took less than 5 ms to complete* | Shell start-up correction can't be more precise than this. Use `-N`/`--shell=none` to avoid the shell entirely. |
| *The first benchmarking run … was significantly slower* | Caching effects (file system cache, JIT, lazy loading). Use `--warmup N` to benchmark a warm state, or `--prepare` to reset caches before *every* run for a cold state. |
| *Statistical outliers were detected* | A few runs were far from the median (modified Z-score). Something else was using the machine; see [Reducing noise](#reducing-noise). Silence it with `--suppress-outlier-warnings`, or exclude them from the statistics with `--discard-outliers` (at most 5% of the runs, but at least one; the discarded run numbers are exported as `discarded_outliers`). |
| *Too many of the N runs … look like outliers, so none were discarded* | `--discard-outliers` refused to drop more than 5% of the runs: the distribution is probably multimodal (two clusters of run times), and dropping runs would hide that. |
| *Substantial off-CPU time detected* | Wall time ≥ 5 × CPU time: the command mostly waited. That's expected for I/O-, network- or sleep-bound commands, but it means you are measuring the environment as much as the program. Silence with `--no-off-cpu-warning` or `--suppress-warnings=off-cpu`. |
| *Ignoring non-zero exit code* | `-i`/`--ignore-exit-code` is active and some runs failed. With `--omit-failed-runs`, failed runs are excluded from the statistics. |

## The summary and relative speed

```
Summary
  sleep 0.05 ran
    1.87 ± 0.06 times faster than sleep 0.1 (109.4 ms, +51.0 ms)
      [Bootstrap t-test: t = -93.74, p = 0.0000 -> statistically significant (p < 0.01)]
```

- The fastest command (or the `--reference` command) is the baseline. Each ratio is the
  slower mean divided by the faster one, so it is always ≥ 1; the words *faster*/*slower*
  give the direction.
- The parentheses show the other command's mean and the absolute difference to the baseline
  (`+` = slower than the baseline, `−` = faster), in the unit of the largest mean, plus mean
  energy and its difference when `--energy` is available.
- The `±` is propagated from both standard deviations, assuming independent measurements:
  `σ_r = r · √((σ_a/μ_a)² + (σ_b/μ_b)²)`. Because σ describes single runs, this is a
  conservative spread of the ratio, not a confidence interval.
- With `--deep-stats`, a two-sample bootstrap test compares the per-run times. `p < 0.05`
  means a difference this large would rarely appear by chance if both commands were
  equally fast. It says nothing about whether the difference *matters*; look at the ratio
  for that. With very few runs, prefer more runs over reading much into the p-value.
- `--estimator median` (or a percentile such as `p95`) replaces the mean in the ranking, the
  ratios, the markers, the delta table and `--compare`. Use it when a few slow runs (garbage
  collection, a cold cache) should not decide the result; a percentile needs many runs. The ratio
  then shows a bootstrap interval, `1.87 [1.80, 1.95]`, at `--confidence-level` instead of `± σ`.
  `--significance-level` sets the alpha of the tests and `--bootstrap-resamples` and `--seed` the
  resampling; the defaults reproduce the same numbers every time. The settings are in
  `perfratio.statistics` of the JSON export.
- With many commands or a parameter scan, one test per command produces false positives (about
  5 in 100 at `--significance-level 0.05`). `--correction holm` (or `bonferroni`) keeps the chance
  of any false marker at that level; `--correction bh` keeps the share of false markers among the
  reported differences at that level and finds more real ones in large scans. The markers then use
  the adjusted p-value, shown as `adjusted p`, and a line under the summary title gives method,
  family size and alpha. `--comparison-family all-pairs` corrects over every pair of commands
  instead of only the comparisons with the reference. Without `--reference` the reference is the
  fastest command, which makes its comparisons lean towards small p-values; name the reference
  for a scan. The export has `statistics.correction` and `results[].p_value_adjusted`.
- A significant difference can be too small to matter, and "not significant" does not mean
  "equivalent". `--noise-threshold 2` reports changes below 2% as `within noise` whatever the
  p-value. `--equivalence-margin 5` runs an equivalence test (TOST) at the significance level: a
  command whose change is shown to lie within ±5% of the reference is `equivalent`; one that is
  neither different nor equivalent is `inconclusive`, with the number of runs per command that
  would settle it. The export has `results[].effect` (the relative change with its interval,
  Cohen's d and the common-language effect size), `classification` and
  `equivalence_p_value`; `--fail-if-regressed` ignores `within_noise` and `equivalent`.

## What runs when: setup, prepare, conclude, cleanup

| Option | Runs | Timed? |
|---|---|---|
| `-s`, `--setup CMD` | once **before all runs** of a benchmark (after the parameters are substituted); give it once for all commands or once per command | no |
| `-w`, `--warmup N` | N untimed runs of the command; with `--warmup auto`, runs until the last 5 differ by at most 1% (at most 100; the count is printed and exported as `warmup_runs`) | no |
| `-p`, `--prepare CMD` (alias `--before`) | before **each** run (warmup and timed) | no |
| the command | each timed run | **yes** |
| `-C`, `--conclude CMD` (alias `--after`) | after **each** run | no |
| `-c`, `--cleanup CMD` | once **after all runs** of a benchmark; once for all commands or once per command | no |

`PERFRATIO_RUN_ID` is a number that is unique for every run (and, with `--concurrency`, for every
instance) and the same for the `--prepare`, the command and the `--conclude` of one run;
`PERFRATIO_WORKER_ID` is the instance number in a `--concurrency` batch. Use them to name files
that parallel runs must not share (`in.$PERFRATIO_RUN_ID.txt`).

`PERFRATIO_ITERATION` is set to the run index (`0`, `1`, … or
`warmup-0`, …) for the command, `--prepare` and `--conclude`. The same value is available as
the `{iteration}` placeholder, which also works with `-N`/`--shell=none`, e.g.
`perfratio -N --prepare 'mkdir -p out/{iteration}' 'tool --out out/{iteration} input'`. It is
not expanded in `--setup`/`--cleanup`, which don't belong to a single run. The complete flow is shown in
[execution-order.png](execution-order.png).

With `--reference`, the reference command is benchmarked first, with the same
setup/prepare/conclude/cleanup handling as every other command.

## Benchmarking across git branches

**Option A: one worktree per branch** (fast; no rebuild between runs; works with uncommitted
changes):

```sh
git worktree add ../app-main main
(cd ../app-main && cargo build --release)
cargo build --release
perfratio -N -w 3 \
  -n main '../app-main/target/release/app input.txt' \
  -n HEAD './target/release/app input.txt'
```

**Option B: switch and build in `--setup`**, one benchmark per branch:

```sh
perfratio -L branch main,feature \
  --setup 'git switch {branch} && cargo build --release' \
  './target/release/app input.txt'
```

(Don't combine this with `-N`: `--setup`, `--prepare` and friends then run without a shell
too, and `&&` would be passed to `git` as a literal argument.)

Option B only works with the default (grouped) schedule: `--schedule round-robin` rejects a
parametrized `--setup`, because all setups would run before the interleaved runs and every
variant would be measured on the last branch built.

To compare against a saved baseline instead, export it once (`--export-json main.json`) and
later combine it with a live run: `perfratio --import-json main.json './target/release/app input.txt'`.

## Reducing noise

1. **Warm up or reset caches deliberately**: `--warmup N` for warm-cache numbers;
   `--prepare 'sync; echo 3 | sudo tee /proc/sys/vm/drop_caches'` (Linux) for cold-cache numbers.
2. **Skip the shell** for fast commands: `-N`/`--shell=none`.
3. **Interleave commands** with `--schedule round-robin` so slow drifts (thermal throttling,
   background jobs, turbo budget) affect all commands equally.
4. **Quiet machine**: close browsers, IDEs and sync clients; stay on **AC power**; don't run
   other benchmarks or builds in parallel.
5. **Fix the CPU frequency** on Linux:
   `sudo cpupower frequency-set -g performance`, and optionally disable turbo boost
   (`echo 1 | sudo tee /sys/devices/system/cpu/intel_pstate/no_turbo`).
6. **Pin to a core** to avoid migrations (especially on hybrid P/E-core CPUs):
   `perfratio --affinity 2 ./app` (Linux and Windows; also accepts lists like `0,2-3`).
7. **More runs**: `--min-runs`/`--runs`. The uncertainty of the mean shrinks with √n; use
   `--deep-stats` to see the confidence interval.
8. **Energy measurements** are the most sensitive to all of the above, because the RAPL
   counters include everything the CPU package does. Prefer longer runs (≥ 100 ms), a quiet
   machine, and compare commands with `--schedule round-robin`.

## The JSON export

`--export-json FILE` writes the machine-readable result. It is a versioned contract that other
tools read; the normative text is [docs/specs/output-schema-contract.md](../docs/specs/output-schema-contract.md)
and the schema is [docs/schema/perfratio-export.schema.json](../docs/schema/perfratio-export.schema.json)
(JSON Schema, draft 2020-12, for the current `schemaVersion`, currently `"1.48"`). CSV, Markdown and the other
exports are not part of the contract.

### Units and rules

Times are **seconds**, memory is **bytes**, energy is **joules**, power is **watts**.
Per-run arrays (`times`, `user_times`, `energy_joules`, `counters`, ...) are aligned: index *i* is
the same run. A value that was not requested or could not be measured is **omitted**, never
written as `0`. `null` appears only where documented: `stddev` with fewer than 2 runs, `exit_codes[i]`
for a run killed by a signal, and `custom_metrics` / `counters` entries that could not be read.

### Compatibility

`schemaVersion` is a string `"MAJOR.MINOR"` and the first key of the document. A new optional
field or a new value of an open enum is a MINOR change. A removed or renamed field, a changed unit
or type, or a changed meaning is a MAJOR change. Consumers must ignore unknown keys and accept any
MINOR of the MAJOR they support. The published schema follows this rule and allows unknown keys.
A file without `schemaVersion` is the legacy shape (`--import-json` still reads it). To check a
file:

```
pip install check-jsonschema
check-jsonschema --schemafile docs/schema/perfratio-export.schema.json result.json
```

### Top-level keys

| Key | Meaning |
|---|---|
| `schemaVersion` | Version of the contract, always first. |
| `metric` | Metric used for ranking: `wall`, `cpu`, `user`, `system`, `energy`, `memory`, `instructions`, `cycles` or `custom:<name>` (`--metric`). |
| `perfratio` | Run metadata, see below. |
| `interrupted` | Only present, as `true`, when Ctrl-C stopped the run. Per-result `runs_planned` then holds the planned run count. |
| `optimize` | Only with `--optimize`: one object per search (`var`, `min`, `max`, `metric`, `algorithm`, `budget`, `evaluations`, `best`, `best_value`, `best_command`, `evaluated`, `at_boundary`, `unimodal_warning`, and optionally `runner_up`, `p_value`, `significant`, `fixed`, `interrupted`). Every evaluated value is also a `results[]` entry. |
| `results` | One entry per benchmarked command. |

### `perfratio`: metadata and the `measurements` status block

`version` (perfratio version), `command_line` (program reduced to its file name), `started_at`
(UTC, RFC 3339), `labels` (from `--label KEY=VALUE`), `system` (`os`, `arch`, `cpus`, `cpu_model`,
`kernel`; no host or user name is exported) and `energy_backend` (`sysfs_powercap`, `perf_pmu`,
`ioreport` or `windows_emi`, only when energy works).

`metrics` (new in 1.20) lists a descriptor per metric of the export: `id`, `unit`, `direction`
(`lower_is_better`, `higher_is_better`, `neutral`), `scope`, `source`, `kind` (`measured`, `derived`, ...),
`status` (`available`, `partial`, `unsupported`, `permission_denied`, ...), `reason` and the `fields` of
`results[]` that hold the values. A metric that could not be measured is listed with its reason and
has no values, never `0`. See `docs/specs/metric-descriptors.md`.

`measurements` tells a consumer, per group, whether the numbers can be trusted. It is a property of
the host and run, identical for every result:

| Group | `measured` | `unavailable` | `not_requested` |
|---|---|---|---|
| `time`, `memory` | always | never | never |
| `energy` (`-E`) | RAPL, IOReport or EMI works | requested, but the backend failed; `energy_unavailable_reason` says why (`unsupported_platform`, `rapl_not_found`, `rapl_unreadable`, `unsupported_cpu`, `ioreport_not_found`, `ioreport_frozen`, `emi_not_found`, `emi_unreadable`, `other`) | no `--energy` |
| `resources` (`--resource-usage`) | requested | - | no flag |
| `counters` (`--counters`) | all events open; `partial` if some did not (listed in `counters_unavailable_events`) | `counters_unavailable_reason`: `unsupported_platform`, `perf_paranoid`, `no_pmu`, `other` | no flag |

The reason lists are open: treat an unknown value like `other`. `counters_requested_scope` is
present when kernel counting was asked for but denied. Check the group before you read the
fields that belong to it: when `energy` is `unavailable` there are no energy fields at all, which
is different from an energy of `0`.

### `results[]`

| Field | Meaning |
|---|---|
| `command` | The benchmarked command. |
| `mean`, `median`, `min`, `max`, `stddev` | Wall time statistics. `stddev` is `null` with one run. |
| `user`, `system` | Mean user and kernel CPU time. |
| `times`, `user_times`, `system_times` | Per-run wall, user and kernel time. |
| `memory_usage_byte` | Peak RSS per run. |
| `exit_codes` | Per-run exit code; `null` = killed by a signal. |
| `percentiles`, `geometric_mean`, `cpu_percent` | `p05`/`p25`/`p75`/`p95`, geometric mean (omitted if a run took 0 s), CPU utilization in %. |
| `relative_speed`, `relative_speed_stddev` | Ratio to the fastest result or `--reference` (1.0 = baseline). |
| `significance`, `p_value` | Difference to the reference: `significant_speedup`, `significant_regression` or `noise`, and the p-value of the bootstrap Welch test. Omitted for the reference and with fewer than 3 runs. |
| `primary_metric` | `metric`, `mean`, `stddev`, `samples` of the ranking metric (with `--metric` other than `wall`). |
| `warmup_runs` | Warmup runs done by `--warmup auto`. |
| `target_duration_seconds`, `total_sampling_seconds`, `runs_done` | With `--duration`. |
| `runs_planned` | Planned runs, when Ctrl-C stopped the benchmark. |
| `precision` | With `--target-precision`: `target`, `reached`, `confidence`, `met`. |
| `first_run` | Cold first run kept apart by `--first-run=separate`; it is in no other field. |
| `diagnostics`, `outliers` | Trend, multimodality and outlier-inflated variance; outlier count, percent and fences by `--outlier-method` for time (and memory and energy when measured). `discarded_outliers` lists runs removed by `--discard-outliers`. |
| `omitted_failed_runs`, `timed_out`, `timeout` | Runs dropped by `--omit-failed-runs`, and `--timeout` state. |
| `baseline` | With `--subtract`: the baseline command and its statistics. |
| `parameters`, `per_run_parameters` | Parameter values (`--parameter-*`) and per-value statistics for `--parameter-sample`. |
| `shell` | The shell, when commands use different ones. |
| `custom_metrics`, `custom_metrics_summary` | Values extracted with `--output-metric`, one per run (`null` = not extracted), and their `mean`, `stddev`, `median`, `min`, `max`. Unitless: perfratio does not interpret them. |
| `mean_energy_joules`, `mean_watts` | Present with at least one energy sample. |
| `energy_joules` | Per-run energy; only when every run has a sample. |
| `energy_samples` | Runs that produced a sample. If it is lower than `len(times)`, energy coverage is partial and `energy_joules` is absent. |
| `idle_watts`, `mean_net_energy_joules` | With `--energy-baseline`: idle power and the mean energy above idle. |
| `energy_domains_joules` | With `--energy-domains`: joules per RAPL domain and run. Domains overlap and are not additive. |
| `resources` | Per-run context switches, page faults and block I/O (`--resource-usage`). A column the OS cannot report is omitted. |
| `counters`, `counters_summary`, `counters_scope`, `counters_multiplexed_runs`, `counters_coverage`, `efficiency` | Hardware counters per run (`null` = not readable), their summary, the scope of unsuffixed keys (`all`, `user`, `kernel`; keys may carry `:u` or `:k`), runs with scaled counters, the lowest share of time each multiplexed event was scheduled (0..1), and `ipc`, `cpi`, `cache_miss_rate`, `branch_miss_rate`. |
| `concurrency`, `instance_times`, `throughput`, `instance_percentiles` | With `--concurrency N`: `times` then holds the makespan of each run, and `exit_codes` has N entries per run. |
| `energy_scope` | With `--energy`: `system` (the package counter covers the whole machine) or `batch` (`--concurrency`: the energy of the whole batch; it is never divided per instance). |
| `run_ids`, `instance_run_ids` | `PERFRATIO_RUN_ID` of every run (with `--concurrency`: of every instance, one row per run; the position in the row is `PERFRATIO_WORKER_ID`). |
| `leftover_processes` | Only when true: processes started by the command outlived it and were killed. |

### Measurement floor

At start perfratio runs a trivial command (`true`, or `exit 0` through a shell) about 20 times the way it
starts your command and keeps the median wall time (`perfratio.calibration.wall_floor`), its spread
(`wall_floor_mad`) and the peak memory (`rss_floor`). A command is *below resolution* when its mean is
under 3 floors or its 95% CI reaches the floor; spawn cost and timer resolution then matter as much as
the command. The result carries `quality.below_resolution: true` and a `measurement_floor` check, and the
numbers stay. `perfratio -N true` is below resolution, `perfratio 'sleep 0.1'` is not. `--calibrate=off`
skips the measurement; `--below-floor fail` exits with code 6. See
[docs/specs/measurement-floor.md](../docs/specs/measurement-floor.md).

### Comparing implementations across languages

Use the same flags for every language so that startup and runtime effects are handled the same way:

```
perfratio -N --warmup auto --first-run=separate \
          --output-metric 'algo_ns=ALGO_NS=(\d+)' \
          --label lang=<lang> --label algorithm=<name> \
          --export-json result.json '<run command>'
```

`-N` keeps the shell start-up out of the numbers, `--warmup auto` lets JIT and VM runtimes reach a
steady state, `--first-run=separate` keeps the cold start in `first_run`, `--output-metric` lets the
program time only its algorithm, and `--label` carries the language and algorithm inside the file.
Use `-E` only on bare-metal Linux with readable RAPL and check `perfratio.measurements.energy`
before you read an energy field. See [docs/CROSS_LANGUAGE.md](../docs/CROSS_LANGUAGE.md).

## Graceful Interruption (Ctrl-C)

- **First Ctrl-C**: Stops starting new benchmark runs. Any runs completed so far for the active benchmark are preserved, and partial statistics, the relative-speed summary, and all exports are written (with exit code 130 and `"interrupted": true` in JSON).
- **Second Ctrl-C**: Forces immediate termination.
