use std::fs;

use crate::model::SkippedPath;
use crate::scanner::revalidate;
use crate::{CacheCandidate, CleanReport};

/// Revalidate and remove a batch of candidates.
///
/// Every candidate is checked immediately before deletion. A refused item does
/// not abort the rest of the batch and is reported with its reason.
pub fn clean_candidates(candidates: &[CacheCandidate], dry_run: bool) -> CleanReport {
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
            bytes_reclaimed_estimate: 0,
            selected_targets,
            cleaned_paths: Vec::new(),
            skipped_paths: Vec::new(),
        };
    }

    let mut cleaned_paths = Vec::new();
    let mut skipped_paths = Vec::new();
    let mut reclaimed = 0_u64;

    for candidate in &candidates {
        if let Err(reason) = revalidate(candidate) {
            skipped_paths.push(SkippedPath {
                path: candidate.path.clone(),
                reason,
            });
            continue;
        }
        match fs::remove_dir_all(&candidate.path) {
            Ok(()) => {
                reclaimed = reclaimed.saturating_add(candidate.bytes);
                cleaned_paths.push(candidate.path.clone());
            }
            Err(error) => skipped_paths.push(SkippedPath {
                path: candidate.path.clone(),
                reason: error.to_string(),
            }),
        }
    }

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
        bytes_reclaimed_estimate: reclaimed,
        selected_targets,
        cleaned_paths,
        skipped_paths,
    }
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
