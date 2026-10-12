# AGENTS.md — perfratio

Follow the org-wide rules in [BenchCommons/.github AGENTS.md](https://github.com/BenchCommons/.github/blob/main/AGENTS.md) and the decisions in [.product](https://github.com/BenchCommons/.product). This file only adds what is specific to this repo.

## Role
Tool: the measurement engine, a Rust fork of hyperfine. It owns the **measurement output contract**, i.e. the JSON export (see perfratio#240 and `.product/CONTRACTS.md`).

## Stack and commands
- Rust (edition 2021, MSRV 1.88). Before committing, run `cargo fmt` and `cargo clippy -- -D warnings`, which pre-commit also runs.
- Specs: `docs/specs/<topic>.md`, one per feature, with Problem/Motivation, Contract and issue references.
- See `CONTRIBUTING.md`.

## Rules
- Energy (RAPL) needs bare-metal Linux with readable `energy_uj`. CI covers time and memory only.
- Changing the JSON export changes a contract that benchmark-tool consumes: version it and note it in the spec.
