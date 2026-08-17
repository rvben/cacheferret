# Security policy

CacheFerret deletes directories, so safety issues are treated as security
issues even when they do not cross a traditional privilege boundary.

## Supported versions

The latest released version receives security fixes. Before the first public
release, fixes are made on `main`.

## Reporting a vulnerability

Please use GitHub's private security-advisory flow for
`rvben/cacheferret`. Do not open a public issue for a vulnerability that could
cause unintended deletion, path escape, symlink traversal, or command
execution.

Include the CacheFerret version, operating system, filesystem type, command,
and the smallest safe reproduction you can provide. Never attach private cache
contents or credentials.

You should receive an acknowledgement within seven days. A fix, disclosure
timeline, and credit will be coordinated privately.

## Safety invariants

- Scans never mutate the filesystem.
- Symlinks are not followed.
- Cleanup targets come from a closed catalog and are revalidated immediately
  before deletion.
- Scan-only catalog entries cannot be deleted through either the CLI or the
  public cleanup API.
- Non-interactive cleanup requires explicit confirmation with `--yes`.
