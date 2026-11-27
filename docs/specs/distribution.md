# Distribution (crates.io, cargo-binstall, Homebrew)

## Problem / Motivation
Users have no packaged way to install perfratio. The old README badge pointed at a crate that
did not exist. Issue: [#93](https://github.com/BenchCommons/perfratio/issues/93).

## Contract
- `Cargo.toml` carries the crates.io fields (description, license, repository, readme,
  categories) and an `include` list. `cargo package --list` ships only `src/`, `tests/`,
  `build.rs`, `doc/perfratio.1`, `docs/schema/*.json`, README, CHANGELOG and both licenses.
- `[package.metadata.binstall]` matches the release archives built by `CICD.yml`:
  `perfratio-v<version>-<target>.tar.gz` (`.zip` on Windows), with the binary under
  `perfratio-v<version>-<target>/`.
- `packaging/homebrew/perfratio.rb.template` and `render.sh` produce a formula from the release
  `SHA256SUMS`. This is a draft; no tap exists.
- The README has no crates.io badge until the crate is published. The JSON export is unchanged.

## Pending on the maintainers
- crates.io account, ownership and the `CARGO_REGISTRY_TOKEN` secret (or trusted publishing).
- Final crate name (`perfratio` returned 404 on crates.io, so it was free when checked).
- Making the repository or its release assets public.
- Creating the Homebrew tap repository and choosing its owner/name.
- Publishing a `publish-crate` job and tap bump in the release workflow after the above.
- Running `cargo binstall --dry-run perfratio` after the first release.
