# Changelog

perfratio is a fork of [hyperfine](https://github.com/sharkdp/hyperfine) (via joulex). This file covers perfratio first; the unchanged upstream hyperfine history is kept at the bottom.

Migrating from hyperfine or joulex: see [docs/MIGRATION.md](docs/MIGRATION.md). Measuring one algorithm in several languages: see [docs/CROSS_LANGUAGE.md](docs/CROSS_LANGUAGE.md).

## Unreleased / 1.0.0

Generated from the merged pull requests (#1 to #284). Pull requests that bump dependencies or GitHub Actions are listed under Internal.

### Breaking changes

- Ambiguous alias dropped in round-robin mode when computing the common run count ([#145](https://github.com/BenchCommons/perfratio/pull/145), issue [#102](https://github.com/BenchCommons/perfratio/issues/102), [#111](https://github.com/BenchCommons/perfratio/issues/111))
- Project renamed from joulex to perfratio (crate, binary, man page, `$PERFRATIO_ITERATION`); see docs/MIGRATION.md ([#224](https://github.com/BenchCommons/perfratio/pull/224))
- Removed the `joulex` shell completions, the `joulex.1` man page and the `joulex_eta` progress key; renamed `$HYPERFINE_RANDOMIZED_ENVIRONMENT_OFFSET` to `$PERFRATIO_RANDOMIZED_ENVIRONMENT_OFFSET`. `$JOULEX_ITERATION`, `$HYPERFINE_ITERATION` and `$JOULEX_SYSTEM_CHECK_ROOT` still work but warn and are removed in the release after 1.0 ([#263](https://github.com/BenchCommons/perfratio/pull/263), issue [#225](https://github.com/BenchCommons/perfratio/issues/225))
- The JSON export no longer has the top-level `joulex` key; read `perfratio` instead. Exports start with `"schemaVersion"` ([#262](https://github.com/BenchCommons/perfratio/pull/262), issue [#240](https://github.com/BenchCommons/perfratio/issues/240))

### Features

- Add --parameter-file flag to read parameter lines from file ([#9](https://github.com/BenchCommons/perfratio/pull/9), issue [#5](https://github.com/BenchCommons/perfratio/issues/5))
- Add --import-json flag to compare and convert saved benchmarks ([#10](https://github.com/BenchCommons/perfratio/pull/10), issue [#6](https://github.com/BenchCommons/perfratio/issues/6))
- Export detailed user_times and system_times in JSON ([#14](https://github.com/BenchCommons/perfratio/pull/14), issue [#11](https://github.com/BenchCommons/perfratio/issues/11))
- Add --suppress-outlier-warnings flag ([#15](https://github.com/BenchCommons/perfratio/pull/15), issue [#13](https://github.com/BenchCommons/perfratio/issues/13))
- Implement round-robin interleaved execution ([#16](https://github.com/BenchCommons/perfratio/pull/16), issue [#12](https://github.com/BenchCommons/perfratio/issues/12))
- Annotate relative speed table when using --sort=command with --reference ([#20](https://github.com/BenchCommons/perfratio/pull/20), issue [#19](https://github.com/BenchCommons/perfratio/issues/19))
- Add CPU utilization indicator and off-CPU warning ([#23](https://github.com/BenchCommons/perfratio/pull/23), issue [#21](https://github.com/BenchCommons/perfratio/issues/21))
- Allow multiple parameter scans and combination with lists/files ([#73](https://github.com/BenchCommons/perfratio/pull/73), issue [#45](https://github.com/BenchCommons/perfratio/issues/45))
- Add --omit-failed-runs to exclude failed runs from summary statistics ([#99](https://github.com/BenchCommons/perfratio/pull/99), issue [#76](https://github.com/BenchCommons/perfratio/issues/76))
- Add short flag counterparts for style, sort, show-output, output, and input ([#115](https://github.com/BenchCommons/perfratio/pull/115), issue [#78](https://github.com/BenchCommons/perfratio/issues/78))
- Add --ignore-exit-code alias for --ignore-failure ([#118](https://github.com/BenchCommons/perfratio/pull/118), issue [#77](https://github.com/BenchCommons/perfratio/issues/77))
- Color --help and usage errors with clap Styles ([#120](https://github.com/BenchCommons/perfratio/pull/120), issue [#70](https://github.com/BenchCommons/perfratio/issues/70))
- Complete executables for the command in fish ([#140](https://github.com/BenchCommons/perfratio/pull/140), issue [#85](https://github.com/BenchCommons/perfratio/issues/85))
- Add an {iteration} placeholder that also works without a shell ([#149](https://github.com/BenchCommons/perfratio/pull/149), issue [#94](https://github.com/BenchCommons/perfratio/issues/94))
- Show the median, and percentiles with --deep-stats ([#155](https://github.com/BenchCommons/perfratio/pull/155), issue [#82](https://github.com/BenchCommons/perfratio/issues/82))
- Show absolute means and differences next to the ratios ([#156](https://github.com/BenchCommons/perfratio/pull/156), issue [#91](https://github.com/BenchCommons/perfratio/issues/91))
- Add --discard-outliers to exclude outlier runs from the statistics ([#158](https://github.com/BenchCommons/perfratio/pull/158), issue [#62](https://github.com/BenchCommons/perfratio/issues/62))
- Add run metadata, labels and relative speed to JSON/CSV ([#159](https://github.com/BenchCommons/perfratio/pull/159), issue [#97](https://github.com/BenchCommons/perfratio/issues/97))
- Report per-run OS resource counters with --resource-usage ([#160](https://github.com/BenchCommons/perfratio/pull/160), issue [#47](https://github.com/BenchCommons/perfratio/issues/47))
- Add percentiles and geometric mean to the JSON export ([#162](https://github.com/BenchCommons/perfratio/pull/162), issue [#82](https://github.com/BenchCommons/perfratio/issues/82))
- Add --affinity to pin benchmarked processes to CPUs ([#163](https://github.com/BenchCommons/perfratio/pull/163), issue [#63](https://github.com/BenchCommons/perfratio/issues/63))
- Add per-run Markdown, Org and AsciiDoc tables ([#164](https://github.com/BenchCommons/perfratio/pull/164), issue [#84](https://github.com/BenchCommons/perfratio/issues/84))
- Allow --setup and --cleanup once per command, like --prepare ([#165](https://github.com/BenchCommons/perfratio/pull/165), issue [#83](https://github.com/BenchCommons/perfratio/issues/83))
- Add --warmup auto to warm up until the timings are stable ([#166](https://github.com/BenchCommons/perfratio/pull/166), issue [#75](https://github.com/BenchCommons/perfratio/issues/75))
- Add --expand-used-parameters to skip unused parameter combinations ([#172](https://github.com/BenchCommons/perfratio/pull/172), issue [#58](https://github.com/BenchCommons/perfratio/issues/58))
- Self-contained HTML report (--export-html) ([#174](https://github.com/BenchCommons/perfratio/pull/174), issue [#60](https://github.com/BenchCommons/perfratio/issues/60))
- Accept the command after -- as an exact argv ([#175](https://github.com/BenchCommons/perfratio/pull/175), issue [#52](https://github.com/BenchCommons/perfratio/issues/52))
- Recover timing data and export partial results on Ctrl-C ([#176](https://github.com/BenchCommons/perfratio/pull/176), issue [#89](https://github.com/BenchCommons/perfratio/issues/89))
- --first-run include|separate|discard to report the cold run separately ([#179](https://github.com/BenchCommons/perfratio/pull/179), issue [#55](https://github.com/BenchCommons/perfratio/issues/55))
- Detect trends, multimodality and outlier-inflated variance ([#181](https://github.com/BenchCommons/perfratio/pull/181), issue [#54](https://github.com/BenchCommons/perfratio/issues/54))
- --priority normal|high|idle|realtime for benchmarked processes ([#184](https://github.com/BenchCommons/perfratio/pull/184), issue [#80](https://github.com/BenchCommons/perfratio/issues/80))
- --show-output-on-failure prints a failed run's output ([#185](https://github.com/BenchCommons/perfratio/pull/185), issue [#96](https://github.com/BenchCommons/perfratio/issues/96))
- --check-system reports noise sources before benchmarking ([#186](https://github.com/BenchCommons/perfratio/pull/186), issue [#64](https://github.com/BenchCommons/perfratio/issues/64))
- CI regression gate (--compare, --fail-if-regressed, --export-diff-markdown) ([#191](https://github.com/BenchCommons/perfratio/pull/191), issue [#61](https://github.com/BenchCommons/perfratio/issues/61))
- Per-run parameters (--parameter-sample, --aggregate-parameter-runs) ([#193](https://github.com/BenchCommons/perfratio/pull/193), issue [#57](https://github.com/BenchCommons/perfratio/issues/57), [#74](https://github.com/BenchCommons/perfratio/issues/74))
- Record per-run parameter values and per-value stats in exports ([#194](https://github.com/BenchCommons/perfratio/pull/194), issue [#192](https://github.com/BenchCommons/perfratio/issues/192))
- --precision N|auto for times in human-readable output ([#196](https://github.com/BenchCommons/perfratio/pull/196), issue [#65](https://github.com/BenchCommons/perfratio/issues/65))
- Adaptive run count with --target-precision ([#198](https://github.com/BenchCommons/perfratio/pull/198), issue [#50](https://github.com/BenchCommons/perfratio/issues/50))
- Record --target-precision in the JSON export ([#200](https://github.com/BenchCommons/perfratio/pull/200), issue [#199](https://github.com/BenchCommons/perfratio/issues/199))
- --shell per command (compare shells, mix shell and none) ([#201](https://github.com/BenchCommons/perfratio/pull/201), issue [#67](https://github.com/BenchCommons/perfratio/issues/67))
- --subtract CMD to measure a delta against a baseline command ([#203](https://github.com/BenchCommons/perfratio/pull/203), issue [#56](https://github.com/BenchCommons/perfratio/issues/56))
- Add --timeout <DURATION> per benchmark run ([#204](https://github.com/BenchCommons/perfratio/pull/204), issue [#51](https://github.com/BenchCommons/perfratio/issues/51))
- Propagate baseline uncertainty and subtract energy ([#205](https://github.com/BenchCommons/perfratio/pull/205), issue [#202](https://github.com/BenchCommons/perfratio/issues/202))
- Add Windows power, power plan, and CPU load checks ([#206](https://github.com/BenchCommons/perfratio/pull/206), issue [#188](https://github.com/BenchCommons/perfratio/issues/188))
- Add --until and --until-stderr to measure startup readiness ([#207](https://github.com/BenchCommons/perfratio/pull/207), issue [#79](https://github.com/BenchCommons/perfratio/issues/79))
- Exclude fork/spawn overhead on Linux via post-fork timing ([#208](https://github.com/BenchCommons/perfratio/pull/208), issue [#81](https://github.com/BenchCommons/perfratio/issues/81))
- Support custom output metrics with --output-metric ([#209](https://github.com/BenchCommons/perfratio/pull/209), issue [#90](https://github.com/BenchCommons/perfratio/issues/90))
- Improve progress UI with ticking elapsed time, live estimates, and iteration count ([#210](https://github.com/BenchCommons/perfratio/pull/210), issue [#66](https://github.com/BenchCommons/perfratio/issues/66))
- JSON export `schemaVersion` (now 1.2) and `perfratio.measurements` status block ([#262](https://github.com/BenchCommons/perfratio/pull/262), issue [#240](https://github.com/BenchCommons/perfratio/issues/240))
- Add `--metric` to choose the primary metric for statistics, comparison and sorting ([#226](https://github.com/BenchCommons/perfratio/pull/226), issue [#48](https://github.com/BenchCommons/perfratio/issues/48))
- Add `--duration` for time-budgeted sampling ([#269](https://github.com/BenchCommons/perfratio/pull/269), issue [#216](https://github.com/BenchCommons/perfratio/issues/216))
- Add `--delta-table` to compare every measured dimension with the reference ([#274](https://github.com/BenchCommons/perfratio/pull/274), issue [#212](https://github.com/BenchCommons/perfratio/issues/212))
- Add `--energy-baseline` for idle power and net energy ([#275](https://github.com/BenchCommons/perfratio/pull/275), issue [#59](https://github.com/BenchCommons/perfratio/issues/59))
- Add `--energy-backend auto|sysfs|perf` and a perf PMU RAPL backend ([#272](https://github.com/BenchCommons/perfratio/pull/272), issue [#223](https://github.com/BenchCommons/perfratio/issues/223))
- Significance indicators for speedups, regressions and noise ([#270](https://github.com/BenchCommons/perfratio/pull/270), issue [#215](https://github.com/BenchCommons/perfratio/issues/215))
- Tukey's fences outlier detection with counts and percentages, `--outlier-method` ([#266](https://github.com/BenchCommons/perfratio/pull/266), issue [#214](https://github.com/BenchCommons/perfratio/issues/214))
- Add `--output-metric-stderr` ([#265](https://github.com/BenchCommons/perfratio/pull/265), issue [#90](https://github.com/BenchCommons/perfratio/issues/90))
- Minimal pre-stable library API, `perfratio::api` ([#277](https://github.com/BenchCommons/perfratio/pull/277), issue [#92](https://github.com/BenchCommons/perfratio/issues/92))
- `--check-system` reports PMU permissions on Linux ([#268](https://github.com/BenchCommons/perfratio/pull/268), issue [#217](https://github.com/BenchCommons/perfratio/issues/217))
- Building blocks, not yet wired to a CLI flag: derived IPC, CPI and miss-rate metrics ([#267](https://github.com/BenchCommons/perfratio/pull/267), issue [#213](https://github.com/BenchCommons/perfratio/issues/213)) and PMU multiplexing detection and scaling ([#271](https://github.com/BenchCommons/perfratio/pull/271), issue [#219](https://github.com/BenchCommons/perfratio/issues/219))

### Bug fixes

- Ensure consistent parameter columns across benchmark commands ([#18](https://github.com/BenchCommons/perfratio/pull/18), issue [#17](https://github.com/BenchCommons/perfratio/issues/17))
- Avoid inf and NaN in markup relative speed when benchmark times are zero ([#100](https://github.com/BenchCommons/perfratio/pull/100), issue [#95](https://github.com/BenchCommons/perfratio/issues/95))
- Correct deep-stats hypothesis testing for zero-variance and identical samples ([#116](https://github.com/BenchCommons/perfratio/pull/116), issue [#101](https://github.com/BenchCommons/perfratio/issues/101))
- Use pointer equality for reference comparison in relative speed ([#117](https://github.com/BenchCommons/perfratio/pull/117), issue [#109](https://github.com/BenchCommons/perfratio/issues/109))
- Port small upstream wording and typo fixes ([#119](https://github.com/BenchCommons/perfratio/pull/119), issue [#87](https://github.com/BenchCommons/perfratio/issues/87))
- Align exit codes and preserve runs in --omit-failed-runs ([#125](https://github.com/BenchCommons/perfratio/pull/125), issue [#105](https://github.com/BenchCommons/perfratio/issues/105))
- Clear clippy warnings on the MSRV and deny new ones in CI ([#126](https://github.com/BenchCommons/perfratio/pull/126), issue [#35](https://github.com/BenchCommons/perfratio/issues/35))
- Document unsafe blocks and return getrusage errors ([#127](https://github.com/BenchCommons/perfratio/pull/127), issue [#33](https://github.com/BenchCommons/perfratio/issues/33))
- Natural subject-first layout for --sort=command with --reference ([#128](https://github.com/BenchCommons/perfratio/pull/128), issue [#108](https://github.com/BenchCommons/perfratio/issues/108))
- Bound Cartesian product across parameter combinations to prevent OOM ([#133](https://github.com/BenchCommons/perfratio/pull/133), issue [#104](https://github.com/BenchCommons/perfratio/issues/104))
- Escape control characters in imported JSON results (CWE-150) ([#134](https://github.com/BenchCommons/perfratio/pull/134), issue [#24](https://github.com/BenchCommons/perfratio/issues/24))
- Inline format argument flagged by clippy on the MSRV ([#135](https://github.com/BenchCommons/perfratio/pull/135))
- Prevent RAPL double-counting by filtering package vs psys domains ([#136](https://github.com/BenchCommons/perfratio/pull/136), issue [#106](https://github.com/BenchCommons/perfratio/issues/106))
- Validate imported JSON results ([#137](https://github.com/BenchCommons/perfratio/pull/137), issue [#27](https://github.com/BenchCommons/perfratio/issues/27))
- Escape commands per markup format in table exports ([#138](https://github.com/BenchCommons/perfratio/pull/138), issue [#25](https://github.com/BenchCommons/perfratio/issues/25))
- Reject parametrized --setup/--cleanup in round-robin mode ([#142](https://github.com/BenchCommons/perfratio/pull/142), issue [#103](https://github.com/BenchCommons/perfratio/issues/103))
- Write exports atomically and stop truncating them at startup ([#144](https://github.com/BenchCommons/perfratio/pull/144), issue [#36](https://github.com/BenchCommons/perfratio/issues/36))
- Measure per-run resource usage with wait4 on Unix ([#150](https://github.com/BenchCommons/perfratio/pull/150), issue [#46](https://github.com/BenchCommons/perfratio/issues/46))
- Respect time unit in deep-stats output and correct help text ([#151](https://github.com/BenchCommons/perfratio/pull/151), issue [#107](https://github.com/BenchCommons/perfratio/issues/107))
- Find the --reference result by index, not position 0 ([#152](https://github.com/BenchCommons/perfratio/pull/152), issue [#26](https://github.com/BenchCommons/perfratio/issues/26))
- Don't report bogus RAPL energy on read failures or unknown wrap range ([#154](https://github.com/BenchCommons/perfratio/pull/154), issue [#32](https://github.com/BenchCommons/perfratio/issues/32))
- Decouple off-CPU warning from outlier warnings ([#167](https://github.com/BenchCommons/perfratio/pull/167), issue [#112](https://github.com/BenchCommons/perfratio/issues/112))
- Support /dev/stdout, /dev/null and FIFOs as export targets ([#171](https://github.com/BenchCommons/perfratio/pull/171))
- Remove panic-prone code outside the benchmark module ([#173](https://github.com/BenchCommons/perfratio/pull/173), issue [#34](https://github.com/BenchCommons/perfratio/issues/34))
- Relative ./ paths under cmd.exe and backslash paths with -N ([#177](https://github.com/BenchCommons/perfratio/pull/177), issue [#68](https://github.com/BenchCommons/perfratio/issues/68))
- Readable color palette on legacy Windows consoles ([#178](https://github.com/BenchCommons/perfratio/pull/178), issue [#68](https://github.com/BenchCommons/perfratio/issues/68))
- Respect --reference in all exports and address review findings ([#180](https://github.com/BenchCommons/perfratio/pull/180), issue [#113](https://github.com/BenchCommons/perfratio/issues/113))
- Remove the remaining panics in src/benchmark and main ([#182](https://github.com/BenchCommons/perfratio/pull/182), issue [#34](https://github.com/BenchCommons/perfratio/issues/34))
- Address review follow-up findings for windows console, energy alignment and UX ([#183](https://github.com/BenchCommons/perfratio/pull/183), issue [#113](https://github.com/BenchCommons/perfratio/issues/113))
- Don't panic or lose the exports when stdout is closed early ([#189](https://github.com/BenchCommons/perfratio/pull/189), issue [#187](https://github.com/BenchCommons/perfratio/issues/187))
- MacOS load threshold and CPU model on Linux ARM ([#190](https://github.com/BenchCommons/perfratio/pull/190), issue [#188](https://github.com/BenchCommons/perfratio/issues/188))
- Keep the Cold/Time/Range columns aligned with --precision ([#197](https://github.com/BenchCommons/perfratio/pull/197), issue [#195](https://github.com/BenchCommons/perfratio/issues/195))
- Decode power plan name without unaligned slice ([#227](https://github.com/BenchCommons/perfratio/pull/227), issue [#188](https://github.com/BenchCommons/perfratio/issues/188))
- Restore cross-platform checks on master ([#228](https://github.com/BenchCommons/perfratio/pull/228))
- Terminate surviving descendants after readiness ([#229](https://github.com/BenchCommons/perfratio/pull/229), issue [#79](https://github.com/BenchCommons/perfratio/issues/79))
- Measure wall time from child pre-exec timestamp ([#231](https://github.com/BenchCommons/perfratio/pull/231), issue [#81](https://github.com/BenchCommons/perfratio/issues/81))
- Align `custom_metrics` with `times` ([#265](https://github.com/BenchCommons/perfratio/pull/265), issue [#90](https://github.com/BenchCommons/perfratio/issues/90))
- Make the round-robin target-precision test deterministic ([#264](https://github.com/BenchCommons/perfratio/pull/264), issue [#244](https://github.com/BenchCommons/perfratio/issues/244))
- Locate shell completions with the Cargo 1.91 build directory layout ([#284](https://github.com/BenchCommons/perfratio/pull/284))

### Internal

- Bump actions/checkout from 5 to 7 ([#1](https://github.com/BenchCommons/perfratio/pull/1))
- Bump softprops/action-gh-release from 2 to 3 ([#2](https://github.com/BenchCommons/perfratio/pull/2))
- Bump clap_complete from 4.5.60 to 4.6.9 ([#3](https://github.com/BenchCommons/perfratio/pull/3))
- Bump clap from 4.5.52 to 4.6.1 ([#4](https://github.com/BenchCommons/perfratio/pull/4))
- Migrate from deprecated cargo_bin to cargo::cargo_bin! ([#8](https://github.com/BenchCommons/perfratio/pull/8), issue [#7](https://github.com/BenchCommons/perfratio/issues/7))
- Rebrand leftovers, update metadata, canonical URLs, and add doc/joulex.1 ([#114](https://github.com/BenchCommons/perfratio/pull/114), issue [#86](https://github.com/BenchCommons/perfratio/issues/86))
- Add a 7-day cooldown for updates ([#121](https://github.com/BenchCommons/perfratio/pull/121), issue [#43](https://github.com/BenchCommons/perfratio/issues/43))
- Update crates with RustSec advisories ([#124](https://github.com/BenchCommons/perfratio/pull/124), issue [#28](https://github.com/BenchCommons/perfratio/issues/28))
- Restrict GITHUB_TOKEN to least-privilege permissions ([#129](https://github.com/BenchCommons/perfratio/pull/129), issue [#38](https://github.com/BenchCommons/perfratio/issues/38))
- Quote GITHUB_OUTPUT and GITHUB_ENV redirections (SC2086) ([#130](https://github.com/BenchCommons/perfratio/pull/130), issue [#41](https://github.com/BenchCommons/perfratio/issues/41))
- Take the .deb Description from Cargo.toml ([#131](https://github.com/BenchCommons/perfratio/pull/131), issue [#42](https://github.com/BenchCommons/perfratio/issues/42))
- Stop publishing under upstream's sharkdp.hyperfine id ([#132](https://github.com/BenchCommons/perfratio/pull/132), issue [#40](https://github.com/BenchCommons/perfratio/issues/40))
- Warn that parameter values are inserted into commands verbatim ([#139](https://github.com/BenchCommons/perfratio/pull/139), issue [#37](https://github.com/BenchCommons/perfratio/issues/37))
- Upgrade nix 0.29 -> 0.31 ([#141](https://github.com/BenchCommons/perfratio/pull/141), issue [#88](https://github.com/BenchCommons/perfratio/issues/88))
- Pin all GitHub Actions to full commit SHAs ([#143](https://github.com/BenchCommons/perfratio/pull/143), issue [#39](https://github.com/BenchCommons/perfratio/issues/39))
- Release static Linux ARM64 (musl) and native Windows ARM64 binaries ([#146](https://github.com/BenchCommons/perfratio/pull/146), issue [#69](https://github.com/BenchCommons/perfratio/issues/69))
- Add security scanning (cargo-deny, zizmor, gitleaks, weekly run) ([#147](https://github.com/BenchCommons/perfratio/pull/147), issue [#44](https://github.com/BenchCommons/perfratio/issues/44))
- Explain the output fields, branch comparisons and noise reduction ([#148](https://github.com/BenchCommons/perfratio/pull/148), issue [#71](https://github.com/BenchCommons/perfratio/issues/71))
- Prepare the crate for publishing and cargo-binstall ([#153](https://github.com/BenchCommons/perfratio/pull/153), issue [#93](https://github.com/BenchCommons/perfratio/issues/93))
- Replace the unmaintained statistical crate ([#157](https://github.com/BenchCommons/perfratio/pull/157), issue [#29](https://github.com/BenchCommons/perfratio/issues/29))
- Bump predicates to 3.1.4 and clap_complete to 4.6.11 ([#161](https://github.com/BenchCommons/perfratio/pull/161))
- Unpin indicatif and compute the progress-bar ETA ourselves ([#168](https://github.com/BenchCommons/perfratio/pull/168), issue [#30](https://github.com/BenchCommons/perfratio/issues/30))
- Remove accidentally committed insta .pending-snap files ([#169](https://github.com/BenchCommons/perfratio/pull/169))
- Replace the unmaintained criterion-stats with an in-crate bootstrap ([#170](https://github.com/BenchCommons/perfratio/pull/170), issue [#31](https://github.com/BenchCommons/perfratio/issues/31))
- Add pre-commit Rust checks and contribution guide ([#230](https://github.com/BenchCommons/perfratio/pull/230), issue [#110](https://github.com/BenchCommons/perfratio/issues/110))
- Add AGENTS.md pointing to the org rules ([#241](https://github.com/BenchCommons/perfratio/pull/241))
- Output schema contract ([#242](https://github.com/BenchCommons/perfratio/pull/242), issue [#240](https://github.com/BenchCommons/perfratio/issues/240))
- Rename to tools-benchmark and tools-dataset ([#243](https://github.com/BenchCommons/perfratio/pull/243))
- Release workflow ready for v1.0.0 ([#273](https://github.com/BenchCommons/perfratio/pull/273))
- Security review status for 1.0, bump rustix to 1.1.6 ([#278](https://github.com/BenchCommons/perfratio/pull/278), issue [#22](https://github.com/BenchCommons/perfratio/issues/22))
- Design specs for `--counters`, `--concurrency` and `--optimize` ([#276](https://github.com/BenchCommons/perfratio/pull/276); issues [#49](https://github.com/BenchCommons/perfratio/issues/49), [#53](https://github.com/BenchCommons/perfratio/issues/53), [#98](https://github.com/BenchCommons/perfratio/issues/98))
- PMU extension specs for 1.x ([#281](https://github.com/BenchCommons/perfratio/pull/281); issues [#211](https://github.com/BenchCommons/perfratio/issues/211), [#218](https://github.com/BenchCommons/perfratio/issues/218), [#220](https://github.com/BenchCommons/perfratio/issues/220), [#221](https://github.com/BenchCommons/perfratio/issues/221), [#222](https://github.com/BenchCommons/perfratio/issues/222))
- Changelog, migration guide and cross-language guide ([#280](https://github.com/BenchCommons/perfratio/pull/280), issue [#72](https://github.com/BenchCommons/perfratio/issues/72)); README rewrite ([#279](https://github.com/BenchCommons/perfratio/pull/279))
- Bump dependencies and actions: [#261](https://github.com/BenchCommons/perfratio/pull/261), [#260](https://github.com/BenchCommons/perfratio/pull/260), [#258](https://github.com/BenchCommons/perfratio/pull/258), [#246](https://github.com/BenchCommons/perfratio/pull/246), [#239](https://github.com/BenchCommons/perfratio/pull/239), [#237](https://github.com/BenchCommons/perfratio/pull/237), [#232](https://github.com/BenchCommons/perfratio/pull/232)

### Not in 1.0

Still open after the changes above:

- [#240](https://github.com/BenchCommons/perfratio/issues/240): JSON `energy_samples`, a JSON Schema file with CI validation, and the `doc/understanding-output.md` section.
- [#225](https://github.com/BenchCommons/perfratio/issues/225): removal of the deprecated `$JOULEX_ITERATION`, `$HYPERFINE_ITERATION` and `$JOULEX_SYSTEM_CHECK_ROOT` in the release after 1.0.
- [#59](https://github.com/BenchCommons/perfratio/issues/59): `--energy-domains` (RAPL domain breakdown).
- [#213](https://github.com/BenchCommons/perfratio/issues/213), [#219](https://github.com/BenchCommons/perfratio/issues/219): wiring the derived metrics and multiplexing scaling to `--counters` ([#49](https://github.com/BenchCommons/perfratio/issues/49)).
- [#92](https://github.com/BenchCommons/perfratio/issues/92): separating printing from measurement in the library, and a `cli` feature.
- [#93](https://github.com/BenchCommons/perfratio/issues/93): publishing to crates.io, Homebrew and winget (needs maintainer credentials).
- [#222](https://github.com/BenchCommons/perfratio/issues/222), [#221](https://github.com/BenchCommons/perfratio/issues/221), [#220](https://github.com/BenchCommons/perfratio/issues/220), [#218](https://github.com/BenchCommons/perfratio/issues/218), [#211](https://github.com/BenchCommons/perfratio/issues/211): PMU extensions (specs only, see [#281](https://github.com/BenchCommons/perfratio/pull/281)).
- [#72](https://github.com/BenchCommons/perfratio/issues/72): ideas tracking issue.

---

# Upstream hyperfine history

Everything below is the changelog of upstream hyperfine, kept unchanged apart from heading levels. Entry numbers refer to the [sharkdp/hyperfine](https://github.com/sharkdp/hyperfine) repository.

## v1.20.0

### Features

- Add `--reference-name` option to give a meaningful name to the reference command, see #808 (@niklasdewally)
- The `--ignore-failure` option now supports a comma-separated list of exit codes to ignore (e.g., `--ignore-failure=1,2`), see #836 (@sharkdp)
- Python scripts: Add `--time-unit` option to `advanced_statistics.py` (@sharkdp)
- Python scripts: Add new `plot_benchmarks.py` script for plotting collections of benchmarks, see #806 (@marxin)

### Bugfixes

- Fix bug where naming individual commands with parameter scan was not working correctly, see #794 (@teofr)

### Other

- Restrict `cat` tests to Unix environments, see #776 and #777 (@ritvikos)

## v1.19.0

### Features

- Add a new `--reference <cmd>` option to specify a reference command for the relative speed comparison, see #579, #577 and #744 (@sharkdp)
- Add `--conclude` argument (analog to `--prepare`), see #565 and #719 (@jackoconnordev)
- Allow `--output=…` to appear once for each command, enabling use cases like `hyperfine --output=null my-cmd --output=./file.log my-cmd`, see #529 and #775 (@sharkdp)
- The environment variable `$HYPERFINE_ITERATION` will now contain the current iteration number for each benchmarked command, see #775 (@sharkdp)
- Add iteration information to failure error message, see #771 and #772 (@sharkdp)
- Python scripts: 
  - legend modification parameters and output DPI, see #758 (@Spreadcat)
  - Nicer whiskers plot, see #727 (@serpent7776)

### Bugfixes

- ETA not clearly visible on terminals with a block cursor, see #698 and #699 (@overclockworked64)
- Fix zsh completions, see #717 (@xzfc)

### Other

- Build binaries for aarch64-apple-darwin, see #728 (@Phault)
- Various cleanups (@hamirmahal, @one230six)

## v1.18.0

### Features

- Add support for microseconds via `--time-unit microsecond`, see #684 (@sharkdp)

### Bugfixes

- Proper argument quoting on Windows CMD, see #296 and #678 (@PedroWitzel)


## v1.17.0

### Features

- Add new `--sort` option to control the order in the rel. speed comparison and in markup export formats, see #601, #614, #655 (@sharkdp)
- Parameters which are unused in the command line are now displayed in parentheses, see #600 and #644 (@sharkdp).
- Added `--log-count` option for histogram plots, see `scripts/plot_histogram.py` (@sharkdp)

### Changes

- Updated hyperfine to use `windows-sys` instead of the unmaintained `winapi`, see #624, #639, #636, #641 (@clemenswasser)
- Silenced deprecation warning in Python scripts, see #633 (@nicovank)
- Major update of the man page, see 0ce6578, #647 (@sharkdp)

### Bugfixes

- Do not export intermediate results to stdout when using `-` as a file name, see #640 and #643 (@sharkdp)
- Markup exporting does not fail if benchmark results are zero, see #642 (@sharkdp)


## v1.16.1

### Bugfixes

- Fix line-wrapping of `--help` text (@sharkdp)
- Fix `--input=null` (@sharkdp)


## v1.16.0

### Features

- Added new `--input` option, see #541 and #563 (@snease)
- Added possibility to specify `-` as the filename in the
  `--export-*` options, see #615 and #623 (@humblepenguinn)

### Changes

- Improve hints for outlier warnings if `--warmup` or `--prepare` are in use already,
  see #570 (@sharkdp)

### Bugfixes

- Fix uncolored output on Windows if `TERM` is not set, see #583 (@nabijaczleweli)
- On Windows, only run `cmd.exe` with the `/C` option. Use `-c` for all other shells.
  See #568 and #582 (@FilipAndersson245)

### Other

- Thanks to @berombau for working on dependency upgrades, see #584
- Fixed installationm on Windows, see #595 and #596 (@AntoniosBarotsis)


## v1.15.0

### Features

- Disable colorized output in case of `TERM=dumb` or `NO_COLOR=1`, see #542 and #555 (@nabijaczleweli)
- Add new (experimental) `--min-benchmarking-time <secs>` option, see #527 (@sharkdp)

### Bugfixes

- Fix user and kernel times on Windows, see #368 and #538 (@clemenswasser)

### Other

- Improve `--help` texts of `--export-*` options, see #506 and #522 (@Engineer-of-Efficiency)


## v1.14.0

### Features

- Add a new `--output={null,pipe,inherit,<FILE>}` option to control
  where the output of the benchmarked program is redirected (if at all),
  see #377 and #509 (@tavianator, originally suggested by @BurntSushi)
- Add Emacs org-mode as a new export format, see #491 (@ppaulweber)


## v1.13.0

### Features

- Added a new `--shell=none`/`-N` option to disable the intermediate
  shell for executing the benchmarked commands. Hyperfine normally
  measures and subtracts the shell spawning time, but the intermediate
  shell always introduces a certain level of measurement noise. Using
  `--shell=none`/`-N` allows users to benchmark very fast commands
  (with a runtime on the order of a few milliseconds). See #336, #429,
  and #487 (@cipriancraciun and @sharkdp)
- Added `--setup`/`-s` option that can be used to run `make all` or
  similar. It runs once per set of tests, like `--cleanup`/`-c` (@avar)
- Added new `plot_progression.py` script to debug background interference
  effects.

### Changes

- Breaking change: the `-s` short option for `--style` is now used for
  the new `--setup` option.
- The environment offset randomization is now also available on Windows,
  see #484

### Other

- Improved documentation and test coverage, cleaned up code base for
  future improvements.


## v1.12.0

### Features

- `--command-name` can now take parameter names from `--parameter-*` options, see #351 and #391 (@silathdiir)
- Exit codes (or signals) are now printed in cases of command failures, see #342 (@KaindlJulian)
- Exit codes are now part of the JSON output, see #371 (@JordiChauzi)
- Colorized output should now be enabled on Windows by default, see #427

### Changes

- When `--export-*` commands are used, result files are created before benchmark execution
  to fail early in case of, e.g., wrong permissions. See #306 (@s1ck).
- When `--export-*` options are used, result files are written after each individual
  benchmark command instead of writing after all benchmarks have finished. See #306 (@s1ck).
- Reduce number of shell startup time measurements from 200 to 50, generally speeding up benchmarks. See #378
- User and system time are now in consistent time units, see #408 and #409 (@film42)



## v1.11.0

### Features

- The `-L`/`--parameter-list` option can now be specified multiple times to
  evaluate all possible combinations of the listed parameters:

  ``` bash
  hyperfine -L number 1,2 -L letter a,b,c \
      "echo {number}{letter}" \
      "printf '%s\n' {number}{letter}"
  # runs 12 benchmarks: 2 commands (echo and printf) times 6 combinations of
  # the "letter" and "number" parameters
  ```

  See: #253, #318 (@wchargin)

- Add CLI option to identify a command with a custom name, see #326 (@scampi)

### Changes

- When parameters are used with `--parameter-list` or `--parameter-scan`, the JSON export format
  now contains a dictionary `parameters` instead of a single key `parameter`. See #253, #318.
- The `plot_parametrized.py` script now infers the parameter name, and its `--parameter-name`
  argument has been deprecated. See #253, #318.

### Bugfixes

- Fix a bug in the outlier detection which would only detect "slow outliers" but not the fast
  ones (runs that are much faster than the rest of the benchmarking runs), see #329
- Better error messages for very fast commands that would lead to inf/nan results in the relative
  speed comparison, see #319
- Show error message if `--warmup` or `--*runs` arguments can not be parsed, see #337
- Keep output colorized when the output is not interactive and `--style=full` or `--style=color` is used.


## v1.10.0

### Features

- Hyperfine now comes with shell completion files for Bash, Zsh, Fish
  and PowerShell, see #290 (@four0000four).
- Hyperfine now comes with a basic man page, see #257 (@cadeef)
- During execution of benchmarks, hyperfine will now set a `HYPERFINE_RANDOMIZED_ENVIRONMENT_OFFSET` environment variable in order to randomize the memory layout. See #235 and #241 for references and details.
- A few enhancements for the histogram plotting scripts and the
  advanced statistics script
- Updates for the `plot_whisker.py` script, see #275 (@ghaiklor)

### Bugfixes

- Fix Spin Icon on Windows, see #229
- A few typos have been fixed, see #292 (@McMartin)

### Packaging

- `hyperfine` is now available on MacPorts for macOS, see #281 (@herbygillot)
- `hyperfine` is now available on OpenBSD, see #289 (@minusf)

Package authors: note that Hyperfine now comes with a set of shell completion files and a man page (see above)

## v1.9.0

### Features

- The new `--parameter-list <VAR> <VALUES>` option can be used to run
  a parametrized benchmark on a user-specified list of values.
  This is similar to `--parameter-scan <VAR> <MIN> <MAX>`, but doesn't
  necessarily required numeric arguments.

  ``` bash
  hyperfine --parameter-list compiler "gcc,clang" \
      "{compiler} -O2 main.cpp"
  ```

  See: #227, #234 (@JuanPotato)

- Added `none` as a possible choice for the `--style` option to
  run `hyperfine` without any output, see #193 (@knidarkness)

- Added a few new scripts for plotting various types of benchmark
  results (https://github.com/sharkdp/hyperfine/tree/master/scripts)

### Changes

- The `--prepare` command is now also run during the warmup
  phase, see #182 (@sseemayer)

- Better estimation of the remaining benchmark time due to an update
  of the `indicatif` crate.

### Other

- `hyperfine` is now available on NixOS, see #240 (@tuxinaut)

## v1.8.0

### Features

- The `--prepare <CMD>` option can now be specified multiple times to
  run specific preparation commands for each of the benchmarked programs:

  ``` bash
  hyperfine --prepare "make clean; git checkout master"  "make" \
            --prepare "make clean; git checkout feature" "make"
  ```

  See: #216, #218 (@iamsauravsharma)

- Added a new [`welch_ttest.py`](https://github.com/sharkdp/hyperfine/blob/master/scripts/welch_ttest.py) script to test whether or not the two benchmark
  results are the same, see #222 (@uetchy)

- The Markdown export has been improved. The relative speed is now exported
  with a higher precision (see #208) and includes the standard deviation
  (see #225).

### Other

- Improved documentation for [`scripts`](https://github.com/sharkdp/hyperfine/tree/master/scripts) folder (@matthieusb)

## v1.7.0

### Features

- Added a new `-D`,`--parameter-step-size` option that can be used to control
  the step size for `--parameter-scan` benchmarks. In addition, decimal numbers
  are now allowed for parameter scans. For example, the following command runs
  `sleep 0.3`, `sleep 0.5` and `sleep 0.7`:
  ``` bash
  hyperfine --parameter-scan delay 0.3 0.7 -D 0.2 'sleep {delay}'
  ```
  For more details, see #184 (@piyushrungta25)

### Other

- hyperfine is now in the official Alpine repositories, see #177 (@maxice8, @5paceToast)
- hyperfine is now in the official Fedora repositories, see #196 (@ignatenkobrain)
- hyperfine is now in the official Arch Linux repositories
- hyperfine can be installed on FreeBSD, see #204 (@0mp)
- Enabled LTO for slightly smaller binary sizes, see #179 (@Calinou)
- Various small improvements all over the code base, see #194 (@phimuemue)

## v1.6.0

### Features

- Added a `-c, --cleanup <CMD>` option to execute `CMD` after the completion of all benchmarking runs for a given command. This is useful if the commands to be benchmarked produce artifacts that need to be cleaned up. See #91 (@RalfJung and @colinwahl)
- Add parameter values (for `--parameter-scan` benchmarks) to exported CSV and JSON files. See #131 (@bbannier)
- Added AsciiDoc export option, see #137 (@5paceToast)
- The relative speed is now part of the Markdown export, see #127 (@mathiasrw and @sharkdp).
- The *median* run time is now exported via CSV and JSON, see #171 (@hosewiejacke and @sharkdp).

### Other

- Hyperfine has been updated to Rust 2018 (@AnderEnder). The minimum supported Rust version is now 1.31.

## v1.5.0

### Features

- Show the number of runs in `hyperfine`s output (@tcmal)
- Added two Python scripts to post-process exported benchmark results (see [`scripts/`](https://github.com/sharkdp/hyperfine/tree/master/scripts) folder)

### Other

- Refined `--help` text for the `--export-*` flags (@psteinb)
- Added Snapcraft file (@popey)
- Small improvements in the progress bar "experience".

## v1.4.0

### Features

- Added `-S`/`--shell` option to override the default shell, see #61 (@mqudsi and @jasonpeacock)
- Added `-u`/`--time-unit` option to change the unit of time (`second` or `millisecond`), see #80 (@jasonpeacock)
- Markdown export auto-selects time unit, see #71 (@jasonpeacock)

## v1.3.0

### Feature

- Compute and print standard deviation of the speed ratio, see #83 (@Shnatsel)
- More compact output format, see #70  (@jasonpeacock)
- Added `--style=color`, see #70 (@jasonpeacock)
- Added options to specify the max/exact numbers of runs, see #77 (@orium)

### Bugfixes

- Change Windows `cmd` interpreter to `cmd.exe` to prevent accidentally calling other programs, see #74 (@tathanhdinh)

### Other

- Binary releases for Windows are now available, see #87

## v1.2.0

- Support parameters in preparation commands, see #68 (@siiptuo)
- Updated dependencies, see #69. The minimum required Rust version is now 1.24.

## v1.1.0

* Added `--show-output` option (@chrisduerr and @sevagh)
* Refactoring work (@stevepentland)

## v1.0.0

### Features

* Support for various export-formats like CSV, JSON and Markdown - see #38, #44, #49, #42 (@stevepentland)
* Summary output that compares the different benchmarks, see #6 (@stevepentland)
* Parameterized benchmarks via `-P`, `--parameter-scan <VAR> <MIN> <MAX>`, see #19

### Thanks

I'd like to say a big THANK YOU to @stevepentland for implementing new features,
for reviewing pull requests and for giving very valuable feedback.

## v0.5.0

* Proper Windows support (@stevepentland)
* Added `--style auto/basic/nocolor/full` option (@stevepentland)
* Correctly estimate the full execution time, see #27 (@rleungx)
* Added Void Linux install instructions (@wpbirney)

## v0.4.0

- New `--style` option to disable output coloring and interactive CLI features, see #24 (@stevepentland)
- Statistical outlier detection, see #23 #18

## v0.3.0

### Features

- In addition to 'real' (wall clock) time, Hyperfine can now also measure 'user' and 'system' time (see #5).
- Added `--prepare` option that can be used to clear up disk caches before timing runs, for example (see #8).

### Other

- [Arch Linux package](https://aur.archlinux.org/packages/hyperfine) for Hyperfine (@jD91mZM2).
- Ubuntu/Debian packages are now are available.

## v0.2.0

Initial public release
