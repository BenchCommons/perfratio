# Security policy

## Supported versions

Security fixes go to the latest `1.x` release. Older releases are not patched.

| Version | Supported |
|---------|-----------|
| 1.x (latest) | yes |
| < 1.0 | no |

## Reporting a vulnerability

Please do not open a public issue for a vulnerability.

Report it privately with a GitHub security advisory:
<https://github.com/BenchCommons/perfratio/security/advisories/new>

Include the affected version (`perfratio --version`), your platform, the
command line or input file that triggers the problem, and the impact you
expect. A minimal reproduction helps.

You can expect an acknowledgement within 7 days. We will tell you when the
report is confirmed or rejected, and we will agree on a disclosure date with
you once a fix is ready. Fixes are published as a patch release with a GitHub
security advisory that credits the reporter, unless you ask not to be named.

## Scope

In scope: the `perfratio` binary and library, the release archives and the
GitHub workflows in this repository. Examples are memory-safety bugs, crashes
on crafted `--import-json` input, terminal escape injection, unsafe handling of
export paths, and supply-chain problems in releases.

Out of scope: running untrusted command lines. `perfratio` executes the
commands you give it, and parameter values are substituted into the command
line as documented in the README. Use `--shell=none` when parameter values are
not trusted.

## Verifying releases

Release assets come with a `SHA256SUMS` file, a CycloneDX SBOM
(`perfratio-<version>.cdx.json`) and GitHub artifact attestations. To verify a
downloaded asset:

```sh
gh attestation verify <file> --repo BenchCommons/perfratio
sha256sum --check --ignore-missing SHA256SUMS
```
