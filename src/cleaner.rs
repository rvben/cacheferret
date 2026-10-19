use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::model::{FilesystemSpaceDelta, SkippedPath};
use crate::native::{self, Removal};
use crate::scanner::revalidate_for_delete;
use crate::{CacheCandidate, CleanReport, outermost};

/// Revalidate and remove a batch of candidates.
///
/// Every candidate is checked immediately before deletion. A refused item does
/// not abort the rest of the batch and is reported with its reason.
pub fn clean_candidates(candidates: &[CacheCandidate], dry_run: bool) -> CleanReport {
    clean_candidates_with_probe(candidates, dry_run, filesystem_space)
}

#[derive(Debug, Clone, Copy)]
struct FilesystemSpace {
    id: u64,
    available_bytes: u64,
}

#[derive(Debug)]
struct PendingMeasurement {
    probe_path: PathBuf,
    free_bytes_before: u64,
}

fn clean_candidates_with_probe<F>(
    candidates: &[CacheCandidate],
    dry_run: bool,
    mut probe: F,
) -> CleanReport
where
    F: FnMut(&Path) -> Option<FilesystemSpace>,
{
    let policy_skipped = candidates
        .iter()
        .filter(|candidate| !candidate.cleanable)
        .count();
    // A selected cache inside another selected cache is removed with it, so
    // only the outermost one is acted on and counted.
    let candidates: Vec<&CacheCandidate> =
        outermost(candidates.iter().filter(|candidate| candidate.cleanable));
    let selected_targets = candidates
        .iter()
        .map(|candidate| (*candidate).into())
        .collect();
    let bytes_selected = candidates.iter().map(|candidate| candidate.bytes).sum();
    let allocated_bytes_selected = candidates
        .iter()
        .map(|candidate| candidate.allocated_bytes)
        .sum();
    if dry_run {
        return CleanReport {
            changed: false,
            dry_run: true,
            confirmed: false,
            selected: candidates.len(),
            cleaned: 0,
            skipped: 0,
            protected_skipped: 0,
            policy_skipped,
            network_restore_selected: candidates
                .iter()
                .filter(|candidate| candidate.network_restore)
                .count(),
            bytes_selected,
            apparent_bytes_selected: bytes_selected,
            allocated_bytes_selected,
            bytes_reclaimed_estimate: 0,
            apparent_bytes_removed: 0,
            allocated_bytes_removed_estimate: 0,
            filesystem_deltas: Vec::new(),
            selected_targets,
            cleaned_paths: Vec::new(),
            deferred_paths: Vec::new(),
            skipped_paths: Vec::new(),
        };
    }

    let mut cleaned_paths = Vec::new();
    let mut deferred_paths = Vec::new();
    let mut skipped_paths = Vec::new();
    let mut reclaimed = 0_u64;
    let mut allocated_removed = 0_u64;
    let mut measurements = HashMap::new();
    let mut candidate_filesystems = HashMap::new();
    for candidate in &candidates {
        let probe_path = candidate.path.parent().unwrap_or(candidate.path.as_path());
        if let Some(space) = probe(probe_path) {
            candidate_filesystems.insert(candidate.path.clone(), space.id);
            measurements
                .entry(space.id)
                .or_insert_with(|| PendingMeasurement {
                    probe_path: probe_path.to_path_buf(),
                    free_bytes_before: space.available_bytes,
                });
        }
    }
    let mut cleaned_filesystems = HashSet::new();

    for candidate in &candidates {
        if let Err(reason) = revalidate_for_delete(candidate) {
            skipped_paths.push(SkippedPath {
                path: candidate.path.clone(),
                reason,
            });
            continue;
        }
        let removed = match with_cargo_build_locks(candidate, || remove_candidate(candidate)) {
            Ok(removed) => removed,
            Err(reason) => {
                skipped_paths.push(SkippedPath {
                    path: candidate.path.clone(),
                    reason,
                });
                continue;
            }
        };
        match removed {
            Ok(removal) => {
                if removal == Removal::Complete {
                    reclaimed = reclaimed.saturating_add(candidate.bytes);
                    allocated_removed = allocated_removed.saturating_add(candidate.allocated_bytes);
                } else {
                    deferred_paths.push(candidate.path.clone());
                }
                if let Some(id) = candidate_filesystems.get(&candidate.path) {
                    cleaned_filesystems.insert(*id);
                }
                cleaned_paths.push(candidate.path.clone());
            }
            Err(error) => skipped_paths.push(SkippedPath {
                path: candidate.path.clone(),
                reason: error.to_string(),
            }),
        }
    }

    let mut filesystem_deltas = cleaned_filesystems
        .into_iter()
        .filter_map(|id| {
            let before = measurements.get(&id)?;
            let after = probe(&before.probe_path)?;
            if after.id != id {
                return None;
            }
            Some(FilesystemSpaceDelta {
                probe_path: before.probe_path.clone(),
                free_bytes_before: before.free_bytes_before,
                free_bytes_after: after.available_bytes,
                delta_bytes: signed_delta(after.available_bytes, before.free_bytes_before),
            })
        })
        .collect::<Vec<_>>();
    filesystem_deltas.sort_by(|left, right| left.probe_path.cmp(&right.probe_path));

    CleanReport {
        changed: !cleaned_paths.is_empty(),
        dry_run: false,
        confirmed: true,
        selected: candidates.len(),
        cleaned: cleaned_paths.len(),
        skipped: skipped_paths.len(),
        protected_skipped: 0,
        policy_skipped,
        network_restore_selected: candidates
            .iter()
            .filter(|candidate| candidate.network_restore)
            .count(),
        bytes_selected,
        apparent_bytes_selected: bytes_selected,
        allocated_bytes_selected,
        bytes_reclaimed_estimate: reclaimed,
        apparent_bytes_removed: reclaimed,
        allocated_bytes_removed_estimate: allocated_removed,
        filesystem_deltas,
        selected_targets,
        cleaned_paths,
        deferred_paths,
        skipped_paths,
    }
}

/// Lock files Cargo holds exclusively for as long as a build of a profile runs.
const CARGO_LOCK_FILES: [&str; 3] = [".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock"];

/// Run `action` while holding every Cargo build lock that guards a Cargo
/// cache, or refuse without running it when a lock is busy.
///
/// Cargo holds these `flock` locks exclusively in each profile directory for
/// the whole build, rustc included, so a busy lock means a build is using the
/// cache right now. The locks stay held until `action` returns, so a build
/// started meanwhile waits for the removal to finish. An incremental cache is
/// guarded by its own profile's locks, a target directory by the locks of
/// every profile inside it. Other kinds run `action` directly.
fn with_cargo_build_locks<T>(
    candidate: &CacheCandidate,
    action: impl FnOnce() -> T,
) -> Result<T, String> {
    let _held = cargo_build_locks(candidate)?;
    Ok(action())
}

fn cargo_build_locks(candidate: &CacheCandidate) -> Result<Vec<fs::File>, String> {
    let profiles: Vec<PathBuf> = match candidate.kind.as_str() {
        "cargo-incremental" => candidate
            .path
            .parent()
            .into_iter()
            .map(Path::to_path_buf)
            .collect(),
        "cargo-target" => cargo_profile_directories(&candidate.path),
        _ => return Ok(Vec::new()),
    };
    let mut held = Vec::new();
    for profile in profiles {
        for name in CARGO_LOCK_FILES {
            let path = profile.join(name);
            let file = match fs::File::open(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(format!(
                        "cannot open Cargo build lock {}: {error}",
                        path.display()
                    ));
                }
            };
            match file.try_lock() {
                Ok(()) => {}
                Err(fs::TryLockError::WouldBlock) => {
                    return Err("a Cargo build is using this target".to_owned());
                }
                Err(fs::TryLockError::Error(error)) => {
                    return Err(format!(
                        "cannot lock Cargo build lock {}: {error}",
                        path.display()
                    ));
                }
            }
            held.push(file);
        }
    }
    Ok(held)
}

/// Profile directories of a Cargo target directory: `target/<profile>` and
/// `target/<triple>/<profile>`, recognized by their `.cargo-lock`.
fn cargo_profile_directories(target: &Path) -> Vec<PathBuf> {
    let subdirectories = |dir: &Path| -> Vec<PathBuf> {
        fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.path())
            .collect()
    };
    subdirectories(target)
        .into_iter()
        .flat_map(|child| {
            let nested = subdirectories(&child);
            std::iter::once(child).chain(nested)
        })
        .filter(|dir| dir.join(".cargo-lock").is_file())
        .collect()
}

/// Native kinds are removed by the tool that owns them; everything else is a
/// directory that is deleted in place.
fn remove_candidate(candidate: &CacheCandidate) -> io::Result<Removal> {
    if native::handles(&candidate.kind) {
        native::remove(candidate)
    } else {
        remove_cache_directory(candidate).map(|()| Removal::Complete)
    }
}

fn remove_cache_directory(candidate: &CacheCandidate) -> io::Result<()> {
    match fs::remove_dir_all(&candidate.path) {
        Ok(()) => Ok(()),
        Err(original) if is_go_module_tree(candidate) => {
            make_owned_directories_writable(&candidate.path).map_err(|repair| {
                io::Error::new(
                    repair.kind(),
                    format!("{original}; could not prepare read-only Go module cache: {repair}"),
                )
            })?;
            fs::remove_dir_all(&candidate.path)
        }
        Err(error) => Err(error),
    }
}

fn is_go_module_tree(candidate: &CacheCandidate) -> bool {
    if candidate.kind == "go-module-cache" {
        return true;
    }
    if candidate.kind != "macos-temporary-build-cache" {
        return false;
    }
    let name = candidate
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    name.contains("go-mod") || name.contains("gomod")
}

#[cfg(unix)]
fn make_owned_directories_writable(root: &Path) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let root_metadata = fs::symlink_metadata(root)?;
    if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cache root is not a real directory",
        ));
    }
    let owner = root_metadata.uid();
    let mut directories = Vec::new();
    for item in WalkDir::new(root).follow_links(false) {
        let entry = item.map_err(io::Error::other)?;
        if !entry.file_type().is_dir() || entry.file_type().is_symlink() {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.uid() != owner {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Go module cache contains a directory owned by another user",
            ));
        }
        directories.push((entry.path().to_path_buf(), metadata.permissions()));
    }
    for (directory, mut permissions) in directories {
        permissions.set_mode(permissions.mode() | 0o200);
        fs::set_permissions(directory, permissions)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn make_owned_directories_writable(_root: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "read-only Go module cache cleanup is unsupported on this platform",
    ))
}

fn signed_delta(after: u64, before: u64) -> i64 {
    let delta = i128::from(after) - i128::from(before);
    delta.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

#[cfg(unix)]
fn filesystem_space(path: &Path) -> Option<FilesystemSpace> {
    use std::ffi::CString;
    use std::mem::MaybeUninit;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    let id = fs::metadata(path).ok()?.dev();
    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stats = MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is a valid NUL-terminated C string and `stats` points to
    // writable storage initialized by statvfs on success.
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: statvfs returned success and initialized the structure.
    let stats = unsafe { stats.assume_init() };
    let block_size = if stats.f_frsize == 0 {
        stats.f_bsize
    } else {
        stats.f_frsize
    };
    Some(FilesystemSpace {
        id,
        available_bytes: available_blocks(&stats).saturating_mul(block_size),
    })
}

#[cfg(target_os = "macos")]
fn available_blocks(stats: &libc::statvfs) -> u64 {
    u64::from(stats.f_bavail)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn available_blocks(stats: &libc::statvfs) -> u64 {
    stats.f_bavail
}

#[cfg(not(unix))]
fn filesystem_space(_path: &Path) -> Option<FilesystemSpace> {
    None
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use crate::{DiscoveryOptions, ScopeFilter, discover};

    use super::*;

    #[test]
    fn removes_a_revalidated_project_cache() {
        let temp = tempdir().unwrap();
        let project = temp.path().join("demo");
        fs::create_dir_all(project.join("target")).unwrap();
        fs::write(project.join("Cargo.toml"), "[package]\nname='demo'\n").unwrap();
        fs::write(project.join("target/object"), "rebuildable").unwrap();
        let scan = discover(&DiscoveryOptions {
            roots: vec![temp.path().to_path_buf()],
            scope: ScopeFilter::Project,
            kinds: Vec::new(),
            protect_days: 0,
        })
        .unwrap();

        let report = clean_candidates(&scan.candidates, false);

        assert!(report.changed);
        assert_eq!(report.cleaned, 1);
        assert!(!project.join("target").exists());
        assert!(project.join("Cargo.toml").exists());
    }

    /// The lock files Cargo 1.96 creates in every profile directory and holds
    /// exclusively during a build.
    const BUILD_LOCKS: [&str; 3] = [".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock"];

    /// A Cargo project with a host profile and a cross-compiled profile, each
    /// holding Cargo's build lock files and an incremental cache.
    fn cargo_project(root: &Path) -> PathBuf {
        let project = root.join("demo");
        let target = project.join("target");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("Cargo.toml"), "[package]\nname='demo'\n").unwrap();
        for profile in [
            target.join("debug"),
            target.join("aarch64-apple-darwin/debug"),
        ] {
            fs::create_dir_all(profile.join(".fingerprint")).unwrap();
            fs::create_dir_all(profile.join("incremental/demo-1")).unwrap();
            for lock in BUILD_LOCKS {
                fs::write(profile.join(lock), b"").unwrap();
            }
            fs::write(
                profile.join("incremental/demo-1/query-cache.bin"),
                [1_u8; 64],
            )
            .unwrap();
        }
        target
    }

    fn scan(root: &Path, kinds: &[&str]) -> Vec<CacheCandidate> {
        discover(&DiscoveryOptions {
            roots: vec![root.to_path_buf()],
            scope: ScopeFilter::Project,
            kinds: kinds.iter().map(|kind| (*kind).to_owned()).collect(),
            protect_days: 0,
        })
        .unwrap()
        .candidates
    }

    /// Hold one of Cargo's lock files the way a running build does.
    fn hold_build_lock(path: &Path) -> fs::File {
        let file = fs::File::open(path).unwrap();
        file.try_lock().unwrap();
        file
    }

    #[test]
    fn removes_an_idle_incremental_cache_and_keeps_the_rest_of_the_target() {
        let temp = tempdir().unwrap();
        let target = cargo_project(temp.path());
        let candidates = scan(temp.path(), &["cargo-incremental"]);
        assert_eq!(candidates.len(), 2);

        let report = clean_candidates(&candidates, false);

        assert_eq!(report.cleaned, 2, "{:?}", report.skipped_paths);
        for profile in [
            target.join("debug"),
            target.join("aarch64-apple-darwin/debug"),
        ] {
            assert!(!profile.join("incremental").exists());
            assert!(profile.join(".fingerprint").is_dir());
            assert!(profile.join(".cargo-lock").is_file());
        }
    }

    #[test]
    fn refuses_an_incremental_cache_while_any_build_lock_of_its_profile_is_held() {
        for lock in BUILD_LOCKS {
            let temp = tempdir().unwrap();
            let target = cargo_project(temp.path());
            let busy = target.join("aarch64-apple-darwin/debug");
            let candidates = scan(temp.path(), &["cargo-incremental"]);
            let _build = hold_build_lock(&busy.join(lock));

            let report = clean_candidates(&candidates, false);

            assert_eq!(report.cleaned, 1, "{lock}");
            assert_eq!(report.skipped_paths.len(), 1, "{lock}");
            assert_eq!(
                report.skipped_paths[0].reason,
                "a Cargo build is using this target"
            );
            assert!(
                report.skipped_paths[0]
                    .path
                    .ends_with("aarch64-apple-darwin/debug/incremental")
            );
            assert!(
                busy.join("incremental/demo-1/query-cache.bin").is_file(),
                "{lock}"
            );
            assert!(!target.join("debug/incremental").exists(), "{lock}");
        }
    }

    #[test]
    fn a_build_lock_released_before_the_clean_does_not_block_it() {
        let temp = tempdir().unwrap();
        let target = cargo_project(temp.path());
        let candidates = scan(temp.path(), &["cargo-incremental"]);
        drop(hold_build_lock(&target.join("debug/.cargo-lock")));

        let report = clean_candidates(&candidates, false);

        assert_eq!(report.cleaned, 2, "{:?}", report.skipped_paths);
    }

    #[test]
    fn refuses_a_cargo_target_while_a_build_of_any_profile_holds_its_lock() {
        let temp = tempdir().unwrap();
        let target = cargo_project(temp.path());
        let candidates = scan(temp.path(), &["cargo-target"]);
        let build = hold_build_lock(&target.join("aarch64-apple-darwin/debug/.cargo-build-lock"));

        let refused = clean_candidates(&candidates, false);
        assert_eq!(refused.cleaned, 0);
        assert_eq!(
            refused.skipped_paths[0].reason,
            "a Cargo build is using this target"
        );
        assert!(target.join("debug/incremental").is_dir());

        drop(build);
        let report = clean_candidates(&candidates, false);
        assert_eq!(report.cleaned, 1, "{:?}", report.skipped_paths);
        assert!(!target.exists());
    }

    #[test]
    fn build_locks_stay_held_until_the_removal_returns() {
        let temp = tempdir().unwrap();
        let target = cargo_project(temp.path());
        let candidates = scan(temp.path(), &["cargo-incremental", "cargo-target"]);
        for candidate in &candidates {
            let profile = if candidate.kind == "cargo-incremental" {
                candidate.path.parent().unwrap().to_path_buf()
            } else {
                target.join("aarch64-apple-darwin/debug")
            };
            let during = with_cargo_build_locks(candidate, || {
                BUILD_LOCKS.map(|lock| {
                    matches!(
                        fs::File::open(profile.join(lock)).unwrap().try_lock(),
                        Err(fs::TryLockError::WouldBlock)
                    )
                })
            })
            .unwrap();
            assert_eq!(during, [true; 3], "{}", candidate.path.display());
            for lock in BUILD_LOCKS {
                drop(hold_build_lock(&profile.join(lock)));
            }
        }
    }

    #[test]
    fn other_kinds_run_without_cargo_locks() {
        let temp = tempdir().unwrap();
        let project = temp.path().join("web");
        fs::create_dir_all(project.join("node_modules/pkg")).unwrap();
        fs::write(project.join("package.json"), "{}").unwrap();
        let candidates = scan(temp.path(), &[]);

        assert_eq!(with_cargo_build_locks(&candidates[0], || 7), Ok(7));
    }

    #[test]
    fn a_selected_cache_inside_another_selected_cache_is_removed_once_with_it() {
        let temp = tempdir().unwrap();
        let target = cargo_project(temp.path());
        let candidates = scan(temp.path(), &[]);
        assert_eq!(candidates.len(), 3);
        let outer = candidates
            .iter()
            .find(|candidate| candidate.kind == "cargo-target")
            .unwrap();

        let preview = clean_candidates(&candidates, true);
        assert_eq!(preview.selected, 1);
        assert_eq!(preview.bytes_selected, outer.bytes);

        let report = clean_candidates(&candidates, false);
        assert_eq!(report.selected, 1);
        assert_eq!(report.cleaned, 1);
        assert!(
            report.skipped_paths.is_empty(),
            "{:?}",
            report.skipped_paths
        );
        assert_eq!(report.cleaned_paths, vec![outer.path.clone()]);
        assert_eq!(report.apparent_bytes_removed, outer.bytes);
        assert!(!target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn removes_read_only_go_module_directories_after_owner_check() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().unwrap();
        let project = temp.path().join("demo");
        let module = project.join("target/example.org/module@v1.0.0");
        fs::create_dir_all(&module).unwrap();
        fs::write(project.join("Cargo.toml"), "[package]\nname='demo'\n").unwrap();
        fs::write(module.join("module.go"), "package module\n").unwrap();
        let mut permissions = fs::metadata(&module).unwrap().permissions();
        permissions.set_mode(0o555);
        fs::set_permissions(&module, permissions).unwrap();
        let mut scan = discover(&DiscoveryOptions {
            roots: vec![temp.path().to_path_buf()],
            scope: ScopeFilter::Project,
            kinds: Vec::new(),
            protect_days: 0,
        })
        .unwrap();
        let mut candidate = scan.candidates.remove(0);
        candidate.kind = "go-module-cache".to_owned();

        remove_cache_directory(&candidate).unwrap();

        assert!(!project.join("target").exists());
        assert!(project.join("Cargo.toml").exists());
    }

    #[test]
    fn reports_the_observed_free_space_delta_separately_from_size_estimates() {
        let temp = tempdir().unwrap();
        let project = temp.path().join("demo");
        fs::create_dir_all(project.join("target")).unwrap();
        fs::write(project.join("Cargo.toml"), "[package]\nname='demo'\n").unwrap();
        fs::write(project.join("target/object"), [0_u8; 64]).unwrap();
        let scan = discover(&DiscoveryOptions {
            roots: vec![temp.path().to_path_buf()],
            scope: ScopeFilter::Project,
            kinds: Vec::new(),
            protect_days: 0,
        })
        .unwrap();
        let mut samples = [1_000_u64, 5_096].into_iter();

        let report = clean_candidates_with_probe(&scan.candidates, false, |_| {
            Some(FilesystemSpace {
                id: 7,
                available_bytes: samples.next().unwrap(),
            })
        });

        assert_eq!(report.bytes_reclaimed_estimate, 64);
        assert!(report.allocated_bytes_removed_estimate >= 64);
        assert_eq!(report.observed_free_bytes_delta(), Some(4_096));
        assert_eq!(report.filesystem_deltas[0].free_bytes_before, 1_000);
        assert_eq!(report.filesystem_deltas[0].free_bytes_after, 5_096);
    }

    #[test]
    fn signed_delta_preserves_concurrent_disk_usage() {
        assert_eq!(signed_delta(800, 1_000), -200);
        assert_eq!(signed_delta(1_200, 1_000), 200);
    }

    #[test]
    fn refuses_when_ownership_marker_disappears() {
        let temp = tempdir().unwrap();
        let project = temp.path().join("demo");
        fs::create_dir_all(project.join("target")).unwrap();
        fs::write(project.join("Cargo.toml"), "[package]\nname='demo'\n").unwrap();
        let scan = discover(&DiscoveryOptions {
            roots: vec![temp.path().to_path_buf()],
            scope: ScopeFilter::Project,
            kinds: Vec::new(),
            protect_days: 0,
        })
        .unwrap();
        fs::remove_file(project.join("Cargo.toml")).unwrap();

        let report = clean_candidates(&scan.candidates, false);

        assert!(!report.changed);
        assert_eq!(report.skipped, 1);
        assert!(project.join("target").exists());
    }

    #[test]
    fn refuses_scan_only_candidates_even_through_library_api() {
        let temp = tempdir().unwrap();
        let project = temp.path().join("demo");
        fs::create_dir_all(project.join("target")).unwrap();
        fs::write(project.join("Cargo.toml"), "[package]\nname='demo'\n").unwrap();
        let mut scan = discover(&DiscoveryOptions {
            roots: vec![temp.path().to_path_buf()],
            scope: ScopeFilter::Project,
            kinds: Vec::new(),
            protect_days: 0,
        })
        .unwrap();
        scan.candidates[0].cleanable = false;

        let report = clean_candidates(&scan.candidates, false);

        assert!(!report.changed);
        assert_eq!(report.selected, 0);
        assert_eq!(report.policy_skipped, 1);
        assert!(project.join("target").exists());
    }
}
