# Releasing CacheFerret

The release workflows publish crates.io, PyPI wheels and a source distribution,
GitHub archives, checksums, and the Homebrew tap from a single
`vMAJOR.MINOR.PATCH` tag.

## Prerequisites

- `CARGO_REGISTRY_TOKEN` is configured for the GitHub repository.
- `PYPI_API_TOKEN` is configured for the GitHub repository.
- `HOMEBREW_TAP_DEPLOY_KEY` can push to `rvben/homebrew-tap`.
- The GitHub repository allows Actions to create releases.
- `main` is clean and CI is green.

Bootstrap the repository secrets with `clihatch secrets rvben/cacheferret
--dry-run`, inspect the proposed changes, then run the command without
`--dry-run`.

## Release checklist

1. Move noteworthy entries from `Unreleased` into a dated version section.
2. Set the same version in `Cargo.toml`, then run `cargo update -w` to refresh
   `Cargo.lock` if needed.
3. Run `make check`, `make conformance`, `cargo package --locked`, and
   `maturin build --release --locked --sdist`.
4. Commit with `chore(release): prepare vX.Y.Z`.
5. Push `main`, then create and push the signed or annotated `vX.Y.Z` tag.
6. Watch every release job. Re-run only failed jobs with
   `gh run rerun <run-id> --failed`.
7. Verify the GitHub checksums, `cargo install cacheferret`,
   `pipx install cacheferret`, and `brew install rvben/tap/cacheferret` on a
   clean machine.

The workflow rejects tags that do not exactly match the Cargo package version.
A manual `workflow_dispatch` starts in dry-run mode and is the preferred final
automation check before tagging.
