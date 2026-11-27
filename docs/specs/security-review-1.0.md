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
| SCA: `cargo audit` | clean after this PR (re-run after merging `master`, which locks rustix 1.1.5). RUSTSEC-2026-0486 (rustix xattr, unsound; we call no xattr API) is fixed here by locking rustix 1.1.6 |
| SAST: `unsafe` | about 70 sites in `src/util`, `src/timer`. Each has a `// SAFETY:` comment, except the `unsafe impl Sync for SendHandle` (Windows), fixed here. Child-side code between fork and exec only calls async-signal-safe functions. `kill(-pid)` is guarded (`pid <= 1` returns early in `terminate_process_group`; PIDs come from `Child::id()`) |
| Command execution | Commands run as `shell -c <command line>` or, with `--shell=none`, via `shell_words` split and `Command::new` without a shell. Parameter values are substituted verbatim into the shell command line; this is documented in the README (#37) and `--shell=none` is the safe mode |
| Export paths | Written atomically via a `create_new` temp file in the target directory and `rename`; symlinks are replaced, not followed (#36). `--export-diff-markdown` uses the same writer (`export::write_to_file`) |
| `--until`, timeout, Ctrl-C | Each child is its own process group (`process_group(0)`); watchdog and Ctrl-C paths SIGTERM then SIGKILL the group; Windows uses a Job Object. Not fuzzed in this review |
| Workflows (zizmor-style) | top-level `permissions: contents: read`; in the current `master` workflow (`CICD.yml`), `contents: write` only on the `build` job (the release workflow in #273 adds a `checksums` job that also has `contents: write`); no `pull_request_target`; all actions pinned to SHAs; `persist-credentials: false`; no `${{ }}` interpolation of untrusted input in `run:`; Dependabot cooldown set; `security.yml` runs cargo-deny, zizmor and gitleaks weekly and on PRs |
| Secrets | gitleaks runs in CI over the full history with a digest-pinned image. `.gitleaks.toml` extends the default ruleset and allowlists only `tests/` and `src/**/test(s).rs` fixtures |

## Left open
- `winget` job still uses the identifier `UnitedOpenSource.perfratio`. It is
  opt-in (`WINGET_ENABLED`), but must be renamed before enabling it for 1.0.
- `windows_timer.rs` uses `assert!` on Win32 failures (panic instead of an
  error); acceptable, but a candidate for `Result`.
- Fuzzing covers `--import-json` and the command splitter (`fuzz/`). Parameter parsing is not fuzzed yet. The targets were only compile-checked locally (no nightly toolchain or cargo-fuzz available); the optional `fuzz.yml` workflow runs them on nightly.
- Windows `unsafe` code is only reviewed by reading; CI runs it but no sanitizer.

## Needed for 1.0
- [ ] 1. Release workflow with artifact checksums and provenance attestation
  (see #273), with `contents: write` limited to the publish step. Done in
  `CICD.yml`: tag-gated `sbom` and `checksums` jobs; attestations via
  `actions/attest-build-provenance` (pinned SHA, `id-token` and `attestations`
  write only on the `checksums` job). Verified for real on the first tag.
- [ ] 2. Branch protection requiring the Security workflow. Needs a maintainer
  (repository settings).
- [x] 3. `SECURITY.md` with a private reporting channel (GitHub security
  advisories) and supported versions. Maintainers must enable private
  vulnerability reporting in the repository settings for the link to work.
- [x] 4. SBOM in releases: CycloneDX JSON generated with `cargo-cyclonedx`,
  attached to the release, listed in `SHA256SUMS` and attested.
  (`cargo vet` is not adopted.)
- [x] 5. Fuzz targets `import_json` and `command_splitter` in `fuzz/`, excluded
  from the package and workspace. The `Fuzz` workflow is optional and not a
  required check.
- [x] 6. `--export-diff-markdown` uses the atomic writer; `.gitleaks.toml` added.
- [ ] 7. Remaining open items above (`winget` identifier, `windows_timer.rs`
  panics, Windows sanitizer).

No JSON export change: this work does not touch the output contract.
