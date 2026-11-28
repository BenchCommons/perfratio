# Specification: Evidence bundle (`--evidence`, `--verify-evidence`)

**Issue:** [#375](https://github.com/BenchCommons/perfratio/issues/375)
**Status:** Shipped. Bundle format 1.0 (versioned separately from the JSON export `schemaVersion`).
**Related:** #313 (`measurement-floor.md`), #314 (`environment-fingerprint.md`), #316 (`workload-validation.md`),
#318 (`measurement-quality.md`), #240 (`output-schema-contract.md`).

## 1. Problem / Motivation

Results, environment, calibration, validation and raw samples are spread over several flags and
exports. A third party cannot audit a benchmark, or tell whether a result was edited, from one
artifact. `--evidence DIR` writes one directory; `--verify-evidence DIR` proves that it is
unchanged.

## 2. Contract

```
--evidence DIR          Write the bundle to DIR (absent or empty) after the run.
--verify-evidence DIR   Check a bundle and exit. Not combinable with commands or --evidence.
```

`--evidence` fails before the first benchmark if `DIR` is a file or a non-empty directory:
a bundle is never overwritten or merged. The bundle is written once, after the last
benchmark (also after Ctrl-C; `results.json` then has `"interrupted": true`).

### 2.1 Files

All files sit directly in `DIR` (no subdirectories).

| file | content |
|:--|:--|
| `results.json` | The JSON export (`output-schema-contract.md`, `schemaVersion` in the manifest), byte for byte what `--export-json` writes for the run. The authoritative record; the other files are views of it. |
| `samples.jsonl` | One JSON object per result and run: `command_index`, `run`, `time` (seconds) and every other per-run series of the result (`user_times`, `system_times`, `exit_codes`, `memory_usage_byte`, `run_ids`, energy, counters, ...) of the same length as `time`. Results without per-run times give no lines. |
| `environment.json` | `environment`, `environment_fingerprint`, `system`, `measurements`, `execution`, `statistics` and `meta` from `perfratio.*` (`null` when absent). |
| `calibration.json` | `{ "available": true, "data": <perfratio.calibration> }`, else `{ "available": false }` (no `--calibrate`). |
| `validation.json` | `{ "available": true, "data": [{ "command_index", "command", "validation" }] }` for the results with output validation, else `{ "available": false }`. |
| `quality.json` | Same shape as `validation.json` with the `quality` report of each result. |
| `command.json` | `argv` (`perfratio.command_line`: the arguments as given, the program reduced to its file name), `perfratio_version`, `started_at`, `labels`. |
| `manifest.json` | See 2.2. Written last; it is the only file not listed in itself. |

Privacy: the bundle has no host name, user name, environment variables or working directory,
as for every export. The command line and the benchmarked commands are recorded exactly as
typed, so a secret passed as an argument or inside a command string is in the bundle. Keep
secrets out of benchmarked commands (read them from the environment of the command instead).
`--label` and `--meta` values are recorded as given.

### 2.2 `manifest.json`

```json
{
  "format": "perfratio-evidence",
  "format_version": "1.0",
  "perfratio_version": "1.0.0",
  "results_schema_version": "1.37",
  "created_at": "2026-11-27T03:17:05Z",
  "files": [
    { "path": "calibration.json", "sha256": "<64 hex digits>", "bytes": 25 }
  ]
}
```

`files` has an entry for every other file, sorted by `path`; the seven files above are
required. `sha256` is the SHA-256 of the file's bytes as lowercase hex. Schema:
`docs/schema/perfratio-evidence-manifest.schema.json` (JSON Schema 2020-12; unknown keys are
allowed).

### 2.3 Versioning

`format_version` is `MAJOR.MINOR`. Adding files or manifest keys raises MINOR; a reader of the
same MAJOR ignores what it does not know. Removing or redefining a file or key, or a different
hash rule, raises MAJOR. The manifest does not change the JSON export contract and does not bump
`schemaVersion`.

### 2.4 Verification

`perfratio --verify-evidence DIR` fails (exit code 1, one `FAILED:` line per problem on stderr) if

- `manifest.json` is missing, is not valid JSON, has another `format` or another MAJOR of `format_version`;
- a listed file is missing, not a regular file (a symlink, a directory), has another size or another SHA-256;
- a path is not a plain file name (`/`, `\`, `:`, `..`, control characters) or is listed twice;
- `manifest.json` is larger than 1 MiB, or a listed file is a symlink or not a regular file;
- a required file is not listed;
- `DIR` contains a file that the manifest does not list.

On success it prints `ok  <file>` per file and `manifest sha256: <hex>`. Publish that value
next to the bundle: the manifest cannot vouch for itself, so a changed manifest (hashes
recomputed together with an edited file) is only detected against a pinned manifest hash.
Verification checks integrity, not authenticity: it does not re-run the benchmark and does not
check a signature.

### 2.5 Hardening (audit M2, L1)

The bundle is untrusted input. Every text in the report that comes from it (names, `format`,
`format_version`) is printed with control characters escaped as `\u{..}`. Files are opened
without following symlinks (`O_NOFOLLOW` on Unix), the type and size are checked on the open
handle, and hashing reads in a stream, at most `bytes + 1` bytes per file. `--evidence` creates
each file with `create_new` and never follows a symlink. The bundle format and the JSON export
are unchanged.

## 3. Tests

Unit tests in `src/evidence.rs` (samples, file names). `tests/evidence_tests.rs`: round trip
(write, verify, content of every file, `results.json` equals `--export-json`), the manifest
against the schema, tamper cases (edited, deleted, added file, size change, bad manifest,
wrong format and version, path traversal, listed-twice), non-empty directory refused, no host
name or environment variable in the bundle.
Hardening tests: escape sequences in names, oversized manifest, symlink planted in place of a
file and at write time.
