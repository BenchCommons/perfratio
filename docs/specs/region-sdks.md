# Spec: Reference region SDKs and client guide (#311)

**Status:** Implemented (C/C++, Rust, Python; protocol 1).

## Problem / Motivation
References: #311, #310, #240, `docs/CROSS_LANGUAGE.md`.

The region protocol (`region-protocol.md`) is only useful if instrumenting a program takes one
copy-paste. `catalog-algorithms` needs the same two calls in many languages; hand-written clients
would drift from the protocol. This feature ships small reference clients and a guide for writing
more. megafine's `instrument/` templates are the prior art (GPL-3.0, concept only; the clients
here are written independently).

## Contract
Each client is one file (the Rust crate: one `src/lib.rs`), has no dependencies, is
`MIT OR Apache-2.0`, and carries the protocol constant (`1`) and its own version (`1.0.0`):

| Language | Path | Calls |
|---|---|---|
| C, C++ | `sdk/c/perfratio_region.h` (stb style: define `PERFRATIO_REGION_IMPLEMENTATION` in one file) | `perfratio_region_start/stop/flush/enabled`, C++ `perfratio::Region` (RAII) |
| Rust | `sdk/rust/` (crate `perfratio-region`, `src/lib.rs`) | `start`, `stop`, `flush`, `enabled`, `region(name)` guard |
| Python | `sdk/python/perfratio_region.py` | `start`, `stop`, `flush`, `enabled`, `region(name)` context manager |

Behaviour of every client (documented in `docs/region-sdk.md`):
- no-op when `PERFRATIO_REGION_FILE` is unset or empty;
- monotonic clock; events buffered in memory; `hello 1 <lang>/<version>` written once, then the
  events, at exit and on `flush()`; I/O errors are ignored;
- invalid names are dropped with one warning on stderr;
- the scoped form stops the region while an exception or panic unwinds.

The SDK id (`c/1.0.0`, `rust/1.0.0`, `python/1.0.0`) is exported as `results[].regions[].sdk`
(schemaVersion 1.16, see `region-protocol.md`), so results of outdated clients can be flagged.
There is no JSON change beyond #310.

Go, Java, JavaScript and Kotlin clients are left for demand; `docs/region-sdk.md` explains how to
write one in about 30 lines.

## Verification
`tests/sdk_tests.rs` (runs in the normal `cargo test`):

| Check | Rust | C | C++ | Python |
|---|---|---|---|---|
| Three-phase example against `perfratio --region`: region 0.28-0.6 s, process at least 0.15 s longer | yes | yes | yes | yes |
| No-op without perfratio (no output, no file), also with an empty variable | yes | yes | yes | yes |
| Wire format parsed by perfratio's own parser, monotonic timestamps | yes | yes | yes | yes |
| Flush on exit (region left open, `exit`) | `process::exit` | `exit` | n/a | unit test (`sys.exit`) |
| Flush on panic/exception, region stopped while unwinding | panic | n/a (no exceptions) | throw | raise |

Per-client unit tests (`cargo test` in `sdk/rust`, `python3 -m unittest` in `sdk/python`) cover
buffering, `hello` once, invalid names, unwritable files and the clock source; the test file runs
them too. A toolchain that is not installed (`cargo`, `cc`, `c++`, `python3`) skips its language
with a message; one that fails to build the example fails the test. Rust runs wherever the tests
run. Windows: Rust and Python run, `cc` is normally absent.
