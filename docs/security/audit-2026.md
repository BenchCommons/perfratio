# Security audit 2026: master and open PRs #393 to #406

Refs #22. Complements `docs/specs/security-review-1.0.md` (which covers the 1.0 review of
`master`). This document adds the features that are still in open pull requests.
It changes no code and no output contract (no JSON export change).

## Scope and method

- `master` at `ef49a5e`, and the diffs of the open PRs #393 to #406 (read with `gh pr diff`;
  PR heads were fetched as refs, none was checked out). #395 only bumps `cargo-deny-action`
  (pinned by SHA). #407 (a test fix) is out of scope.
- Method: reading code. Nothing was compiled or run, and no finding was reproduced with a
  proof of concept. Where a finding depends on behavior of an external program (the shell,
  the kernel), the text says so.
- Focus: execution of external commands, parsing of untrusted input, `dlopen`/FFI, file
  paths and writes, and secrets in `command_line` and exports.
- SCA: `cargo audit` on the `master` lockfile reports no vulnerabilities (1571 advisories,
  170 crates).
- PRs reviewed for these sinks with no finding: #394 (raw PMU events), #396, #397, #398,
  #400, #405 (statistics and CLI only; no process, file, `unsafe` or `/proc` access beyond
  what is listed below). The language SDKs of #399 (`sdk/`) were not reviewed in depth.

## Summary

| Severity | Count | Findings |
|---|---|---|
| Critical | 0 | |
| High | 0 | |
| Medium | 4 | M1 to M4 |
| Low | 9 | L1 to L9 |
| Info | 2 | I1, I2 |

No finding allows code execution by a party that could not already run code as the user who
starts perfratio. The Medium findings concern availability (M1), terminal and CI-log
injection from untrusted data (M2, M4) and disclosure of secrets in shareable files (M3).

Line numbers for PR findings refer to the head of the PR at audit time; they are valid in
the PR diff, not on `master`.

## Medium

### M1. `--power-meter`: a stuck reader thread can hang the benchmark (availability)

- Where: PR #402, `src/energy/meter.rs:76-128` (`run_command`), reader `join` at `:112`
  (timeout path) and `:121` (normal path).
- Issue: the meter command runs as `sh -c <command>`. On timeout, only `sh` is killed
  (`child.kill()`, `:110`), not its process group. The reader thread then blocks in
  `read_to_string` until every holder of the stdout pipe closes it, and `reader.join()`
  waits for it. If the command spawns a child that outlives `sh` (a pipeline such as
  `curl ... | awk ...`, or a background process), the `join` does not return and the
  5 s timeout does not bound the call. The same applies after a normal exit when a
  descendant keeps the pipe open. The poller thread, `PowerMeterSampler::drop` (`:258`)
  and `try_new` (`:212`) all wait on this call. Not reproduced.
- Recommendation: start the meter in its own process group (as the benchmarked commands
  are) and kill the group. Do not `join` the reader after a kill; drop the handle or use a
  non-blocking read with a deadline.
- State: open, in PR #402.

### M2. `--verify-evidence` prints untrusted text unescaped and reads files without a size bound

- Where: PR #393, `src/evidence.rs:263`, `:271`, `:281`, `:333-336` (messages built from
  manifest fields and directory entry names), printed by `src/main.rs:92`.
  Unbounded reads: `src/evidence.rs:245` (manifest) and `:302` (every listed file).
- Issue: verifying a bundle that someone else produced is the main use of this command,
  so the bundle is untrusted. `format`, `format_version`, a manifest `path` that fails
  `is_plain_name` (which rejects only `/`, `\`, `:` and NUL, not control characters), and the
  names of unlisted files in the directory go into `stderr` without escaping. A name or
  field with ESC or CSI bytes can rewrite the terminal (same class as #24, CWE-150) or
  forge a `ok  results.json` line in a CI log. A line starting with `::` in a GitHub
  Actions log is read as a workflow command. Separately, `fs::read` loads a whole file
  before the size is compared with the manifest, so a large file listed in the manifest
  uses that much memory.
- Recommendation: pass every untrusted string through `util::sanitize::escape_control_chars`
  (already used for imported JSON). Compare `metadata.len()` with `entry.bytes` first and
  hash with a streaming reader (`io::copy` into the hasher, through `Read::take(bytes + 1)`).
  Cap the manifest size (for example 1 MiB).
- State: open, in PR #393.

### M3. Secrets in the command line are exported and bundled; redaction exists only for `--power-meter`

- Where: `master` `src/export/metadata.rs:457-468` (`command_line`, exported as
  `perfratio.command_line`) and `results[].command`. PR #402 adds `redact_power_meter`
  (`src/export/metadata.rs:473`). PR #393 copies `command_line` into `command.json`
  (`src/evidence.rs`, `bundle_files`) and `results.json`.
- Issue: the export keeps the full argument vector. Anything typed as an argument is
  written to JSON, and to the evidence bundle, which exists to be shared. That covers the
  benchmarked commands, `--prepare`, `--setup`, `--conclude`, `--cleanup`, `--shell`,
  `--meta`, `--label`, `--collector-plugin` and parameter values (for example
  `curl -H "Authorization: Bearer ..."`). PR #402 redacts one option because it can carry
  a token, which shows the problem is known, but the other options carry the same risk.
  The bundle spec (`docs/specs/evidence-bundle.md`) says so in prose; `--help` and the
  JSON export docs do not. The program name is already reduced to its file name for the
  same kind of reason.
- Recommendation: (1) warn in `--help` of `--evidence`, `--export-json` and in the README.
  (2) Add an opt-in `--redact-command-line` (or a general redaction list) applied to
  `command_line`, `results[].command` and the bundle. (3) Make the redaction code in #402
  table-driven (a list of option names) instead of one function per option. Adding a
  redaction option changes the export contract: version it and note it in the spec.
- State: open (master). Partly mitigated in PR #402 for `--power-meter`.

### M4. Text from the benchmarked program is printed unescaped (region protocol, persistent mode)

- Where, `master`: `src/region.rs:108` (`got '{kind}'`), `:142` (`unknown event '{other}'`),
  `:102` (`report.sdk`, no validation or length limit, exported as `region_sdk`).
  PR #399: `src/persistent.rs:238-239` (`fail` message, unknown kind) and `:430` (message
  printed in the error), `:268` (`lines()` without a line-length limit).
- Issue: the program under test writes these strings, and a CI job may benchmark code from
  an untrusted pull request. Control characters reach the terminal and CI log (CWE-150,
  and `::` workflow commands after an embedded newline). `region_sdk` can be any length.
  In persistent mode, `BufReader::lines()` has no per-line bound and the channel is
  unbounded, so one long line uses unbounded memory (the collector protocol in #403 has a
  64 KiB bound for the same case). `master` already escapes command names and program
  output (`executor.rs:505`), so this is a gap in a few sites.
- Recommendation: use `escape_control_chars` at these sites. Validate the SDK id with the
  rule of `is_valid_name` and limit it to 64 characters. In persistent mode read with a
  limited reader (as `collector::read_line_limited`), and bound the number of lines kept
  for one iteration.
- State: open (master and PR #399).

## Low

### L1. Evidence directory: created before the run, then written with `fs::write`

- Where: PR #393, `src/evidence.rs:161-180` (`prepare`), `:187` and `:211` (`fs::write`).
- Issue: `prepare` checks that the directory is empty and creates it, but the files are
  written after the benchmarks. A benchmarked command that runs as the same user can plant
  symlinks with the names of the bundle files in the meantime, and `fs::write` follows
  them and truncates the target. The directory also gets the default umask mode, and the
  bundle holds the command line and the environment. The attacker already runs as the user
  (so the gain is small), unless perfratio runs privileged for RAPL or `perf` and benchmarks
  less-trusted code. `verify` has a similar window between `symlink_metadata` (`:290`) and
  `fs::read` (`:302`).
- Recommendation: create the directory with mode 0700, and open each file with
  `create_new(true)` (and `O_NOFOLLOW` on Unix). Write into a fresh temporary directory
  next to the target and `rename` it into place at the end, so the final directory appears
  complete or not at all.
- State: open, in PR #393.

### L2. Collector plugin: path lookup, unbounded reply queue, inherited environment

- Where: PR #403, `src/collector.rs:290` (`Command::new(path)`), `:298-306` (reader thread
  and `mpsc::channel`), `:293` (`Stdio::inherit()` for stderr).
- Issue: a plugin given as a bare name (`--collector-plugin gpu`) is looked up in `PATH`.
  The reader thread sends every line into an unbounded channel while the benchmark runs
  without reading it, so a plugin that writes unsolicited lines (64 KiB each) grows memory,
  and also shifts replies out of step with requests. The plugin receives the full
  environment. Reply validation is otherwise good: protocol version, name and unit charset,
  declared ids only, finite numbers, escaped error text, 64 KiB line limit, timeouts, no
  shell. Metric values are finite but have no range check (for example `1e308`).
- Recommendation: require a path with a directory component or canonicalize and print the
  resolved path. Use `sync_channel` with a small bound and treat an unsolicited line as a
  protocol error. Document that the plugin is trusted and inherits the environment.
- State: open, in PR #403.

### L3. Valgrind adapter: binary chosen by an environment variable, unbounded profile read

- Where: PR #403, `src/valgrind.rs:190` (`PERFRATIO_VALGRIND`, constant `BINARY_ENV`), `:348` (`read_to_string` of each profile), `:279-294` (`profile_files`).
- Issue: an environment variable selects the program that runs the benchmarked command, so
  anyone who controls the environment of a privileged perfratio can run any program. The
  profile files are written into a 0700 directory created with `create` (not reused),
  which is good, but the benchmarked process runs as the same user, so it can write a
  `cachegrind.out.*` file (or a symlink) there. The read has no size limit.
  `Profile::merge` saturates, so there is no overflow.
- Recommendation: document the variable next to the other `PERFRATIO_*` ones and print the
  resolved binary in verbose mode. Limit the profile read (`take`) and skip entries that are
  not regular files (`symlink_metadata`).
- State: open, in PR #403.

### L4. GPU backends: `dlopen` by soname follows the loader search path

- Where: PR #401, `src/energy/gpu.rs:92-93` (`libc::dlopen`), `:126` (`NVML_LIBS`), `:251`
  (`AMDSMI_LIBS`), `:109` (`assert_eq!` on the symbol type size), `:335` (`unsafe fn
  enumerate` without a `# Safety` section).
- Issue: `libnvidia-ml.so.1` and `libamd_smi.so` are resolved by the dynamic loader
  (`LD_LIBRARY_PATH`, `RUNPATH`, `ld.so.cache`). A library found earlier in the path runs
  its constructors in the perfratio process (with `RTLD_NOW`). That is the usual cost of
  `dlopen`, but `--gpu` is likely to run with elevated privileges together with `--energy`.
  The code is careful elsewhere: `RTLD_LOCAL`, never `dlclose`, out-pointers of valid
  size, vendor return codes checked, a `// SAFETY:` comment on each block. Error cases
  for `sym` use `Option`, except the `assert_eq!`, which panics.
- Recommendation: try fixed absolute system paths first (for example
  `/usr/lib/x86_64-linux-gnu/`, `/usr/lib64/`, `/opt/rocm/lib/`) and fall back to the
  soname only with a warning. Refuse to load when running as root and the file is writable
  by group or other. Replace the `assert_eq!` with a compile-time assertion.
- State: open, in PR #401.

### L5. `--membw`: arithmetic on values read from sysfs and `numa_maps`

- Where: PR #404, `src/membw.rs:88-98` (`parse_format`, accepts `high < low`), `:114`
  (`high - low + 1`), `:119` (`value << low` with `low` up to `u32::MAX`), `:250` and `:259`
  (`kb * 1024`, `count * page_bytes`).
- Issue: `format/<term>` of an uncore PMU with `config:9-3` or a start bit of 64 or more
  underflows or overflows. A debug build panics and a release build produces a wrong
  `config`. The `numa_maps` multiplications are unchecked in the same way. The values come
  from the kernel (root-owned sysfs, `/proc`), so exploitation needs a hostile kernel
  interface or a test root. File names in `numa_maps` are escaped by the kernel (`=` and
  spaces) to my knowledge, so a mapping name cannot forge `N0=` tokens. This was not
  checked against kernel sources in this audit. The sibling parser in PR #394
  (`src/pmu_events.rs:144`) already checks `lo > hi || hi > 63`; the same check belongs here.
  There is also a PID reuse window in `Sampler` (`/proc/{pid}/numa_maps`, `:291`) after the
  child is reaped, which can only give wrong placement data.
- Recommendation: reuse the `pmu_events` range check; use `checked_mul` and `saturating_add`.
- State: open, in PR #404.

### L6. `--verdict-json`: stale file, baseline path, free text read by agents

- Where: PR #406, `src/main.rs:290-302` (written only after a successful run),
  `src/verdict.rs:170` and `:394` (`baseline` path), `command` and the `new`/`removed` lists.
- Issue: the file is written with `write_to_file` (atomic, replaces symlinks), which is
  right. But if perfratio fails before the end (a failing command, Ctrl-C), an older
  verdict file stays in place and a script or agent reading it gets the previous result.
  The document has no run id or timestamp to detect that. `baseline` repeats the path of
  the baseline file (it can reveal a home directory). Command strings and names come from
  the command line or an imported baseline and reach an agent as text, which makes them a
  prompt-injection channel; they are JSON-escaped, which is enough for parsers, not for
  models.
- Recommendation: remove or truncate the target at start (after argument validation), add a
  run id and `started_at`, document that string fields are data and never instructions,
  and consider the file name only (as for the program name) in `baseline`.
- State: open, in PR #406. Adding fields changes the verdict schema: version it.

### L7. Test hook `PERFRATIO_SYSTEM_CHECK_ROOT` is active in release builds

- Where: `master` `src/system_check.rs:12` and `:74`, `src/thermal.rs:18`,
  `src/benchmark/mod.rs:280`.
- Issue: the variable moves the root of the system and thermal checks to any directory. It
  exists for tests, but anyone who sets it makes `--check-system` and the thermal watch
  report whatever the directory holds, and the export does not record that it was set.
  The environment block of an export (and the evidence bundle) is then not evidence of the
  real machine. `PERFRATIO_ENERGY_BACKEND` is a user-facing variable and is fine.
- Recommendation: build the hook only under `cfg(test)` or a `test-hooks` feature; if it
  must stay, record `fake_root: true` in the export and print a warning.
- State: open (master). Recording it changes the export: version it.

### L8. `--import-json`: no size limit

- Where: `master` `src/import.rs:82` (`serde_json::from_reader` on the whole file) and
  `:368` (`import_environment`).
- Issue: a large imported file uses memory proportional to its size. `serde_json` limits
  nesting to 128, so there is no stack overflow, and the fuzz target `import_json` covers
  parsing. Content is validated after parsing (#27) and names are escaped (#24).
- Recommendation: check `metadata.len()` against a limit (for example 256 MiB) with a clear
  message.
- State: open (master), minor.

### L9. `PERFRATIO_RUN_DIR`: directory created from the user-supplied artifact path

- Where: `master` `src/benchmark/executor.rs:233` (`create_dir_all(run_dir)`).
- Issue: the directory is created below the directory of `--artifact-dir` and follows a
  symlink at that level. The path is chosen by the user who runs perfratio, so this is
  not a privilege boundary. Listed for completeness because other file writes in this
  codebase were hardened against symlinks (#36).
- Recommendation: none beyond documenting that the artifact directory should be private.
- State: accepted risk unless perfratio runs privileged on a shared directory.

## Info

- I1. Evidence bundle integrity: `manifest.json` holds the hashes and nothing signs it, so
  an attacker who rewrites a file and the manifest together is only caught if the manifest
  SHA-256 printed by `--verify-evidence` was pinned elsewhere. PR #393 documents this in
  `docs/specs/evidence-bundle.md` (secrets, pinned hash, no signature). The names
  `CON`, `NUL` and similar Windows device names pass `is_plain_name` (`:233`). They are
  harmless on Unix and could matter on Windows; reject them if Windows is supported for
  `--verify-evidence`.
- I2. `--power-meter` and `--prepare` style options run a shell command by design. That
  matches the README note on parameter substitution (#37) and the `--shell=none` mode. The
  meter command is never exported after #402 (`redact_power_meter`), and the meter reply is
  validated (`parse_reading`: first number, known units, finite and not negative, 4 KiB
  limit, 5 s timeout, no stderr capture).

## Verified controls on `master` (no action)

- Export files: temporary file with `create_new` in the target directory, then `rename`.
  Symlinks are replaced, not followed (`src/export/mod.rs:438-477` (`write_to_file`)). `--verdict-json` and
  the Markdown export use the same writer.
- Region file: `create_new`, mode 0600 on Unix, 64 MiB read limit, region names limited to
  64 characters of `[A-Za-z0-9_]` (`src/region.rs:173-221`, `:42`).
- Terminal output of imported names and of program output goes through
  `escape_control_chars` (`src/util/sanitize.rs`).
- `cgroup` and `/proc/stat` readers return `Option` on any parse failure and never panic.
  The cgroup path from `/proc/self/cgroup` is joined without removing `..`, but it is
  read from the kernel and only used to read `cpu.stat`/`cpuacct.usage` (read-only).
- PR #394: PMU and event names are restricted to `[A-Za-z0-9_.-]`, at most 128 characters,
  before becoming path components (`src/pmu_events.rs:59-69`).
- PR #403 `PrivateDir`: mode 0700 and `create` (not `create_dir_all`), removed on drop.

## Follow-up

| Item | Where to track |
|---|---|
| M1 to M4 and L1 to L6 for the PRs | comments on #393, #399, #401, #402, #403, #404, #406 |
| M3, M4 (master parts), L7, L8 | new issues after review of this document |
| Fuzz targets for `--verify-evidence`, collector replies, `parse_numa_maps`, `parse_format` | extend `fuzz/` |
| Windows: `unsafe` review and `windows_timer.rs` panics, `winget` identifier | unchanged from `docs/specs/security-review-1.0.md` |
