# Specification: Remove legacy `joulex` / `hyperfine` names

**Issue:** [BenchCommons/perfratio#225](https://github.com/BenchCommons/perfratio/issues/225)
**Status:** Implemented.

## Problem / Motivation

After the rebrand (see `rebrand-to-perfratio.md`), compatibility aliases and old
identifiers remained in code, tests, docs and packaging files.

## Contract

### Deprecated aliases removed before the first release

No release was ever tagged, so the aliases added in #263 were removed in 1.0
without a deprecation period (final step of #225). `src/util/legacy.rs` is deleted.

| Name | Behaviour now |
| --- | --- |
| `$JOULEX_ITERATION` | Not exported. |
| `$HYPERFINE_ITERATION` | Not exported. `docs/MIGRATION.md` promises hyperfine compatibility for flags and JSON import only, and `$HYPERFINE_RANDOMIZED_ENVIRONMENT_OFFSET` already has no fallback, so a lone env var alias is not kept. |
| `$JOULEX_SYSTEM_CHECK_ROOT` (hidden test hook) | Ignored; use `$PERFRATIO_SYSTEM_CHECK_ROOT`. |

The deprecation warning on the command line is gone as well.

### Removed immediately

These removals take effect without a deprecation period and are breaking for
anyone using them: the `joulex` completions, `doc/joulex.1` and the
`joulex_eta` progress template key. They are listed in `docs/MIGRATION.md`.

- `joulex` shell completions (`build.rs`, fish snippet in `src/cli.rs`) and `doc/joulex.1`.
- Fallback progress template key `joulex_eta`.
- Internal names: `HyperfineSummary` is now `PerfratioSummary`; test helpers
  `hyperfine*()` are now `perfratio*()`.
- `HYPERFINE_RANDOMIZED_ENVIRONMENT_OFFSET` is now `PERFRATIO_RANDOMIZED_ENVIRONMENT_OFFSET`
  (set for child processes only to randomize memory layout; not documented as an interface).
- Dual `json["joulex"]` assertions in tests.
- The iteration and system-check aliases above (see "Deprecated aliases removed").

### JSON export

No change in this spec. The legacy `"joulex"` key in `src/export/json.rs` is
removed in 1.0 by #262; `perfratio` stays the contract key. Import of files written by
hyperfine keeps working.

### Kept on purpose

- `--import-json` reading old files that only have a top-level `joulex` key
  (`tests/export_metadata_tests.rs`), and the README notes on the old name and import.

- `LICENSE-MIT` copyright line, README attribution, `CHANGELOG.md` history: upstream credit.
- Upstream issue references in comments (`hyperfine#872`, ...) and `--import-json`
  reading hyperfine result files.
- `.github/workflows/CICD.yml` winget identifier `UnitedOpenSource.joulex`: external
  package ID, needs a maintainer decision.
- `doc/execution-order.png`: raster image still shows `hyperfine` (the SVG is updated).
- README package-manager install sections for hyperfine: need rewriting once perfratio is packaged.
