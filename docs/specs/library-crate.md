# Spec: library target with a minimal pre-stable API (#92)

## Problem / Motivation
References: `sharkdp/hyperfine#747`, `#367`, `#104`, perfratio#92.

Everything lived in the binary crate, so other Rust code (CI tools, test harnesses, `cargo xtask bench`) could not reuse the measurement engine. Many internal modules change in parallel PRs, so the public surface must stay tiny.

## Contract
- The package has a library target `perfratio` (`src/lib.rs`) and the unchanged binary `perfratio` (`src/main.rs`, which only uses the library). `build.rs`, the binary name and CLI behavior are unchanged.
- The module declarations moved from `main.rs` to `lib.rs`. These modules are `pub` only so the binary can use them. They are `#[doc(hidden)]` and carry **no compatibility promise**.
- Public API, module `perfratio::api`, **pre-stable** (may change in any `0.x` release), with `#![warn(missing_docs)]` in that module only:
  - `measure(argv: &[&str], runs: u64) -> anyhow::Result<Measurement>`: runs the program without a shell, output discarded. `Measurement` has per-run `real`, `user`, `system` (seconds), `memory_bytes`, `energy_joules` (`Option<f64>`), `exit_codes`. A non-zero exit is not an error.
  - `EnergyMeter` (`new`, `is_available`, `start`, `stop`): RAPL on bare-metal Linux with readable `energy_uj`, otherwise unavailable and `stop` returns `None`.
  - `stats::{mean, median, standard_deviation}`.
- `Benchmark` (builder: `new(argv)`, `runs`, `warmup`, `energy`, `deep_stats`, `run`) runs the same measurement engine as the CLI (`benchmark::Benchmark`, `BenchmarkRunner`) with `OutputStyleOption::Disabled` and returns `BenchmarkReport { result: BenchmarkResult, deep_stats: Option<DeepStats>, warnings: Vec<String> }`. The command runs without a shell, its output is discarded, a non-zero exit is not an error. `BenchmarkResult` and `DeepStats` are re-exported from `perfratio::api`.
- `BenchmarkRunner::finish(print_header)` (terminal) and `finish_quiet()` share one body (`finalize`). `finish_quiet` prints nothing (no summary, no warnings on stderr) and returns the rendered warnings next to the result. The terminal output of the binary is byte-identical (snapshot tests).
- Cargo feature `cli` (default on) enables `clap`, `clap_complete` and `indicatif`. It gates `perfratio::cli`, the `from_cli_arguments` constructors and the progress bars (no-op stand-ins without it). The `perfratio` binary has `required-features = ["cli"]`, so `cargo install perfratio` is unchanged. `colored` stays a dependency of the library: it is used throughout the output code and has no transitive dependencies of its own. CI runs clippy and the library tests with `--no-default-features`.
- The JSON export contract (perfratio#240) is not changed and not exposed by the library. `Measurement` is not the export schema.
- Not done: dropping `colored` from library builds; a stable (1.0) API promise; a `Benchmark` option for custom environment, input or shell execution.

## Tests
- Doc tests on the crate root and `Benchmark`, and unit tests in `src/api.rs` (`true`, empty argv, zero runs, non-zero exit, mock executor with deep stats and energy).
- `cargo test --lib --no-default-features` (the tests that need clap are gated by `cli`).
- The existing suite covers the unchanged binary. `cargo package` verifies the package builds.
