# Spec: MCP server `perfratio-mcp` (#386)

**Issue:** #386 (the `--verdict-json` half is `verdict-json.md`, #406). Parent: #365. Related:
`library-crate.md` (the `api::Benchmark` engine), `compare-baseline.md`, `output-schema-contract.md`.

## Problem / Motivation
An agent that optimizes code should call perfratio as a tool: measure, compare with a baseline and
read the verdict (`verdict-json.md`), without a shell, terminal parsing or its own statistics. The
Model Context Protocol (MCP) is the common way for agent hosts to call tools.

## Decisions (answers to the questions of `verdict-json.md`)
1. **Scope:** `capabilities`, `run_benchmark`, `compare`. The two last ones execute programs; the
   restrictions are in "Trust model".
2. **Dependencies:** none. JSON-RPC 2.0 over stdio is written with `serde_json` (about 400 lines in
   `src/mcp.rs`). `rmcp` would add `tokio` and a large tree through `cargo-deny` for three tools.
3. **Packaging:** a second binary `perfratio-mcp` of the same crate (`src/bin/perfratio-mcp.rs`),
   logic in the hidden library module `perfratio::mcp` (no compatibility promise, like the other
   modules). It does not need the `cli` feature (no `clap`). A separate crate was not chosen: it needs
   a workspace, a second release and a second lockfile for 400 lines. It can be split later without
   changing the protocol.
4. **Capabilities:** version, `verdictVersion`, protocol versions, metrics available on this host
   (`energy` only with a working sampler), limits, allow-list and root. It does not duplicate
   `--check-system` (#314/#313 fingerprint and quality floor are not included, see "Not done").
5. **Protocol:** MCP revision **2026-07-28** (current at modelcontextprotocol.io when this was
   written), plus the `initialize` handshake of **2025-11-25** for hosts that have not moved yet.
6. **Where:** here, next to the engine. `tools-benchmark` can use the verdict file without it.

## Contract

### Start
```
perfratio-mcp [--root DIR] [--allow PROGRAM]...
```
The host starts it as a subprocess. One JSON-RPC message per line on stdin and stdout, UTF-8, no
embedded newline; stderr is for diagnostics only; EOF on stdin ends the process (exit 0). The
benchmarked programs have stdin, stdout and stderr redirected to null, so they cannot corrupt the
channel. No socket is opened and there is no HTTP transport.

### Protocol (subset)
- **Modern (2026-07-28):** stateless. Every request carries
  `params._meta["io.modelcontextprotocol/protocolVersion"]`; a different value is answered with
  error `-32022` and `data: {supported, requested}`. Results carry `"resultType": "complete"`; those of `server/discover` and `tools/list` also carry the mandatory caching hints `ttlMs: 0` and `cacheScope: "public"` (no user-specific data; the tool set depends on the build only).
  Methods: `server/discover`, `ping`, `tools/list`, `tools/call`.
- **Legacy (2025-11-25):** `initialize` answers `protocolVersion: "2025-11-25"`; after it (or after
  `notifications/initialized`) the methods above are accepted without `_meta`. Without `_meta` and
  before `initialize` the answer is `-32602`.
- Notifications get no answer. `notifications/cancelled` is ignored: calls are served one at a time
  and a running benchmark is not interrupted. Batches are not part of MCP: `-32600`. Invalid JSON:
  `-32700` with `id: null`. Unknown method: `-32601`. Unknown tool or missing `name`: `-32602`.
- Not implemented: resources, prompts, logging, progress, tasks, subscriptions, `outputSchema`.

### Tools
Failures of a tool (invalid arguments, program not allowed, path outside the root, program cannot
be started, unreadable export) are results with `isError: true` and the message as text, so the
model can read and correct them. Successful results carry the JSON both in `structuredContent` and
serialized in a `text` block.

| Tool | Arguments | Result (`structuredContent`) |
| --- | --- | --- |
| `capabilities` | none | `tool`, `verdictVersion`, `protocolVersions`, `metrics`, `limits`, `allowed_programs`, `root`, `platform` |
| `run_benchmark` | `argv` (required), `runs` (1..1000, default 10), `warmup` (0..1000, default 0), `energy`, `export_file` | `{result, exported_to}`; `result` is one entry of the export's `results[]` |
| `compare` | `baseline_file` (required), and either `current_file` or `argv` (+ `runs`, `warmup`, `energy`) | the verdict document of `verdict-json.md` (`verdictVersion` 1.0), `baseline` is `baseline_file` |

- `argv` is a program and its arguments, run **without a shell**, 1 to 256 strings, no NUL.
- `export_file` writes `{"results": [result]}`, readable by `--import-json`, `--compare` and by
  `baseline_file`. It is written atomically (`export::write_to_file`).
- `compare` matches commands by their command line, as `--compare` does: the baseline must have been
  measured with the same `argv`. The estimator, confidence, significance and noise settings are the
  defaults of the CLI. Budgets (`--budget`) are not exposed; the verdict has no `gate`.
- Unknown arguments are rejected.
- The measurement engine is `api::Benchmark` (library-crate.md): the same code as the CLI, with
  `--style none`. Energy needs bare-metal Linux with a readable `energy_uj` (AGENTS.md); elsewhere
  `energy: true` leaves the energy fields empty and `capabilities.metrics.energy` is `false`.
- The JSON export contract and `schemaVersion` are unchanged. The MCP surface is versioned by
  `serverInfo.version` and by `verdictVersion`; a tool added is MINOR, an argument or result key
  removed or changed is MAJOR, clients ignore unknown keys.

## Trust model
`run_benchmark` and `compare` with `argv` **execute programs chosen by the client with the privileges
of the `perfratio-mcp` process**. This is code execution by design, like any shell tool.

- The server trusts its host. Whoever writes to its stdin can run what the process user can run. MCP
  leaves consent to the host: it must obtain the user's approval of each tool call (the tool
  descriptions and annotations say `destructiveHint: true`, `openWorldHint: true`, but annotations
  are not a control). Do not connect it to an untrusted model or host without that approval step.
- `--allow PROGRAM` (repeatable) restricts `argv[0]` to the exact strings given. It is a string
  match, not a sandbox: an allowed interpreter (`python`, `sh`) runs anything. Allow benchmark
  binaries, not interpreters.
- No shell is used, so there is no metacharacter injection through `argv`; arguments are passed
  verbatim. The environment of the process is inherited, including secrets in it.
- Files: `baseline_file`, `current_file` and `export_file` must be relative, without `..`, and
  resolve (after symlinks) to a path below `--root` (default: the working directory). For
  `export_file` the directory must already exist. This limits what the file tools read and write,
  not what an executed program can do.
- Input: a line is at most 1 MiB (`MAX_LINE`, without the newline). A longer line is answered with `-32600` (`id: null`) and its remainder is discarded without being buffered; invalid UTF-8 gives `-32700`.
- `export_file` is refused if the destination is a symbolic link, so a link planted below the root cannot redirect the write.
- Limits: `runs` and `warmup` at most 1000, `argv` at most 256 strings. There is no time limit, one
  call at a time, no cancellation: a slow command blocks the server until it ends. The host should
  apply its own timeout and kill the process.
- Imported exports are untrusted data: `--import-json` validation and control-character escaping
  apply.
- No network: no listening socket, no outgoing connection, no telemetry.

## Test plan (implemented)
`tests/mcp_tests.rs` starts the binary and talks over stdio: `server/discover` and `tools/list`,
legacy handshake, version mismatch (`-32022`), unknown method and tool, parse error and batch,
`capabilities`, `compare` of two fixed exports (trade-off: faster, more memory; identical data),
paths outside the root (`..`, absolute, empty, symlink), invalid arguments (empty `argv`, `runs`
0 and 1001, unknown key, missing key), caching hints, an oversized line, `--root` without value, export to a symlink), the allow-list, and a `run_benchmark` of `true` whose export
is read back by `compare` (with `current_file` and with `argv`). Verdicts are asserted only on
fixed exports; the commands that run are checked for shape, not for timing. Unix-only tests use
`true` and `false`.

## Not done
- Distribution: the `perfratio-mcp` binary is not in the release archives of `CICD.yml` (pending; it is built with `cargo install` or from source only).
- Budgets and the `gate` in `compare`; `--noise-threshold`, `--equivalence-margin`, estimator as
  arguments.
- Fingerprint (#314) and measurement-quality floor (#313) in `capabilities`.
- Timeouts, cancellation, progress, parallel calls; `outputSchema`; HTTP transport.
- A separate crate and publication; the surface is pre-stable.

## References
#386, #365, #406 (`--verdict-json`), #325, #318, #92 (library). MCP 2026-07-28:
<https://modelcontextprotocol.io/specification/2026-07-28/basic>.
