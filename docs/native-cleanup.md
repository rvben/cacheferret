# Native cleanup integrations

CacheFerret currently deletes recognized directory candidates itself. It
remeasures the selected tree, checks its identity and ownership markers, and
then removes exactly that path. It does not currently invoke package-manager or
daemon cleanup commands.

Docker is the first read-only native adapter. `cacheferret docker` invokes a
bounded `docker system df --format json` inspection and exposes the result in
text, JSON, the offline schema, and the TUI. It does not implement Docker
revalidation or cleanup yet.

That remains the right default for disposable build trees such as Cargo
`target/`, `node_modules`, CMake build directories, and Python bytecode. The
path is the unit the user inspected and selected. A project-level native clean
command may follow workspace configuration, resolve a different output path, or
remove more than the selected candidate.

Native cleanup is preferable when another program owns the storage as a
database, content-addressed store, or daemon-managed resource. CacheFerret must
use that owner's semantics rather than deleting its internal files.

## Decision rule

| Strategy | Use when | Safety requirement |
| --- | --- | --- |
| Exact path deletion | A closed-catalog directory is wholly rebuildable and the selected path is the cleanup boundary | Preserve the current identity, containment, ownership, symlink, and final-fingerprint checks |
| Native prune adapter | A package manager or daemon tracks references, shared layers, or garbage-collection state | Preview and clean through argv-based native commands; never emulate the operation with filesystem deletion |
| Diagnostic only | State may be unique, shared with projects, or lacks a bounded cleanup operation | Report it as scan-only and require a deliberately designed integration before enabling cleanup |

Native does not automatically mean safer. A broad command such as “clear all”
can exceed the item selected in CacheFerret. An adapter is eligible only when
its preview and cleanup boundaries can be represented honestly in the TUI,
text output, and JSON schema.

## Existing native opportunities

| Storage owner | Native inspection or cleanup | CacheFerret direction |
| --- | --- | --- |
| Docker | [`docker system df`](https://docs.docker.com/reference/cli/docker/system/df/), plus resource-specific `docker builder/image/container/volume prune` commands | Native integration required. Never delete Docker's data root. Prefer separately selectable resource classes over a default `docker system prune --volumes`. |
| Homebrew | [`brew cleanup --dry-run` and `brew cleanup`](https://docs.brew.sh/Manpage#cleanup-options-formulacask) | Good native-prune candidate because Homebrew owns download, version, and lock-file state. Keep autoremove separate from cache cleanup. |
| pip | [`pip cache info`, `remove`, and `purge`](https://pip.pypa.io/en/stable/cli/pip_cache/) | A whole-cache action can use pip. Exact subdirectories can remain direct only while their boundaries are stable and validated. |
| uv | [`uv cache clean` and `uv cache prune`](https://docs.astral.sh/uv/concepts/cache/#clearing-the-cache) | Prefer `prune` for garbage collection; expose full clean as the broader, download-backed action. |
| pnpm | [`pnpm store status` and `pnpm store prune`](https://pnpm.io/cli/store) | Strong native-prune candidate because the content-addressed store owns package references. |
| npm | [`npm cache verify`](https://docs.npmjs.com/cli/v11/commands/npm-cache) | Prefer verification/advice. npm describes its cache as self-healing, and force-cleaning the entire cache is rarely a useful default. |
| NuGet | [`dotnet nuget locals`](https://learn.microsoft.com/dotnet/core/tools/dotnet-nuget-locals) | Native clear is viable, but its named resource classes must be shown separately instead of collapsing `all` into one opaque target. |
| Cargo project output | [`cargo clean`](https://doc.rust-lang.org/cargo/commands/cargo-clean.html) | Keep exact-path deletion by default. Cargo can resolve workspace and configured target directories beyond the focused candidate. |

Other ecosystems should be added only after the same boundary analysis. The
existence of a `clean` command is not enough: it must identify what it will
remove, support non-interactive execution, and avoid unrelated user state.

## Adapter contract

A native adapter must provide four explicit operations:

1. `availability`: detect the executable or daemon without turning absence into
   a scan failure.
2. `inspect`: return stable resource identities, human labels, apparent or
   native-reported usage, and reclaimable estimates without mutation.
3. `revalidate`: immediately refresh selected identities and impact before the
   confirmation decision and again before mutation.
4. `clean`: invoke a bounded non-interactive native operation and return the
   owner's reported result, exit status, and diagnostics.

Implementations must invoke executables with argument arrays, never a shell
command string. They need timeouts, output bounds, cancellation behavior, and
fixture-driven parsers. Human-oriented command output is not a stable API; use
machine-readable output where the owner provides it and gate unsupported
versions or capabilities honestly.

Storage reporting keeps its existing layers and adds native accounting without
conflating them:

- native-reported usage and reclaimable bytes before cleanup;
- native-reported bytes removed, when the owner supplies them; and
- observed host-filesystem free-space change after cleanup.

The observed delta remains the strongest local measurement but can still lag
because of APFS snapshots, shared extents, daemon compaction, and concurrent
disk activity.

## Docker delivery sequence

Docker is the first adapter because its storage is both large and unsafe to
treat as ordinary directories.

1. Read-only detection and `docker system df` inspection. A stopped,
   missing, remote, or permission-denied daemon becomes a bounded diagnostic.
2. Build cache, images, containers, and volumes are represented as
   distinct native resources with native-reported total and reclaimable bytes.
3. JSON and TUI presentation ship before mutation. Daemon resources never use
   fake filesystem paths and remain non-selectable.
4. Add resource-specific prune operations with refreshed previews and the same
   proportional confirmation policy used for directory candidates.
5. Consider a combined system prune only as an explicit advanced action;
   volumes remain separately selected and confirmed.

This requires a native-resource model alongside `CacheCandidate`, not an
exception that weakens the directory scanner's closed catalog.
