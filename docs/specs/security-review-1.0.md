# Spec: security review status for 1.0 (#22)

## Problem / Motivation
Issue #22 tracks the SAST/SCA/DAST review and its findings (#24 to #44). All of
those issues are closed. This document records the re-audit of the current
`master` and what 1.0 still needs. It changes no behavior and no output
contract (no JSON export change).

## Audit (state of master, re-checked for this PR)

| Area | Result |
|---|---|
| SCA: `cargo deny check` (advisories, licenses, bans, sources) | passes; `unmaintained = "all"`, `yanked = "deny"`, no ignores |
| SCA: `cargo audit` | one warning: rustix 1.1.2, RUSTSEC-2026-0486 (xattr, unsound). We call no xattr API. Fixed here by locking rustix 1.1.6 |
| SAST: `unsafe` | about 70 sites in `src/util`, `src/timer`. Each has a `// SAFETY:` comment, except the `unsafe impl Sync for SendHandle` (Windows), fixed here. Child-side code between fork and exec only calls async-signal-safe functions. `kill(-pid)` is guarded (`pid <= 1` returns early in `terminate_process_group`; PIDs come from `Child::id()`) |
| Command execution | Commands run as `shell -c <command line>` or, with `--shell=none`, via `shell_words` split and `Command::new` without a shell. Parameter values are substituted verbatim into the shell command line; this is documented in the README (#37) and `--shell=none` is the safe mode |
| Export paths | Written atomically via a `create_new` temp file in the target directory and `rename`; symlinks are replaced, not followed (#36). `--export-diff-markdown` still uses `std::fs::write` (truncates, follows symlinks); it is a user-chosen path, low risk |
| `--until`, timeout, Ctrl-C | Each child is its own process group (`process_group(0)`); watchdog and Ctrl-C paths SIGTERM then SIGKILL the group; Windows uses a Job Object. Not fuzzed in this review |
| Workflows (zizmor-style) | top-level `permissions: contents: read`; `contents: write` only on the `build` job; no `pull_request_target`; all actions pinned to SHAs; `persist-credentials: false`; no `${{ }}` interpolation of untrusted input in `run:`; Dependabot cooldown set; `security.yml` runs cargo-deny, zizmor and gitleaks weekly and on PRs |
| Secrets | gitleaks runs in CI over the full history with a digest-pinned image and the default ruleset. There is no `.gitleaks.toml`, so there are no allowlists to audit |

## Left open
- `winget` job still uses the identifier `UnitedOpenSource.joulex`. It is
  opt-in (`WINGET_ENABLED`), but must be renamed before enabling it for 1.0.
- `--export-diff-markdown` should use the same atomic writer as `--export-*`.
- `windows_timer.rs` uses `assert!` on Win32 failures (panic instead of an
  error); acceptable, but a candidate for `Result`.
- No fuzzing of `--import-json`, parameter parsing or the command splitter.
- Windows `unsafe` code is only reviewed by reading; CI runs it but no sanitizer.

## Needed for 1.0
1. Release workflow with artifact checksums and provenance attestation
   (see #273), with `contents: write` limited to the publish step.
2. Branch protection requiring the Security workflow.
3. A `SECURITY.md` with a private reporting channel and supported versions.
4. `cargo vet` or `cargo-deny` `bans` for crates that may build scripts, and
   `cargo auditable` or an SBOM in releases.
5. A fuzz target for the JSON importer and the command-line splitter.
6. Resolve the open items above.
