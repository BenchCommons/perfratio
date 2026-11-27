# Contributing

Install [pre-commit](https://pre-commit.com/) and enable the repository hooks:

```sh
pre-commit install
```

Every commit then runs `cargo fmt --all -- --check` and
`cargo clippy --locked --all-targets -- -D warnings`. Run the same checks before
opening a pull request. The CI workflow also runs formatting, linting, tests,
and builds across supported targets.

Keep each commit focused on one change. Put unrelated formatting or CLI aliases
in separate commits so reviews and regressions remain easy to trace. Open a
pull request for changes to `master` and wait for the required checks and an
independent approval before merging.

## Schema version

The JSON export version is `SCHEMA_VERSION` in `src/export/json.rs`. Changing the
export changes a contract (see `docs/specs/output-schema-contract.md`), so a bump
touches several places. `scripts/check-schema-version.sh` checks all of them and
names each file that disagrees; the `Schema version` workflow runs it on every pull
request. Run it locally with no build needed:

```sh
scripts/check-schema-version.sh
```

To bump the version, change `SCHEMA_VERSION` first, then fix what the script reports:

- `docs/schema/perfratio-export.schema.json` (`schemaVersion.const` and the description) and
  its frozen copy `docs/schema/<version>/perfratio-export.schema.json`.
- The "currently `"X.Y"`" statement in `README.md`, `docs/CROSS_LANGUAGE.md`,
  `docs/MIGRATION.md` and `doc/understanding-output.md`.
- The status line of `docs/specs/output-schema-contract.md`.
- Tests that assert the current `schemaVersion` literally.

When several pull requests bump the version, the one merged later renumbers on rebase
and reruns the script.
