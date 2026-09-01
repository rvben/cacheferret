# Releasing CacheFerret

The release workflows publish crates.io, PyPI wheels and a source distribution,
GitHub archives, checksums, and the Homebrew tap from a single
`vMAJOR.MINOR.PATCH` tag.

## Prerequisites

- The `cacheferret` crate trusts `rvben/cacheferret`'s `release.yml` workflow on
  crates.io.
- `PYPI_API_TOKEN` is configured for the GitHub repository.
- `HOMEBREW_TAP_DEPLOY_KEY` can push to `rvben/homebrew-tap`.
- The GitHub repository allows Actions to create releases.
- `main` is clean and CI is green.

Tag-driven workflows fail closed before building if a required long-lived
publishing credential is missing. Vership runs the same secret-name check
through its `pre-bump` hook, so an incomplete PyPI or Homebrew release is
refused before the version commit or tag is created. The crates.io publish job
fails closed if its short-lived Trusted Publishing exchange fails.

Bootstrap the repository secrets with `clihatch secrets rvben/cacheferret
--dry-run`, inspect the proposed changes, then run the command without
`--dry-run`.

## Release checklist

1. Put user-visible changes under the appropriate headings in `Unreleased`.
2. Run `make release-readiness`, then preview the release with
   `vership bump patch --dry-run` (or `minor`/`major`).
3. Run `vership preflight`, `make conformance`, `cargo package --locked`, and
   `maturin build --release --locked --sdist`.
4. Run `vership bump patch`. Vership synchronizes the Cargo and Maturin version,
   updates configured documentation version references, promotes `Unreleased`,
   runs its checks and pre-bump secret gate, creates the Conventional Commit and
   annotated tag, and pushes `main` plus the tag. The `make release-patch`,
   `release-minor`, and `release-major` targets are aliases for this step.
5. Watch every release job. Re-run only failed jobs with
   `gh run rerun <run-id> --failed`.
6. Watch the `Public install smoke tests` workflow. It installs the exact
   released version from GitHub archives, crates.io, PyPI wheels, and Homebrew
   on native Intel/ARM macOS and Linux runners, checks archive hashes, executes
   every installed binary, and validates its schema.
7. Run `vership verify X.Y.Z` and independently inspect the GitHub release asset
   and checksum list. The install workflow can also be dispatched manually for
   the version currently exposed by all four distribution channels.

Use `vership bump patch --no-push` only when the release must remain local to
the source checkout. In that mode, publish the validated crate and Python
artifacts directly, then run `vership verify X.Y.Z --targets crates,pypi`.

The workflow rejects tags that do not exactly match the Cargo package version.
A manual `workflow_dispatch` starts in dry-run mode and is the preferred final
automation check before tagging.
