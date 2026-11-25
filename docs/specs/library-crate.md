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
- The JSON export contract (perfratio#240) is not changed and not exposed by the library. `Measurement` is not the export schema.
- Not in this change (follow-ups from #92): separating printing from `BenchmarkRunner::finish`, exposing `deep` statistics and results types, a `cli` feature to drop `clap`/`indicatif`/`colored` from library builds.

## Tests
- Doc test on the crate root and unit tests in `src/api.rs` (`true`, empty argv, zero runs, non-zero exit).
- The existing suite covers the unchanged binary. `cargo package` verifies the package builds.
