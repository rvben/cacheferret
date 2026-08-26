use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::model::{FilesystemSpaceDelta, SkippedPath};
use crate::scanner::revalidate_for_delete;
use crate::{CacheCandidate, CleanReport};

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
    let candidates: Vec<&CacheCandidate> = candidates
        .iter()
        .filter(|candidate| candidate.cleanable)
        .collect();
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
            skipped_paths: Vec::new(),
        };
    }

    let mut cleaned_paths = Vec::new();
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
        match fs::remove_dir_all(&candidate.path) {
            Ok(()) => {
                reclaimed = reclaimed.saturating_add(candidate.bytes);
                allocated_removed = allocated_removed.saturating_add(candidate.allocated_bytes);
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
        skipped_paths,
    }
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
        available_bytes: u64::from(stats.f_bavail).saturating_mul(block_size),
    })
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
