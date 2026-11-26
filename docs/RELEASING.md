# Releasing

Release assets are built by `.github/workflows/CICD.yml` when a tag `vX.Y.Z` is pushed.

## Asset names

| Asset | Name |
|---|---|
| Archive | `perfratio-vX.Y.Z-<target>.tar.gz` (`.zip` for `*-pc-windows-msvc`) |
| Archive layout | `perfratio-vX.Y.Z-<target>/perfratio(.exe)`, man page, `autocomplete/`, licenses, README, CHANGELOG |
| Debian | `perfratio[-musl]_X.Y.Z_<arch>.deb` |
| Checksums | `SHA256SUMS` (all archives, `.deb` and the SBOM, `sha256sum` format) |
| SBOM | `perfratio-X.Y.Z.cdx.json` (CycloneDX 1.5, from `Cargo.lock`) |
| Attestations | GitHub artifact attestations (build provenance) for every asset above; verify with `gh attestation verify <file> --repo BenchCommons/perfratio` |

`[package.metadata.binstall]` in `Cargo.toml` must match the archive name and layout. Archives are
reproducible for a given binary: mtime is the commit time, entries are sorted, owner is 0, gzip has no name or timestamp.

## Checklist

1. Master is green. `cargo publish --dry-run --locked` and `cargo package --list` look right.
2. Bump `version` in `Cargo.toml`, run `cargo build` to refresh `Cargo.lock`, update `CHANGELOG.md` and `CITATION.cff`.
3. If the JSON export changed, bump its version and note it in `docs/specs/output-schema-contract.md` (tools-benchmark consumes it).
4. Merge the release PR. Tag the merge commit: `git tag -s vX.Y.Z && git push origin vX.Y.Z`.
5. Wait for CICD: build matrix, `checksums`, `publish-crate`.
6. Check the GitHub release: all archives, `.deb` files and `SHA256SUMS` are present. Spot-check one with `sha256sum -c`.
7. `cargo binstall --dry-run perfratio@X.Y.Z` resolves a real asset.
8. Homebrew: `packaging/homebrew/render.sh X.Y.Z SHA256SUMS > Formula/perfratio.rb` in the tap repo, then open a PR there.
9. Winget: set repository variable `WINGET_ENABLED=true` only after the first manual submission.

## Publish job

`publish-crate` runs on `v*` tags. It checks that the tag equals the `Cargo.toml` version, then runs
`cargo publish --locked`. If the `CARGO_REGISTRY_TOKEN` secret is not set, the job succeeds and
skips every publishing step (it prints a notice). Crates.io trusted publishing (OIDC) can replace the token later.

## Maintainer-only prerequisites

- Make the repository (or at least releases) public.
- Create the crates.io token, or configure trusted publishing, and store it as `CARGO_REGISTRY_TOKEN`.
- Create the Homebrew tap repository and give it a way to receive the formula.
- Confirm the repository owner in `Cargo.toml` URLs and the formula (currently `BenchCommons/perfratio`).
