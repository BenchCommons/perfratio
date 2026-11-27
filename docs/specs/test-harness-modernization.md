# Specification: Test Harness Modernization

**Author:** Matheus Breguêz <matbrgz@gmail.com>  
**Status:** Approved  
**Issue:** [BenchCommons/perfratio#7](https://github.com/BenchCommons/perfratio/issues/7)  
**Upstream Reference:** `sharkdp/hyperfine#844`

---

## 1. Overview
When executing `cargo test`, `assert_cmd` issues a deprecation warning:
`warning: use of deprecated associated function assert_cmd::cargo::CommandCargoExt::cargo_bin: incompatible with a custom cargo build-dir, see instead cargo::cargo_bin!`

## 2. Solution
Replace `Command::cargo_bin("perfratio").unwrap()` in `tests/common.rs` with `Command::new(assert_cmd::cargo::cargo_bin!("perfratio"))`.

## 3. Verification Plan
- `cargo test` executes without deprecation warnings.
- All integration tests pass identically.
