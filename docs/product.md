# CacheFerret product direction

## Product promise

CacheFerret helps developers quickly answer two questions:

1. Where did my disk space go?
2. Which rebuildable caches can I remove right now?

It should feel fast, trustworthy, and satisfying: open it, see the largest
developer caches, focus one, and press `d`. CacheFerret handles the difficult
recognition and last-moment validation without turning ordinary cleanup into a
warning-dialog exercise.

CacheFerret is deliberately narrower than a generic disk analyzer. Its value
comes from understanding developer ecosystems, knowing which directories are
actually caches, explaining their restoration cost, and refusing to treat
potentially irreplaceable state as disposable.

## Primary users

- Developers reclaiming space during normal local work.
- Power users who expect ncdu/gdu-style keyboard speed.
- Teams and automation that need deterministic, inspectable JSON behavior.
- Coding agents that need an offline schema and stable confirmation/error
  contracts before they invoke a destructive command.

## Experience contract

The default experience is the TUI. Bare `cacheferret` opens it when stdin and
stdout are terminals; when piped, the same command performs a read-only JSON
scan. Explicit subcommands remain available for interactive and automated use.

Core TUI interactions:

| Intent | Interaction |
| --- | --- |
| Move | Arrow keys or `j`/`k` |
| Select/unselect and advance | `Space` |
| Select/unselect all visible caches | `a` |
| Delete selection, or focused cache | `d` |
| Confirm a risky delete | `y`; cancel with `n` or `Esc` |
| Filter | `/` |
| Cycle all/project/global scope | `Tab` |
| Cycle size/age/name sorting | `s` |
| Help | `?` |
| Quit | `q` |

The interface should prioritize the cache list, size, age, scope, ecosystem,
and a useful focused-item explanation. Selection must remain quick: repeated
`Space` presses build a batch while advancing down the list, and `a` operates
on the current filtered/scope view. Background work must remain visible and
must not freeze input. Empty, filtered-empty, scanning, deleting, success,
conflict, and failure states all deserve intentional copy and layout.

Storage accounting must distinguish three concepts throughout the TUI, text,
and JSON interfaces: logical/apparent file length, filesystem blocks allocated
to the tree, and the observed net change in available filesystem space after a
real deletion. Hard links are counted once. Allocated blocks are not presented
as guaranteed reclaimable space because APFS clones can share extents. Observed
free-space changes stay separate per filesystem rather than being summed across
volumes that may share an APFS container, and the interface explains that other
disk activity, snapshots, and delayed reclamation can affect the result.

## Proportional confirmation

Deletion must remain direct. With a selection, `d` reviews and deletes the
batch; without one, it reviews and deletes the focused cache. A universal
confirmation prompt would undermine
the product's ncdu/gdu-style workflow, while deleting every target without
context would make the tool hard to trust. CacheFerret therefore confirms only
when the freshly reviewed target has one or more risk signals:

- modified within the last seven days;
- size is at least 1 GiB;
- shared/global scope;
- modification age is unknown; or
- restoration requires downloading packages again.
- an individually selected scan-only item requires a manual override.

The prompt should summarize the batch, state the concrete reasons, and accept a
simple `y`/`n` answer. It is one confirmation for the operation, not one prompt
per selected cache.
Scan-only items remain excluded from focused deletion, visible-batch selection,
and CLI cleanup. A user may select one individually with `Space`; this deliberate
action unlocks deletion only after the prompt clearly states `manual override`.

This UX policy is separate from filesystem correctness. On `d`, CacheFerret
remeasures the item before choosing whether to prompt. Immediately before
removal it checks identity, containment, ownership markers, kind, and symlink
policy again. A target that changed or became unsafe should produce an honest
conflict instead of being removed.

## Product boundaries

In scope:

- Recognized project and shared caches across supported developer ecosystems.
- Recognized macOS temporary build caches and diagnostic visibility into large
  temporary project workspaces.
- Disk-usage discovery, filtering, sorting, inspection, and focused deletion.
- Safe batch preview/cleanup for scripts.
- Human-readable terminal output and clispec.dev v0.3 JSON behavior.
- macOS and Linux, on x86_64 and aarch64.

Out of scope unless deliberately designed later:

- General arbitrary-directory deletion.
- Following symlinks.
- Implicitly or batch-selecting state that may be the only copy of user-created
  artifacts; scan-only entries require individual selection and confirmation.
- Treating arbitrary system-temporary contents as caches; uncertain project
  workspaces remain scan-only even when they live under `/private/tmp`.
- Treating Docker storage as ordinary directories. Docker cleanup needs a
  Docker-aware integration with native sizing and prune semantics.
- Claiming Windows support before its discovery rules, terminal behavior,
  tests, and packaging are intentionally supported.

## Quality bar

A release is ready when the behavior is pleasant for a human and predictable
for automation. At minimum:

- formatting, Clippy, unit/integration tests, and PTY tests pass;
- clispec.dev conformance remains 24/24;
- dependency audit is clean or every exception is explicitly understood;
- Cargo package and Python artifacts validate;
- public-registry installs execute and report the expected version;
- narrow, color-limited, no-color, ASCII, reduced-motion, and interrupted TUI
  sessions remain usable and restore the terminal correctly;
- release documentation and actual authentication/automation behavior agree.

## Current distribution

Version 0.2.1 is published on
[crates.io](https://crates.io/crates/cacheferret/0.2.1) and
[PyPI](https://pypi.org/project/cacheferret/0.2.1/). Users can install it with:

```sh
cargo install cacheferret
pipx install cacheferret
```

PyPI packages the native Rust executable through Maturin. The current artifact
matrix contains macOS and manylinux2014 wheels for Intel and ARM, plus an sdist.
Homebrew and GitHub archive automation exist in the repository, but their public
availability must be verified separately rather than inferred from the workflow
files.

## Near-term opportunities

- Publish and verify the GitHub source repository, tag, release archives, and
  Homebrew tap when explicitly authorized and correctly credentialed.
- Exercise release installs on clean Intel/ARM macOS and Linux environments.
- Continue polishing navigation, focus continuity, responsive layouts, copy,
  and visual identity based on real terminal use.
- Design Docker cleanup as an explicit integration instead of weakening the
  closed cache catalog.
- Consider Windows only as a complete platform effort, not just an extra wheel.
