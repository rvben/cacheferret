# Native cleanup integrations

CacheFerret deletes recognized directory candidates itself. It uses the scan's
tree measurement, checks the selected path's identity and ownership markers,
and then removes exactly that path. Docker ordinary build cache is the first
daemon-owned exception and uses Docker's own bounded prune operation.

Docker is the first native adapter. `cacheferret docker` invokes a
bounded `docker system df --format json` inspection and exposes the result in
text, JSON, the offline schema, and the TUI. Inspection allows up to 60 seconds
for large or VM-backed inventories; the TUI keeps filesystem actions available
and shows a persistent pending status. Timeouts are reported explicitly and
can be retried with `r`. `cacheferret docker clean` refreshes
the ordinary build-cache estimate, always requires confirmation unless `--yes`
is explicit, revalidates again, and invokes only `docker builder prune --force`.
It never uses `--all` or any system-, image-, container-, or volume-prune action.

`cacheferret docker clean --unused-images` reads
`docker system df --verbose --format json` and offers every tagged image whose
container count is zero, largest unique size first. Untagged images and images
whose count cannot be read are never offered. After confirmation each image is
looked up again in a fresh listing and removed only if the same reference still
names the same image ID with no container, one `docker image rm -- <ref>` per
image and never `--force`. Docker's own refusal, a changed ID, or a new
container keeps the image and is reported with its reason. Unique sizes are
summed once per image ID, so an image with two tags is not counted twice.

When the active Docker endpoint (`$DOCKER_HOST`, else `docker context inspect`)
is a Colima socket (`.../.colima/<profile>/docker.sock`), a successful prune or
image removal is
followed by `colima ssh --profile <profile> -- sudo fstrim -v /var/lib/docker`.
Colima mounts the Docker data disk without discard, so without the trim the
space freed inside the VM is not returned to the host disk. The trim is bounded
by a timeout and its failure is reported as a `trim_failed` diagnostic while the
prune itself still counts as done. The byte count in the report
(`trim_reported_bytes`) is what `fstrim -v` says it submitted for discard; it can
include extents discarded earlier, so it is not a measure of host space regained. Docker Desktop, OrbStack and other runtimes
get no trim.

Rust toolchains and Xcode simulator devices and runtimes follow the same
native pattern. These candidates carry no filesystem identity: each is
resolved again through its owning tool immediately before removal, and the
removal is refused when the tool no longer reports it as unused. Runtime
deletion can complete asynchronously inside Xcode services, so the runtime
list is queried again after the command. A runtime that is no longer listed
counts as removed; one that is still listed, or whose state cannot be
confirmed, is reported in `deferred_paths` and its size is not counted as
freed. The observed free-space change is reported separately either way.

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
| Docker | [`docker system df`](https://docs.docker.com/reference/cli/docker/system/df/), plus resource-specific `docker builder/image/container/volume prune` commands | Ordinary build-cache prune and per-image removal of unused tagged images are implemented. Never delete Docker's data root; keep containers, volumes, and untagged images inspection-only until each boundary is designed. |
| Homebrew | [`brew cleanup --dry-run` and `brew cleanup`](https://docs.brew.sh/Manpage#cleanup-options-formulacask) | Good native-prune candidate because Homebrew owns download, version, and lock-file state. Keep autoremove separate from cache cleanup. |
| pip | [`pip cache info`, `remove`, and `purge`](https://pip.pypa.io/en/stable/cli/pip_cache/) | A whole-cache action can use pip. Exact subdirectories can remain direct only while their boundaries are stable and validated. |
| uv | [`uv cache clean` and `uv cache prune`](https://docs.astral.sh/uv/concepts/cache/#clearing-the-cache) | Prefer `prune` for garbage collection; expose full clean as the broader, download-backed action. |
| pnpm | [`pnpm store status` and `pnpm store prune`](https://pnpm.io/cli/store) | Strong native-prune candidate because the content-addressed store owns package references. |
| npm | [`npm cache verify`](https://docs.npmjs.com/cli/v11/commands/npm-cache) | Prefer verification/advice. npm describes its cache as self-healing, and force-cleaning the entire cache is rarely a useful default. |
| NuGet | [`dotnet nuget locals`](https://learn.microsoft.com/dotnet/core/tools/dotnet-nuget-locals) | Native clear is viable, but its named resource classes must be shown separately instead of collapsing `all` into one opaque target. |
| rustup | `rustup toolchain list -v`, `rustup show home`, `rustup toolchain uninstall` | Implemented. Each installed toolchain is a candidate unless it is the default, a directory override, or `RUSTUP_TOOLCHAIN`, whether selected by name or by absolute directory path (compared by canonical path). The in-use set is read from `settings.toml`, and when that file is missing or not understood, or a selection is neither a name nor an absolute path, no toolchain is offered and a warning says so. Symlinked (linked) toolchains are skipped. |
| Xcode simulator devices | `xcrun simctl list devices --json`, `xcrun simctl delete <udid>` | Implemented. Size is the device directory. Only devices with at least one installed app (a non-empty `data/Containers/Bundle/Application`) are listed, since a pristine device only removes an Xcode destination. Age is the newest modification time of a regular file under `data/`; directory times are ignored because CoreSimulator rewrites them, and a device with no files has an unknown age. Only `Shutdown` devices are cleanable, re-queried right before deletion. |
| Xcode simulator runtimes | `xcrun simctl runtime list -j`, `xcrun simctl runtime delete <id>` | Implemented. Size is the `sizeBytes` simctl reports and age is `lastUsedAt` (unknown when absent). Only runtimes marked `deletable` are cleanable, and a runtime is refused while any non-shut-down device uses it. A runtime is unprotected regardless of its age when a strictly newer runtime for the same platform is installed (versions compared numerically) and every device on it is shut down, has a trusted data path, and holds no apps; the scan names the newer runtime as the reason. Restoring one means a download. |
| Cargo project output | [`cargo clean`](https://doc.rust-lang.org/cargo/commands/cargo-clean.html) | Keep exact-path deletion by default. Cargo can resolve workspace and configured target directories beyond the focused candidate. The `incremental` directory of each profile is its own candidate; its removal holds the profile's Cargo build locks and is refused while a build holds them. |

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
   fake filesystem paths.
4. Ordinary build cache is separately selectable and uses refreshed previews,
   mandatory confirmation, final native revalidation, and the bounded
   `docker builder prune --force` operation. This is implemented.
5. Unused tagged images are removable from the CLI, one confirmed image at a
   time with a per-image recheck. This is implemented.
6. Containers, volumes, untagged images, and `builder prune --all` records
   remain inspection-only. A combined system prune is outside the current
   product boundary.

This requires a native-resource model alongside `CacheCandidate`, not an
exception that weakens the directory scanner's closed catalog.
