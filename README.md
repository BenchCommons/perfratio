# perfratio

[![CICD](https://github.com/BenchCommons/perfratio/actions/workflows/CICD.yml/badge.svg)](https://github.com/BenchCommons/perfratio/actions/workflows/CICD.yml)

perfratio is a command-line benchmarking tool that measures **time, peak memory and energy**
for the same command and compares the results with statistics. It is the measurement engine
of [BenchCommons](https://github.com/BenchCommons). It owns the versioned JSON output
contract that other BenchCommons tools consume. Every JSON export starts with a `schemaVersion` field (see [JSON export](#json-export-and-the-schemaversion-contract)).

perfratio was formerly called `joulex`.

## How it differs from hyperfine

perfratio is a fork of [`sharkdp/hyperfine`](https://github.com/sharkdp/hyperfine) by
David Peter and contributors. The command-line model (shell commands, warmup, parameter scans,
exports) comes from hyperfine, and the credit for it belongs upstream. The statistical ideas
borrow from [`Gabriella439/bench`](https://github.com/Gabriella439/bench). perfratio adds:

- Energy (`-E`, `--energy`) from RAPL on Linux, and peak memory and CPU utilization for every run.
- Deep statistics (`--deep-stats`): bootstrapped confidence intervals and hypothesis tests.
- A regression gate against an earlier export (`--compare`, `--fail-if-regressed`).
- Cross-language helpers: `--warmup auto`, `--first-run`, `--output-metric`, `--target-precision`.
- Time-budgeted sampling (`--duration 5s`), a primary metric (`--metric`), a multi-dimensional `--delta-table`, and significance indicators on speedups and regressions.
- Scripting helpers: `-P n 10 1000000 --parameter-step-n 5 --parameter-step-log` (geometric scans), `perfratio -` (one command per line from stdin), `--raw-ratios` (only the ratios on stdout) and `--command-name=` (automatic names).
- `--pareto wall,energy,memory` marks the commands on the Pareto frontier; `--constraint 'memory <= 256MiB'` and `--objective minimize:energy` pick the best candidate under limits.
- A versioned JSON export for tools that consume the results (`schemaVersion`).

Run `perfratio --help` for every option. perfratio can read hyperfine and joulex JSON
files (`--import-json`).

## Install

perfratio is not yet on crates.io or Homebrew. Both are planned (see
[#93](https://github.com/BenchCommons/perfratio/issues/93)).

**From source** (Rust 1.88 or newer):

```sh
git clone https://github.com/BenchCommons/perfratio
cd perfratio
cargo install --path .
```

**GitHub release assets.** Each release at
<https://github.com/BenchCommons/perfratio/releases> contains one archive per target, named
`perfratio-v<version>-<target>.tar.gz` (`.zip` on Windows), and Debian packages. Unpack the
archive and put the `perfratio` binary on your `PATH`.

## Quick start

Benchmark one command, or compare several:

```console
$ perfratio --style basic -N --runs 5 'sleep 0.1' 'sleep 0.2'
Benchmark 1: sleep 0.1
  Time (mean ± σ):     108.8 ms ±   2.6 ms    [User: 0.7 ms, System: 1.2 ms, CPU: 2%, Peak Memory: 960.0 KB]
  Range (min … median … max):   104.7 ms … 109.4 ms … 111.3 ms    5 runs

Benchmark 2: sleep 0.2
  Time (mean ± σ):     212.5 ms ±   4.6 ms    [User: 0.9 ms, System: 1.3 ms, CPU: 1%, Peak Memory: 960.0 KB]
  Range (min … median … max):   207.1 ms … 210.7 ms … 218.2 ms    5 runs

Summary
  sleep 0.1 ran
    1.95 ± 0.06 times faster than sleep 0.2 (212.5 ms, +103.7 ms)
```

(The off-CPU warnings that perfratio prints for `sleep` are left out here.) Commands run
through a shell by default. `-N` (`--shell=none`) skips it, which matters for commands
under about 5 ms. To pass an exact argument vector, put it after `--`:

```sh
perfratio -w 3 -- grep -E 'a|b' "my file.txt"
```

Common options:

| Option | Purpose |
|---|---|
| `-r, --runs <NUM>` | Run exactly NUM times. By default perfratio picks the count (at least 10 runs). |
| `-w, --warmup <NUM\|auto>` | Warmup runs before timing. `auto` stops once the last 5 timings differ by at most 1%. |
| `-p, --prepare <CMD>` | Run CMD before each timing run, for example to clear caches. |
| `-s, --setup <CMD>` / `-c, --cleanup <CMD>` | Run CMD once before or after all runs of a command. |
| `-P`, `-L`, `-F` | Parameter scans over a range, a list or a file. |
| `--optimize <VAR> <MIN> <MAX>` | Search a parameter for the value that minimizes `--metric` (golden-section search, assumes a unimodal metric; `--optimize-budget <N>` caps the evaluations; `-D <DELTA>` allows fractional values; the best candidates are re-run once and pooled, `--optimize-confirm <N>`; the HTML report marks the best value). |
| `-e, --export <FILE>` | Export results. The format follows the file extension. |
| `--deep-stats` | Bootstrapped confidence intervals and hypothesis tests. |
| `--estimator <mean\|median\|pNN>`, `--confidence-level`, `--significance-level`, `--bootstrap-resamples`, `--seed` | Choose the central value behind ranking, ratios and tests (default `mean`), and the levels and bootstrap settings of intervals and tests (see `docs/specs/statistics-settings.md`). |

> [!WARNING]
> Values from `-P`, `-L` and `-F/--parameter-file` are inserted into the command **verbatim**,
> not shell-escaped. A value such as `a; rm -rf ~` runs as shell code. Prefer `-N`, quote the
> placeholder, or validate values that come from outside your control.

[doc/understanding-output.md](doc/understanding-output.md) explains each output field and
how to reduce noise. The manual page is [doc/perfratio.1](doc/perfratio.1).

## Energy and memory

**Memory.** Peak memory (RSS) of each run is always measured and shown as `Peak Memory`.
`--resource-usage` adds context switches, page faults and block I/O per run. Windows reports
page faults and I/O operation counts only; the other counters show as `n/a` ([spec](docs/specs/windows-resource-usage.md)).
`--counters` (Linux) adds hardware counters (instructions, cycles, cache and branch misses), IPC and the effective CPU frequency per run, and with `--counters=default,sw` CPU migrations and context switches (a warning points to `--affinity` when migrations slow runs down); see `docs/specs/hardware-counters.md`, `docs/specs/software-events-migrations.md` and `docs/specs/effective-frequency.md`. `--scope user|kernel|all|split` and per-event `:u` / `:k` modifiers (`--counters=instructions:u,instructions:k`) choose the privilege level; see `docs/specs/counter-scope-modifiers.md`.
`--topdown` (Linux x86_64; Intel Ice Lake or later, AMD Zen 4 or later) adds Top-down level 1: the share of pipeline slots that retire work or are lost to bad speculation, frontend stalls or backend stalls. The largest stall class is a hypothesis about the bottleneck, not a diagnosis; see `docs/specs/topdown-tma.md`.
`--simd` (Linux, Intel and AMD Zen 4/5) adds a floating-point breakdown (scalar, 128-, 256- and 512-bit) and a vectorization ratio per run; see `docs/specs/simd-breakdown.md`.

**Energy.** `-E, --energy` measures energy in joules and average power in watts. It reads RAPL
counters through Linux powercap sysfs, so it needs **bare-metal Linux with a readable
`energy_uj`** (`/sys/class/powercap/intel-rapl`). Containers and virtual machines usually cannot
read it, and recent kernels restrict `energy_uj` to root. macOS (Apple Silicon, IOReport) and
Windows (Energy Meter Interface) have their own backends; see
[docs/specs/energy-macos-windows.md](docs/specs/energy-macos-windows.md). Continuous
integration covers time and memory only.

Without a readable counter, perfratio says so instead of printing a number. This is real output from
a macOS machine:

```console
$ perfratio -N -E --resource-usage --style basic --runs 3 'sleep 0.1'
Benchmark 1: sleep 0.1
  Time (mean ± σ):     107.6 ms ±   1.1 ms    [User: 0.7 ms, System: 1.0 ms, CPU: 2%, Peak Memory: 960.0 KB]
  Range (min … median … max):   106.6 ms … 107.4 ms … 108.8 ms    3 runs
  Resources (mean):   ctx-sw 0 vol / 8.3 invol · faults 123 minor / 1 major · I/O 0 in / 0 out blocks
  Energy:             RAPL unprivileged/unavailable on host
```

`--energy-backend <auto|sysfs|perf|ioreport|emi>` (or `PERFRATIO_ENERGY_BACKEND`) picks the source (`ioreport` on macOS, `emi` on Windows; `auto` picks the native one). On Linux:
`auto`, the default, uses sysfs powercap and falls back to the perf power PMU
(`power/energy-pkg`) when `energy_uj` is unreadable. `--energy-baseline <DURATION>` measures
idle power first and also reports net energy (measured minus idle power times run time). It is
off by default so that `--energy` does not add a delay; `--energy-baseline 2s` is a good value
when you compare commands of different duration.
`--metric energy` ranks by energy and turns on `--energy`.

`--energy-domains` adds a per-run breakdown (`package`, `core`, `uncore`, `dram`, `psys`) to the
console and to `energy_domains_joules` in the JSON export. The domains overlap, so the total stays
the package energy ([spec](docs/specs/rapl-domains.md)). If `energy_uj` is root-only, give your user
read access. The `chmod` below resets on reboot; a udev rule persists. Both let any local user
read the power signal (PLATYPUS, CVE-2020-8694), so use them only on machines you control:

```sh
sudo chmod o+r /sys/class/powercap/intel-rapl*/*energy_uj /sys/class/powercap/intel-rapl*/*/energy_uj
# or persistently, in /etc/udev/rules.d/99-rapl.rules:
SUBSYSTEM=="powercap", ACTION=="add|change", RUN+="/bin/sh -c 'chmod o+r /sys%p/energy_uj'"
```

`--delta-table` prints one table per command against the reference, with a row for each measured
dimension (wall time, peak memory, energy, power, CPU time): mean ± σ, relative delta with its
95% confidence interval, and the p-value. `--metric <wall|cpu|user|system|energy|memory|instructions|cycles>` sets the
primary metric for statistics, comparison and sorting. `instructions` and `cycles` turn on
`--counters` (Linux). Markdown, AsciiDoc and Org tables get IPC, CPI and miss-rate columns when
counters were collected.

`--expect-output-sha256 HEX` and `--validate CMD` check that each run produced the right result, so a command cannot look fast because it did less work; `--on-invalid fail|omit|keep` picks what happens to a wrong run ([spec](docs/specs/workload-validation.md)).

`--check-system` reports conditions that add noise (frequency governor, turbo boost, load,
power source). `--check-system=strict` aborts with exit code 4 if a check fails.

## Compare runs and gate regressions in CI

Export a baseline on your main branch, then compare later runs against it:

```sh
perfratio -n work --export-json baseline.json 'sleep 0.1'
perfratio -n work --compare baseline.json --fail-if-regressed 5% 'sleep 0.2'
```

Commands are matched by name, so give both runs the same command or `-n` name. A real run of
the second command (`--style basic -N --suppress-warnings all --runs 8` added):

```console
Benchmark 1: work
  Time (mean ± σ):     209.1 ms ±   3.0 ms    [User: 0.6 ms, System: 0.7 ms, CPU: 1%, Peak Memory: 976.0 KB]
  Range (min … median … max):   206.2 ms … 208.3 ms … 215.5 ms    8 runs

Comparison with baseline.json
  Command        Baseline         Current    Change  Significance
  work     111.4 ms ± 2.9  209.1 ms ± 3.0  +87.7% ▲  p < 0.001     REGRESSION

Error: 1 benchmark(s) regressed compared with the baseline ('--fail-if-regressed')
```

The exit code is 3. A benchmark counts as regressed only if it is slower than the threshold
**and** the difference is statistically significant (p < 0.05 on the per-run times), so noise
alone does not fail the gate. When the command did not get slower, the same gate prints
`-2.3%  p = 0.401  ~` and exits 0. `--export-diff-markdown <FILE>` writes the table for a pull
request comment. A GitHub Actions example is in [doc/ci.md](doc/ci.md).

Every benchmark also gets a measurement quality report (`results[].quality` in the JSON, a summary in the terminal when a check warns or fails): sample sufficiency, CI precision, outliers, trend, clusters, counter and energy coverage, workload validation. `--fail-on-quality with_caveats` exits with code 5 if a measurement is `unreliable`, next to `--fail-if-regressed` ([spec](docs/specs/measurement-quality.md)).

## JSON export and the `schemaVersion` contract

`--export-json <FILE>` (or `-e results.json`) writes the summary statistics and every run's
timings. perfratio owns this output, and consumers such as `tools-benchmark` read it.
The current version is `"1.13"`. The `perfratio` key holds run metadata and `perfratio.measurements` says whether time, memory, energy and resource counters were measured, unavailable or not requested. The legacy top-level `joulex` key was removed in `schemaVersion` 1.0: read `perfratio` instead ([docs/MIGRATION.md](docs/MIGRATION.md)).

The contract is a top-level `"schemaVersion": "MAJOR.MINOR"` string. Consumers must ignore
unknown keys and accept any MINOR version of the MAJOR version they support. A removed or
renamed field, or a changed unit, bumps MAJOR.

The contract is specified in [docs/specs/output-schema-contract.md](docs/specs/output-schema-contract.md)
(see [#240](https://github.com/BenchCommons/perfratio/issues/240)). Read that document for the
field list. Files without `schemaVersion` are the legacy shape, and `--import-json` still
reads them.

- [docs/schema/perfratio-export.schema.json](docs/schema/perfratio-export.schema.json) is the
  JSON Schema (draft 2020-12) of the current version. `cargo test` validates exports of many
  flag combinations against it, so it cannot drift from the output.
- [doc/understanding-output.md](doc/understanding-output.md#the-json-export) explains every field,
  the units and the `perfratio.measurements` status block.
- `energy_samples` (per result) says how many runs produced an energy sample.

## Recommended flags for cross-language runs

When you compare the same algorithm across many languages
([#240](https://github.com/BenchCommons/perfratio/issues/240)), startup cost and
warmup effects differ a lot between runtimes. Use:

| Flag | Why |
|---|---|
| `--warmup auto` | Warms up each command until timings are stable (at most 100 runs), so JIT-compiled and interpreted languages are not penalized by a fixed count. |
| `--first-run=separate` | Reports the cold first run on its own line (`first_run` in the JSON) and keeps it out of the statistics. |
| `--output-metric NAME=REGEX` | Reads a number from the command's stdout, or stderr with `--output-metric-stderr` (first capture group of the last match). Have the program time its algorithm and print it, so startup is separated from algorithm time. |
| `--duration <DURATION>` | Samples each command until the summed run time reaches DURATION (at least 3 runs), for example `--duration 5s`. Conflicts with `--runs` and `--target-precision`. |
| `--target-precision <PCT>` | Runs until the 95% confidence interval of the mean is within PCT of the mean, bounded by `--min-runs`, `--max-runs` and `--max-benchmarking-time`. Fast commands finish early. |

Example (real output; the program prints `algo_ms=12.5` and sleeps 50 ms):

```console
$ perfratio --style basic -N --suppress-warnings all --warmup auto --first-run=separate \
    --target-precision 5% --output-metric 'algo_ms=algo_ms=([0-9.]+)' \
    -- sh -c 'echo algo_ms=12.5; sleep 0.05'
Benchmark 1: sh -c 'echo algo_ms=12.5; sleep 0.05'
  Cold (warmup 1):      65.6 ms
  Time (mean ± σ):      73.6 ms ±  15.3 ms    [User: 2.6 ms, System: 5.9 ms, CPU: 12%, Peak Memory: 2.4 MB]
  Range (min … median … max):    60.1 ms …  68.8 ms … 153.1 ms    69 runs (±5.0% @95%, target 5%)
  algo_ms (mean ± σ):      12.500 ±    0.000
  Warmup (auto):      100 runs, last 5 within 12.7% (not stable)
```

On this busy laptop `--warmup auto` hit its 100-run limit without the timings settling, and
perfratio also printed a warning saying so. Other useful flags: `--affinity <CPUS>`,
`--priority <POLICY>`, `--discard-outliers`, `--timeout <DURATION>`,
`--concurrency <N>` (N simultaneous instances per run, `{param}` allowed for a scaling curve, see [docs/specs/concurrency.md](docs/specs/concurrency.md)).

## Platform support

The CI builds and tests these targets and attaches them to releases. Features differ by platform.

| Feature | Linux | macOS | Windows |
|---|---|---|---|
| Time, CPU time, peak memory | yes | yes | yes |
| Energy (`--energy`) | bare metal RAPL, via sysfs `energy_uj` or the perf power PMU | Apple Silicon via IOReport (unavailable where CPU counters are frozen, e.g. macOS 27) | Energy Meter Interface (EMI), needs a meter driver and usually admin |
| `--resource-usage` | yes | yes | partial (page faults, I/O operations; no context switches) |
| `--affinity` | yes | `p-cores`/`e-cores` only (scheduling preference) | yes |
| `--priority realtime` | yes (`SCHED_FIFO`) | nice -20 + foreground role (not real-time) | yes |
| `--check-system` | yes | yes (fewer checks) | yes (power source, power plan, CPU load, frequency limit) |

Release targets: `x86_64`, `aarch64`, `i686` and `arm` Linux (gnu and musl), `x86_64` and
`aarch64` macOS, and `x86_64`, `i686` and `aarch64` Windows (MSVC).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for the local checks (`cargo fmt`,
`cargo clippy -- -D warnings`) and the pull request workflow. Feature specs live in
[docs/specs/](docs/specs/).

## License

perfratio is dual-licensed under the MIT License and the Apache License 2.0. See
[LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE). The code derived from
hyperfine is copyright David Peter and the hyperfine contributors.
