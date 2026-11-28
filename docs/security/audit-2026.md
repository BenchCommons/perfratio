# Security audit 2026: master and open PRs #399, #403, #404, #405

Refs #22. Complements `docs/specs/security-review-1.0.md` (which covers the 1.0 review of
`master`). This document adds the features merged after that review (#393 to #406) and the pull
requests that are still open.
It changes no code and no output contract (no JSON export change).

## Scope and method

- Scope: `master` at `447c541` plus the open PRs #399, #403, #404 and #405. Each finding is
  marked **master** or **PR aberto #n**. Line numbers of **master** findings are for
  `447c541`. Line numbers of open-PR findings are for the PR head at audit time and are valid
  in the PR diff, not on `master`.
- Audited heads of the open PRs: #399 `63397bf`, #403 `4009ee7`, #404 `598661a`. #405 was not
  re-verified. The PRs #393, #394, #396, #397, #398, #400, #401, #402 and #406 were audited as
  open PRs and are on `master` now; the findings were re-read against `master` for this
  revision. #419 (macOS thermal pressure) was not reviewed. #395 only bumps
  `cargo-deny-action` (pinned by SHA). #407 (a test fix) is out of scope.
- Method: reading code. No perfratio code was compiled or run, and no finding was reproduced
  with a proof of concept. The only tool run was `cargo audit` for the SCA line below. Where a finding depends on behavior of an external program (the shell,
  the kernel), the text says so.
- Focus: execution of external commands, parsing of untrusted input, `dlopen`/FFI, file
  paths and writes, and secrets in `command_line` and exports.
- SCA: `cargo audit` on the `master` lockfile reported no vulnerabilities (1571 advisories,
  170 crates). The command line, date and advisory-database commit of that run were not
  recorded, and `cargo-audit` was not available to re-run it when this text was corrected, so
  the figure is not verified here. Re-run `cargo audit` to confirm.
- Reviewed for these sinks with no finding: #394 (raw PMU events), #396, #397, #398,
  #400 (all on master) and #405 (open; statistics and CLI only; no process, file, `unsafe` or `/proc` access beyond
  what is listed below). The language SDKs of #399 (`sdk/`) were not reviewed in depth.

## Summary

| Severity | Count | Findings |
|---|---|---|
| Critical | 0 | |
| High | 0 | |
| Medium | 3 | M2 to M4 |
| Low | 10 | M1 (reclassified), L1 to L9 |
| Info | 2 | I1, I2 |

No finding allows code execution by a party that could not already run code as the user who
starts perfratio. The Medium findings concern terminal and CI-log
injection from untrusted data (M2, M4) and disclosure of secrets in shareable files (M3).


## Medium

### M1. `--power-meter`: the normal exit path can block on a descendant (availability, low)

- Where: **master**, `src/energy/meter.rs`, `run_command`: reader `join` at `:130`.
- Already fixed: the meter runs in its own process group (`process_group(0)`, `:91`), the
  timeout path kills the group (`kill(-pid, SIGKILL)`, `:118`) and returns without joining
  the reader.
- Remaining: on the normal exit path, `reader.join()` waits until every holder of the
  stdout pipe closes it. If `sh` exits but a background descendant keeps the pipe open
  (for example `sleep 100 & echo 5`), the `join` blocks, and the timeout does not apply
  because the loop has already ended. The poller thread, `PowerMeterSampler::drop` and
  `try_new` wait on this call. Not reproduced. Severity is low: the meter command is
  chosen by the user.
- Recommendation: after a normal exit, kill the process group too, or do not `join` and
  receive the output through a channel with `recv_timeout`.
- State: **master**. Reclassified from Medium to Low. Corrigido em #427 (PR aberto): the reader reports over a
  channel, `recv_timeout` with the same deadline, the group is killed on expiry (test with a
  background grandchild).

### M2. `--verify-evidence` prints untrusted text unescaped and reads files without a size bound

- Where: **master**, `src/evidence.rs:262-263`, `:270-271`, `:281`, `:286`, `:336` (messages
  built from manifest fields and directory entry names), printed by `src/main.rs:98-103`.
  Unbounded reads: `src/evidence.rs:245` (manifest) and `:302` (every listed file).
- Issue: verifying a bundle that someone else produced is the main use of this command,
  so the bundle is untrusted. `format`, `format_version`, a manifest `path` that fails
  `is_plain_name` (`:233`, which rejects only `/`, `\`, `:` and NUL, not control characters), and the
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
- State: **master**. Corrigido em #428 (PR aberto): every text of the report is escaped with
  `escape_control_chars`, `is_plain_name` rejects control characters, `manifest.json` is
  limited to 1 MiB and files are hashed in a stream (`Sha256`). `verify` opens with
  `O_NOFOLLOW` and checks the metadata of the open handle (no TOCTOU).

### M3. Secrets in the command line are exported and bundled; warnings and redaction are partial

- Where: **master**, `src/export/metadata.rs:480-491` (`command_line`, exported as
  `perfratio.command_line`) and `results[].command`. `redact_power_meter` is at
  `src/export/metadata.rs:496`. The evidence bundle copies `command_line` into `command.json`
  (`src/evidence.rs:147`) and `results.json`.
- Issue: the export keeps the full argument vector. Anything typed as an argument is
  written to JSON, and to the evidence bundle, which exists to be shared. That covers the
  benchmarked commands, `--prepare`, `--setup`, `--conclude`, `--cleanup`, `--shell`,
  `--meta`, `--label`, `--collector-plugin` and parameter values (for example
  `curl -H "Authorization: Bearer ..."`). `redact_power_meter` redacts one option because it can carry
  a token; the other options carry the same risk. The program name is already reduced to
  its file name for the same kind of reason.
- Already covered: the `--meta` help says "Do not put secrets in values" (`src/cli.rs:1322`)
  and the `--evidence` help says "The commands are recorded as typed: keep secrets out of
  them" (`src/cli.rs:1385`). `docs/specs/evidence-bundle.md` also says so.
- Missing: no such warning on `--export-json` (`src/cli.rs:1397`, and the other exports that
  carry `command_line`) or in the README, and there is no redaction for the other options.
- Recommendation: (1) add the warning to the `--export-json` help and to the README.
  (2) Add an opt-in `--redact-command-line` (or a general redaction list) applied to
  `command_line`, `results[].command` and the bundle. (3) Make `redact_power_meter`
  table-driven (a list of option names) instead of one function per option. Adding a
  redaction option changes the export contract: version it and note it in the spec.
- State: open, **master**. Partly mitigated: `--power-meter` is redacted, and `--evidence`
  and `--meta` warn.

### M4. Text from the benchmarked program is printed unescaped (region protocol, persistent mode)

- Where, **master**: `src/region.rs:108` (`got '{kind}'`), `:142` (`unknown event '{other}'`),
  `:102` (`report.sdk`, no validation or length limit, exported as `region_sdk`).
  **PR aberto #399** (head `63397bf`): `src/persistent.rs:238-239` (`fail` message, unknown kind) and
  `:445` (message printed in the error), `:274` (`lines()` without a line-length limit).
- Issue: the program under test writes these strings, and a CI job may benchmark code from
  an untrusted pull request. Control characters reach the terminal and CI log (CWE-150).
  The `::` workflow-command case does not apply to `region.rs` and `persistent.rs`: input is
  read line by line, so no embedded newline survives, and the error message is printed
  after the `Error:` prefix, not at the start of a line. `region_sdk` can be any length.
  In persistent mode, `BufReader::lines()` has no per-line bound and the channel is
  unbounded, so one long line uses unbounded memory (the collector protocol in #403 has a
  64 KiB bound for the same case). `master` already escapes command names and program
  output (`executor.rs:505`), so this is a gap in a few sites.
- Recommendation: use `escape_control_chars` at these sites. Validate the SDK id with the
  rule of `is_valid_name` and limit it to 64 characters. In persistent mode read with a
  limited reader (as `collector::read_line_limited`), and bound the number of lines kept
  for one iteration.
- State: open, **master** (`src/region.rs`) and **PR aberto #399** (`src/persistent.rs`).

## Low

### L1. Evidence directory: created before the run, then written with `fs::write`

- Where: **master**, `src/evidence.rs:161-180` (`prepare`), `:187` and `:211` (`fs::write`).
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
- State: **master**. Corrigido em parte em #428 (PR aberto): files are written with `create_new` and
  `O_NOFOLLOW`. The 0700 directory mode and the temp-directory plus `rename` are not done.

### L2. Collector plugin: path lookup, unbounded reply queue, inherited environment

- Where: **PR aberto #403**, `src/collector.rs:290` (`Command::new(path)`), `:298-306` (reader thread
  and `mpsc::channel`), `:293` (`Stdio::inherit()` for stderr).
- Fixed in head `4009ee7`: a bare name is rejected and an explicit path is required
  (`src/collector.rs:290-296`). The path is not looked up in `PATH`.
- Issue: the reader thread sends every line into an unbounded channel while the benchmark runs
  without reading it, so a plugin that writes unsolicited lines (64 KiB each) grows memory,
  and also shifts replies out of step with requests. The plugin receives the full
  environment: `docs/specs/collector-plugins.md:66` says it runs with the privileges and
  environment of the user. The `--collector-plugin` help in PR #403 also says "The plugin
  runs with your privileges: only use programs you trust". Reply validation is otherwise good: protocol version, name and unit charset,
  declared ids only, finite numbers, escaped error text, 64 KiB line limit, timeouts, no
  shell. Metric values are finite but have no range check (for example `1e308`).
- Recommendation: use `sync_channel` with a small bound and treat an unsolicited line as a
  protocol error. The trust warning and the environment note already exist.
- State: open, **PR aberto #403**.

### L3. Valgrind adapter: binary chosen by an environment variable, unbounded profile read

- Where: **PR aberto #403**, `src/valgrind.rs:190` (`PERFRATIO_VALGRIND`, constant `BINARY_ENV`), `:348` (`read_to_string` of each profile), `:279-294` (`profile_files`).
- Issue: an environment variable selects the program that runs the benchmarked command, so
  anyone who controls the environment of a privileged perfratio can run any program. The
  profile files are written into a 0700 directory created with `create` (not reused),
  which is good, but the benchmarked process runs as the same user, so it can write a
  `cachegrind.out.*` file (or a symlink) there. The read has no size limit.
  `Profile::merge` saturates, so there is no overflow.
- Already covered: `PERFRATIO_VALGRIND` is documented in `docs/specs/valgrind-collector.md`
  (section "CLI", PR #403), which calls the binary trusted code run with the user's
  privileges, and in the `--collector` help.
- Recommendation: print the resolved binary in verbose mode. Limit the profile read (`take`) and skip entries that are
  not regular files (`symlink_metadata`).
- State: open, **PR aberto #403**.

### L4. GPU backends: `dlopen` by soname follows the loader search path

- Where: **master**, `src/energy/gpu.rs:92` (`libc::dlopen`), `:126` (`NVML_LIBS`), `:251`
  (`AMDSMI_LIBS`), `:109` (`assert_eq!` on the symbol type size), `:335` (`unsafe fn
  enumerate` without a `# Safety` section).
- Issue: `libnvidia-ml.so.1` and `libamd_smi.so` are resolved by the dynamic loader
  (`LD_LIBRARY_PATH`, `RUNPATH`, `ld.so.cache`). A library found earlier in the path runs
  its constructors in the perfratio process (with `RTLD_NOW`). That is the usual cost of
  `dlopen`, but `--gpu` is likely to run with elevated privileges together with `--energy`.
  The code is careful elsewhere: `RTLD_LOCAL`, never `dlclose`, out-pointers of valid
  size, vendor return codes checked, a `// SAFETY:` comment on each block. Error cases
  for `sym` use `Option`, except the `assert_eq!`, which panics.
- Partly covered: `AMDSMI_LIBS` (`gpu.rs:251`) already lists
  `/opt/rocm/lib/libamd_smi.so` as a second candidate, but only after the bare soname.
  `NVML_LIBS` has only sonames.
- Recommendation: try fixed absolute system paths first (for example
  `/usr/lib/x86_64-linux-gnu/`, `/usr/lib64/`, `/opt/rocm/lib/`) and fall back to the
  soname only with a warning. Refuse to load when running as root and the file is writable
  by group or other. Replace the `assert_eq!` with a compile-time assertion.
- State: open, **master**. Corrigido em parte em #427 (PR aberto): the `assert_eq!` is now a handled `None`
  (backend degrades). `dlopen` by soname stays; search order and the `LD_LIBRARY_PATH` risk
  are documented in `docs/specs/gpu-energy.md`.

### L5. `--membw`: arithmetic on values read from sysfs and `numa_maps`

- Fixed in head `598661a` of #404: `parse_format` validates `low <= high && high <= 63`
  (`src/membw.rs:98`), which removes the underflow and the oversized shift.
- Where (remaining), **PR aberto #404**: `src/membw.rs:252` (`kb * 1024`) and `:261` (`count * page_bytes`),
  both without `checked_mul`.
- Issue: the `numa_maps` multiplications are unchecked, so a huge value overflows
  (a debug build panics, a release build wraps). The values come
  from the kernel (root-owned sysfs, `/proc`), so exploitation needs a hostile kernel
  interface or a test root. File names in `numa_maps` are escaped by the kernel (`=` and
  spaces) to my knowledge, so a mapping name cannot forge `N0=` tokens. This was not
  checked against kernel sources in this audit.   There is also a PID reuse window in `Sampler` (`/proc/{pid}/numa_maps`, `:291`) after the
  child is reaped, which can only give wrong placement data.
- Recommendation: use `checked_mul` and `saturating_add`.
- State: open, **PR aberto #404**.

### L6. `--verdict-json`: stale file, baseline path, free text read by agents

- Where: **master**, `src/main.rs:322-332` (written only after a successful run),
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
- State: open, **master**. Adding fields changes the verdict schema: version it.

### L7. Test hook `PERFRATIO_SYSTEM_CHECK_ROOT` is active in release builds

- Where: **master**, `src/system_check.rs:12` and `:74`, `src/thermal.rs:18`,
  `src/benchmark/mod.rs:356`.
- Issue: the variable moves the root of the system and thermal checks to any directory. It
  exists for tests, but anyone who sets it makes `--check-system` and the thermal watch
  report whatever the directory holds, and the export does not record that it was set.
  The environment block of an export (and the evidence bundle) is then not evidence of the
  real machine. `PERFRATIO_ENERGY_BACKEND` is a user-facing variable and is fine.
- Recommendation: build the hook only under `cfg(test)` or a `test-hooks` feature; if it
  must stay, record `fake_root: true` in the export and print a warning.
- State: open, **master**. Recording it changes the export: version it.

### L8. `--import-json`: no size limit

- Where: **master**, `src/import.rs:82` (`serde_json::from_reader` on the whole file) and
  `:368` (`import_environment`).
- Issue: a large imported file uses memory proportional to its size. `serde_json` limits
  nesting to 128, so there is no stack overflow, and the fuzz target `import_json` covers
  parsing. Content is validated after parsing (#27) and names are escaped (#24).
- Recommendation: check `metadata.len()` against a limit (for example 256 MiB) with a clear
  message.
- State: **master**, minor. Corrigido em #427 (PR aberto): 256 MiB limit (`MAX_IMPORT_BYTES`), clear
  error; also in `import_environment`.

### L9. `PERFRATIO_RUN_DIR`: directory created from the user-supplied artifact path

- Where: **master**, `src/benchmark/executor.rs:233` (`create_dir_all(run_dir)`).
- Issue: the directory is created below the directory of `--artifact-dir` and follows a
  symlink at that level. The path is chosen by the user who runs perfratio, so this is
  not a privilege boundary. Listed for completeness because other file writes in this
  codebase were hardened against symlinks (#36).
- Recommendation: none beyond documenting that the artifact directory should be private.
- State: **master**, accepted risk unless perfratio runs privileged on a shared directory.

## Info

- I1. Evidence bundle integrity: `manifest.json` holds the hashes and nothing signs it, so
  an attacker who rewrites a file and the manifest together is only caught if the manifest
  SHA-256 printed by `--verify-evidence` was pinned elsewhere. **master**. `docs/specs/evidence-bundle.md` documents this
  (secrets, pinned hash, no signature). The names
  `CON`, `NUL` and similar Windows device names pass `is_plain_name` (`src/evidence.rs:233`). They are
  harmless on Unix and could matter on Windows; reject them if Windows is supported for
  `--verify-evidence`.
- I2. (**master**) `--power-meter` and `--prepare` style options run a shell command by design. That
  matches the README note on parameter substitution (#37) and the `--shell=none` mode. The
  meter command is never exported (`redact_power_meter`, `src/export/metadata.rs:496`), and the meter reply is
  validated (`parse_reading`: first number, known units, finite and not negative, 4 KiB
  limit, 5 s timeout, no stderr capture).

## Verified controls on `master` (no action)

- Export files: temporary file with `create_new` in the target directory, then `rename`.
  Symlinks are replaced, not followed (`src/export/mod.rs:503` (`write_to_file`, `create_new` at `:528`)). `--verdict-json` and
  the Markdown export use the same writer.
- Region file: `create_new`, mode 0600 on Unix, 64 MiB read limit, region names limited to
  64 characters of `[A-Za-z0-9_]` (`src/region.rs:185-215`, `MAX_FILE_BYTES` at `:40`, `is_valid_name` at `:43`).
- Terminal output of imported names and of program output goes through
  `escape_control_chars` (`src/util/sanitize.rs`).
- `cgroup` and `/proc/stat` readers return `Option` on any parse failure and never panic.
  The cgroup path from `/proc/self/cgroup` is joined without removing `..`, but it is
  read from the kernel and only used to read `cpu.stat`/`cpuacct.usage` (read-only).
- PMU and event names are restricted to `[A-Za-z0-9_.-]`, at most 128 characters, before
  becoming path components (`src/pmu_events.rs`, around `:62`).
- **PR aberto #403** `PrivateDir`: mode 0700 and `create` (not `create_dir_all`), removed on drop.

## Follow-up

| Item | Where to track |
|---|---|
| M1 to M4, L1, L4, L6 to L9 (master) | new issues after review of this document |
| L2, L3 (PR #403), L5 (PR #404), M4 `src/persistent.rs` (PR #399) | comments on those PRs |
| Fuzz targets for `--verify-evidence`, collector replies, `parse_numa_maps`, `parse_format` | extend `fuzz/` |
| Windows: `unsafe` review and `windows_timer.rs` panics, `winget` identifier | unchanged from `docs/specs/security-review-1.0.md` |
