# Spec: Reference region SDKs and client guide (#311)

**Status:** Implemented (C/C++, Rust, Python; protocol 1). JavaScript/TypeScript, Go, Java and
Kotlin added by #372 (SDK 1.0.0 each; the protocol and the JSON export are unchanged).

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

The SDK id (`c/1.0.0`, `rust/1.1.0`, `python/1.1.0`; 1.1.0 adds `serve` for `--persistent`, #370) is exported as `results[].regions[].sdk`
(schemaVersion 1.16, see `region-protocol.md`), so results of outdated clients can be flagged.
There is no JSON change beyond #310.

### Managed-language clients (#372)
| Language | Path | Calls | Exit handling |
|---|---|---|---|
| JavaScript, TypeScript | `sdk/js/perfratio_region.js` + `.d.ts` (id `javascript/1.0.0`) | `start`, `stop`, `flush`, `enabled`, `region(name, fn)` | `process.on('exit')` |
| Go | `sdk/go/perfratio_region.go` (id `go/1.0.0`) | `Start`, `Stop`, `Flush`, `Enabled`, `Region(name)` returns the stop function | none in Go: `defer Flush()` in `main`, `Flush()` before `os.Exit` |
| Java | `sdk/java/perfratio/PerfratioRegion.java` (id `java/1.0.0`) | `start`, `stop`, `flush`, `enabled`, `region(name)` (`AutoCloseable`) | shutdown hook |
| Kotlin | `sdk/kotlin/PerfratioRegion.kt` (id `kotlin/1.0.0`) | `start`, `stop`, `flush`, `enabled`, `region(name) { }` | shutdown hook |

They behave like the other clients (no-op without perfratio, monotonic clock, `hello` once,
invalid names dropped with one warning, scoped form stops on throw/panic) and also implement
`serve` for `--persistent` (`docs/specs/persistent-mode.md`). Clocks: `process.hrtime.bigint()`,
`time.Since` of a start instant (monotonic), `System.nanoTime()`. The `.d.ts` file types the
JavaScript client for TypeScript. Other languages: `docs/region-sdk.md` explains how to write a
client in about 30 lines.

## Verification
`tests/sdk_tests.rs` (runs in the normal `cargo test`):

| Check (shared by every client, `tests/sdk_tests.rs`) | Rust | C | C++ | Python | JS | Go | Java | Kotlin |
|---|---|---|---|---|---|---|---|---|
| Three-phase example against `perfratio --region`: region 0.28-0.6 s, process at least 0.15 s longer | yes | yes | yes | yes | yes | yes | yes | yes |
| No-op without perfratio (no output, no file), also with an empty variable | yes | yes | yes | yes | yes | yes | yes | yes |
| Wire format parsed by perfratio's own parser, monotonic timestamps | yes | yes | yes | yes | yes | yes | yes | yes |
| Flush on exit (region left open, `exit`) | `process::exit` | `exit` | n/a | unit test (`sys.exit`) | `process.exit` | n/a (`defer Flush`) | `System.exit` | `exitProcess` |
| Flush on panic/exception, region stopped while unwinding | panic | n/a (no exceptions) | throw | raise | throw | panic | throw | throw |

Per-client unit tests (`cargo test` in `sdk/rust`, `python3 -m unittest` in `sdk/python`) cover
buffering, `hello` once, invalid names, unwritable files and the clock source; the test file runs
them too (`go test` in `sdk/go`). A toolchain that is not installed (`cargo`, `cc`, `c++`, `python3`, `node`, `go`, `java`/`javac`, `kotlinc`) skips its language
with a message; one that fails to build the example fails the test. Rust runs wherever the tests
run. Windows: Rust and Python run, `cc` is normally absent.
