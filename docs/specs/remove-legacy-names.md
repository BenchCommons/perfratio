# Specification: Remove legacy `joulex` / `hyperfine` names

**Issue:** [BenchCommons/perfratio#225](https://github.com/BenchCommons/perfratio/issues/225)
**Status:** Partially implemented (deprecation phase); the removal release is pending.

## Problem / Motivation

After the rebrand (see `rebrand-to-perfratio.md`), compatibility aliases and old
identifiers remained in code, tests, docs and packaging files.

## Contract

### Deprecation phase (this change)

| Name | Behaviour now | Removal |
| --- | --- | --- |
| `$JOULEX_ITERATION`, `$HYPERFINE_ITERATION` | Still exported next to `$PERFRATIO_ITERATION`. If the command line mentions one of them (command, `--prepare`, `--conclude`, ...), one warning is printed to stderr: `Warning: $X is deprecated and will be removed in the next release; use $PERFRATIO_ITERATION.` | next release |
| `$JOULEX_SYSTEM_CHECK_ROOT` (hidden test hook) | Still read as fallback for `$PERFRATIO_SYSTEM_CHECK_ROOT`, with a deprecation warning. | next release |

The aliases live in `src/util/legacy.rs` and the fallback in `src/system_check.rs`.
Detection of the iteration variables is textual and only counts `$NAME`,
`${NAME}` and `%NAME%` references in the command line, not other mentions such
as file names. A variable set by a wrapper script that is invoked from the
command line cannot trigger the warning.

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

### JSON export

No change in this spec. The legacy `"joulex"` key in `src/export/json.rs` is
removed in 1.0 by #262; `perfratio` stays the contract key. Import of files written by
hyperfine keeps working.

### Kept on purpose

- `LICENSE-MIT` copyright line, README attribution, `CHANGELOG.md` history: upstream credit.
- Upstream issue references in comments (`hyperfine#872`, ...) and `--import-json`
  reading hyperfine result files.
- `.github/workflows/CICD.yml` winget identifier `UnitedOpenSource.joulex`: external
  package ID, needs a maintainer decision.
- `doc/execution-order.png`: raster image still shows `hyperfine` (the SVG is updated).
- README package-manager install sections for hyperfine: need rewriting once perfratio is packaged.
