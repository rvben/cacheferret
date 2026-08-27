# CacheFerret product direction

<!-- impeccable:product-schema 1 -->

## Platform

terminal

## Users

- Developers reclaiming space during normal local work.
- Power users who expect ncdu/gdu-style keyboard speed.
- Teams and automation that need deterministic, inspectable JSON behavior.
- Coding agents that need an offline schema and stable confirmation and error
  contracts before invoking a destructive command.

## Product Purpose

CacheFerret helps developers quickly answer two questions:

1. Where did my disk space go?
2. Which rebuildable caches can I remove right now?

Success means a developer can open CacheFerret, understand the largest
rebuildable consumers, focus or select the right targets, and reclaim space
without learning filesystem-cleanup internals or accepting blanket risk.

## Positioning

CacheFerret is a disk-space tool for developers, not a general-purpose
filesystem cleaner. Its advantage comes from a closed, reviewable catalog that
understands developer ecosystems, distinguishes cache scope and restoration
cost, and refuses to present uncertain or potentially irreplaceable state as
ordinary disposable data.

The product has two equally important faces: a fast keyboard-first workspace
for humans and a clispec.dev v0.3 command surface for scripts and agents. A
neighboring disk analyzer cannot truthfully copy this combination without the
same ecosystem recognition, proportional deletion policy, and last-moment
filesystem validation.

## Operating Context

CacheFerret runs as a native Rust terminal application on macOS and Linux. The
`terminal` platform value intentionally extends Impeccable's web/mobile
vocabulary because classifying this product as web, iOS, Android, or adaptive
would misdirect future interface work.

Users run it among source trees, package-manager stores, compiler output,
virtual environments, and recognized temporary build storage. Bare
`cacheferret` opens the TUI when stdin and stdout are terminals; when piped, the
same command performs a read-only JSON scan. Explicit `tui`, `scan`, `clean`,
`catalog`, `docker`, `docker clean`, and `schema` commands support focused human
and automated workflows.

The TUI follows a familiar ncdu/gdu interaction model:

| Intent | Interaction |
| --- | --- |
| Move | Arrow keys or `j`/`k` |
| Select/unselect and advance | `Space` |
| Select/unselect all visible cleanable storage | `a` |
| Clean selection, or focused item | `d` |
| Confirm a risky cleanup | `y`; cancel with `n` or `Esc` |
| Filter | `/` |
| Cycle all/project/global scope | `Tab` |
| Cycle size/age/name sorting | `s` |
| Help | `?` |
| Quit | `q` |

## Capabilities and Constraints

### Experience contract

The interface prioritizes the cache list, apparent size, age, scope,
ecosystem, and a useful focused-item explanation. Repeated `Space` presses
build a batch while advancing, and `a` operates on the current filtered and
scoped view.

Background work remains visible and never freezes input. Recognized caches
appear immediately as sizing rows, completed measurements become browsable as
they arrive, and rescans preserve the previous workspace until the fresh
snapshot is ready. Before the user interacts, focus follows the leading
measured result. Navigation, filtering, or selection transfers focus ownership
to the user, after which scan updates preserve the focused path while rows
reorder. Focus never implies cleanup selection, and caches are never selected
automatically.

Warm starts use a small private snapshot as a scheduling index, never as an
authority. Previously known paths appear immediately in prior-size order with
approximate values and an explicit refreshing state. They cannot be selected
or deleted until current catalog recognition and a fresh tree measurement
finish. Known paths are measured first, then fixed global locations, while the
complete project-root crawl continues to find new caches. Successful scans
replace the covered snapshot atomically; stale, corrupt, oversized, or
version-mismatched state is ignored. `CACHEFERRET_NO_CACHE` disables this
accelerator.

Empty, filtered-empty, scanning, deleting, success, conflict, and failure
states require intentional copy and layout. The TUI is the product experience,
not a decorative wrapper around the CLI.

When global storage is in scope and cache-kind filters are not active, Docker
inspection runs independently of the filesystem scan. Images, containers,
volumes, and build cache appear after filesystem cache rows, ordered by
Docker-reported reclaimable bytes. They use daemon scope, never fake paths,
and always keep their action visible in compact layouts. Ordinary build cache
is the only selectable Docker resource; images, containers, volumes, and
broader internal/frontend build records remain inspection-only. Build-cache
cleanup always requires confirmation, refreshes Docker's estimate before the
prompt and immediately before mutation, and runs only
`docker builder prune --force` without `--all`. Mixed filesystem and
build-cache selections use one confirmation while keeping their measurements
separate. Missing, stopped, remote, permission-denied, slow, or malformed
Docker responses degrade to bounded diagnostics without failing the filesystem
scan.

### Storage accounting

The TUI, text, and JSON interfaces distinguish three concepts:

- Apparent bytes are logical file lengths, with hard links counted once.
- Allocated bytes are filesystem blocks attributed to the tree. They are not
  guaranteed reclaimable space because filesystems such as APFS may share
  cloned extents.
- Observed free-space change is sampled around a real deletion. It remains a
  per-filesystem net measurement because concurrent activity, snapshots,
  compression, and delayed reclamation can affect it.

Free-space changes must not be summed across volumes that may share one APFS
container.

Native providers report their own total usage and potentially reclaimable
bytes. Those values remain a separate accounting layer and must not be folded
into filesystem apparent, allocated, or observed-free-space totals.

### Deletion and proportional confirmation

Deletion stays direct. With a selection, `d` reviews and deletes the batch;
without one, it acts on the focused cache. Low-risk cleanup should be one
keypress. CacheFerret asks for one compact `y`/`n` confirmation only when a
freshly reviewed operation includes one or more risk signals:

- modified within the last seven days;
- apparent size of at least 1 GiB;
- shared/global scope;
- unknown modification age;
- restoration that requires another package download; or
- an individually selected scan-only item requiring a manual override.

Scan-only entries remain excluded from focused deletion, visible-batch
selection, and CLI cleanup. A user may select one individually with `Space`;
that deliberate action permits deletion only after the prompt clearly names
the manual override.

Pressing `d` remeasures the whole operation before deciding whether to prompt.
Immediately before removal, CacheFerret revalidates each path, filesystem
identity, containment, kind, ownership markers, and symlink policy. Temporary
trees that require quiescence must also match their fresh safety fingerprint.
A changed or unsafe target produces a conflict instead of being removed.

Docker build-cache cleanup is deliberately stricter than ordinary low-risk
directory cleanup because it is shared daemon state and restoration may require
downloads. It always asks once, even for a small estimate. The CLI equivalent
is `cacheferret docker clean`; non-interactive callers must use `--yes`, and
agents can preview the same output contract with `--dry-run`.

### Product boundaries

In scope:

- Recognized project and shared caches across supported developer ecosystems.
- Recognized macOS temporary build caches and diagnostic visibility into large
  temporary project workspaces.
- Disk-usage discovery, filtering, sorting, inspection, focused deletion, and
  rapid batch selection.
- Safe preview and cleanup for scripts.
- Docker storage inspection plus guarded ordinary build-cache pruning in the
  TUI, text output, and JSON.
- Human-readable terminal output and clispec.dev v0.3 JSON behavior.
- macOS and Linux on x86_64 and aarch64.

Out of scope unless deliberately designed later:

- General arbitrary-directory deletion.
- Following symlinks.
- Implicitly or batch-selecting state that may be the only copy of user-created
  artifacts.
- Treating arbitrary system-temporary contents as caches.
- Treating Docker storage as ordinary directories; pruning images, containers,
  volumes, or internal/frontend build records; or running broad system prune.
- Claiming Windows support before discovery rules, terminal behavior, tests,
  and packaging are intentionally supported.

Maven's local repository and the shared renv cache are scan-only because they
may contain or back state that is not safely reproducible.

## Brand Commitments

CacheFerret is the established public product name. Its voice is concise,
direct, technically precise, and quietly personable. Ferret language may add a
small amount of character to benign discovery states, but it must never obscure
risk, measurements, recovery cost, or the exact action being taken.

The existing logo concept is stored at `assets/cacheferret-logo-concept.png`.
It is evidence of the current identity, not approval to fabricate additional
brand claims or declare an unreviewed replacement final.

## Evidence on Hand

- `README.md` documents the public promise, command surface, safety model,
  supported cache catalog, installation methods, and storage terminology.
- `src/catalog.rs`, `src/scanner.rs`, `src/cleaner.rs`, `src/docker.rs`, and
  `src/tui.rs` are the
  behavioral source of truth for recognition, measurement, mutation safety,
  and the interactive workspace.
- `cacheferret schema` and `src/schema.rs` expose the offline machine contract.
- Unit, integration, PTY, conformance, packaging, and release checks exercise
  the claimed behavior.
- `docs/native-cleanup.md` records the bounded design for future native cleanup
  adapters.
- Version 0.5.0 is published through crates.io, PyPI, GitHub Releases, and the
  Homebrew tap, with native macOS and Linux artifacts described in the README.
- No testimonials, customer logos, usage analytics, or performance claims are
  approved evidence; future work must not invent them.

## Product Principles

1. **Directness is part of safety.** Keep focused deletion immediate while
   preserving rapid keyboard batch selection.
2. **Guardrails are proportional and contextual.** Apply friction only when a
   freshly measured operation carries a concrete risk signal.
3. **Fresh filesystem truth outranks remembered state.** Progressive results
   and warm-start hints improve responsiveness, but mutation always depends on
   current recognition, measurement, and last-moment validation.
4. **Human and machine interfaces are peers.** TUI improvements must preserve
   deterministic JSON, schema introspection, pagination, field selection,
   stream separation, and non-interactive confirmation contracts.
5. **Be precise about what is rebuildable.** Uncertain or potentially unique
   state stays scan-only, and native systems such as Docker require native
   integration rather than convenient directory deletion.

## Accessibility & Inclusion

CacheFerret is keyboard-first and must remain usable without a mouse. Terminal
capability is a supported product constraint: truecolor, 256-color, basic ANSI,
`NO_COLOR`, ASCII, non-UTF-8 locales, reduced motion, narrow windows, and PTY
lifecycle behavior all require deliberate support. Color cannot be the only
carrier of selection, status, progress, warning, or danger. Copy and controls
must remain legible at the minimum supported terminal size.

## Quality Bar

A release is ready when behavior is pleasant for a human and predictable for
automation. At minimum:

- formatting, strict Clippy, unit and integration tests, and PTY tests pass;
- clispec.dev conformance remains 24/24;
- dependency audit findings are clean or explicitly understood;
- Cargo and Python packages validate;
- public-registry installation checks execute the expected version;
- narrow, color-limited, no-color, ASCII, reduced-motion, and interrupted TUI
  sessions remain usable and restore the terminal correctly; and
- release documentation and actual authentication and automation behavior
  agree.

## Current Distribution

Version 0.5.0 is published on crates.io, PyPI, GitHub Releases, and the Homebrew
tap. PyPI packages the native Rust executable through Maturin and provides
macOS and manylinux2014 wheels for Intel and ARM plus an sdist. Tag-driven
publication fails before building when a required publishing credential is
absent. Release readiness is checked locally and in GitHub Actions, and public
availability is verified after publication rather than inferred from a
successful build.

## Near-Term Opportunities

- Keep the public-install smoke matrix green across Intel and ARM macOS and
  Linux for GitHub archives, crates.io, PyPI, and Homebrew.
- Continue polishing navigation, focus continuity, responsive terminal
  layouts, copy, and visual identity based on real terminal use.
- Evaluate whether any additional native cleanup class can be bounded as
  precisely as Docker's ordinary build cache; keep images, containers, and
  volumes inspection-only until that evidence exists.
- Consider Windows only as a complete platform effort, not merely another
  wheel target.
