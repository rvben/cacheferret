use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, mpsc};
use std::time::{SystemTime, UNIX_EPOCH};

use rayon::prelude::*;
use walkdir::WalkDir;

use crate::catalog::{GlobalPath, global_paths, identify_project_cache};
use crate::model::{FileIdentity, TreeFingerprint};
use crate::{
    CacheCandidate, CacheScope, DiscoveredCache, DiscoveryEvent, DiscoveryOptions, Error,
    ScanReport, catalog,
};

#[derive(Debug, Clone)]
struct RawCandidate {
    kind: String,
    ecosystem: String,
    scope: CacheScope,
    path: PathBuf,
    anchor: PathBuf,
    network_restore: bool,
    cleanable: bool,
    minimum_bytes: u64,
}

/// Discover and size every matching cache.
pub fn discover(options: &DiscoveryOptions) -> Result<ScanReport, Error> {
    validate_kinds(&options.kinds)?;
    let roots = canonical_project_roots(options)?;
    let wanted: HashSet<&str> = options.kinds.iter().map(String::as_str).collect();
    let wants_kind = |kind: &str| wanted.is_empty() || wanted.contains(kind);
    let globals = if options.scope.includes(CacheScope::Global) {
        global_paths()
    } else {
        Vec::new()
    };
    let mut raw = Vec::new();
    let mut warnings = Vec::new();

    if options.scope.includes(CacheScope::Project) {
        for root in &roots {
            scan_project_root(root, &wants_kind, &mut raw, &mut warnings);
        }
    }

    if options.scope.includes(CacheScope::Global) {
        for global in globals {
            if !wants_kind(global.kind) {
                continue;
            }
            let Ok(metadata) = fs::symlink_metadata(&global.path) else {
                continue;
            };
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                continue;
            }
            let anchor = global
                .path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| global.path.clone());
            raw.push(RawCandidate {
                kind: global.kind.to_owned(),
                ecosystem: global.ecosystem.to_owned(),
                scope: CacheScope::Global,
                path: global.path,
                anchor,
                network_restore: global.network_restore,
                cleanable: global.cleanable,
                minimum_bytes: global.minimum_bytes,
            });
        }
    }

    deduplicate(&mut raw);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let mut candidates: Vec<CacheCandidate> = raw
        .into_par_iter()
        .filter_map(|raw| measure_candidate(raw, options.protect_days, now))
        .collect();
    candidates.sort_by(|left, right| {
        right
            .bytes
            .cmp(&left.bytes)
            .then_with(|| left.path.cmp(&right.path))
    });

    Ok(ScanReport {
        candidates,
        warnings,
    })
}

/// Discover caches while reporting recognized and fully measured entries.
///
/// Events may be emitted concurrently. A `Discovered` event always precedes
/// the corresponding `Measured` or `Skipped` event, and a successful return
/// means every emitted discovery has finished measurement.
pub fn discover_with_progress<F>(
    options: &DiscoveryOptions,
    progress: F,
) -> Result<ScanReport, Error>
where
    F: Fn(DiscoveryEvent) + Sync,
{
    discover_with_progress_prioritized(options, &[], progress)
}

/// Discover caches while measuring previously known paths first.
///
/// Priority paths are untrusted scheduling hints. Every path is recognized
/// against the current catalog, constrained to the requested roots and scope,
/// and measured normally before it appears in the returned report.
pub fn discover_with_progress_prioritized<F>(
    options: &DiscoveryOptions,
    priority_paths: &[PathBuf],
    progress: F,
) -> Result<ScanReport, Error>
where
    F: Fn(DiscoveryEvent) + Sync,
{
    discover_with_progress_with_limit(options, priority_paths, measurement_task_limit(), progress)
}

fn discover_with_progress_with_limit<F>(
    options: &DiscoveryOptions,
    priority_paths: &[PathBuf],
    task_limit: usize,
    progress: F,
) -> Result<ScanReport, Error>
where
    F: Fn(DiscoveryEvent) + Sync,
{
    validate_kinds(&options.kinds)?;
    let roots = canonical_project_roots(options)?;
    let wanted: HashSet<&str> = options.kinds.iter().map(String::as_str).collect();
    let wants_kind = |kind: &str| wanted.is_empty() || wanted.contains(kind);
    let globals = if options.scope.includes(CacheScope::Global) {
        global_paths()
    } else {
        Vec::new()
    };
    let mut warnings = Vec::new();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let candidates = Mutex::new(Vec::new());
    let mut seen = HashSet::new();
    let task_limit = task_limit.max(1);
    let (token_tx, token_rx) = mpsc::sync_channel(task_limit);
    let token_rx = Mutex::new(token_rx);
    for _ in 0..task_limit {
        token_tx.send(()).expect("token channel is open");
    }

    rayon::scope(|scope| {
        let mut dispatch = |raw: RawCandidate| {
            let key = raw.path.canonicalize().unwrap_or_else(|_| raw.path.clone());
            if !seen.insert(key) {
                return;
            }

            progress(DiscoveryEvent::Discovered(discovered_cache(&raw)));
            if token_rx
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .recv()
                .is_err()
            {
                return;
            }
            let candidates = &candidates;
            let progress = &progress;
            let token_tx = &token_tx;
            scope.spawn(move |_| {
                let path = raw.path.clone();
                if let Some(candidate) = measure_candidate(raw, options.protect_days, now) {
                    progress(DiscoveryEvent::Measured(candidate.clone()));
                    candidates
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .push(candidate);
                } else {
                    progress(DiscoveryEvent::Skipped { path });
                }
                let _ = token_tx.send(());
            });
        };
        let mut warn = |warning: String| {
            progress(DiscoveryEvent::Warning(warning.clone()));
            warnings.push(warning);
        };

        for path in priority_paths {
            if let Some(raw) =
                prioritized_candidate(path, &roots, &globals, options.scope, &wants_kind)
            {
                dispatch(raw);
            }
        }

        if options.scope.includes(CacheScope::Global) {
            for global in globals {
                if !wants_kind(global.kind) {
                    continue;
                }
                let Ok(metadata) = fs::symlink_metadata(&global.path) else {
                    continue;
                };
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    continue;
                }
                let anchor = global
                    .path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| global.path.clone());
                dispatch(RawCandidate {
                    kind: global.kind.to_owned(),
                    ecosystem: global.ecosystem.to_owned(),
                    scope: CacheScope::Global,
                    path: global.path,
                    anchor,
                    network_restore: global.network_restore,
                    cleanable: global.cleanable,
                    minimum_bytes: global.minimum_bytes,
                });
            }
        }

        for root in &roots {
            scan_project_root_progressive(root, &wants_kind, &mut dispatch, &mut warn);
        }
    });

    let mut candidates = candidates
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    sort_candidates(&mut candidates);

    Ok(ScanReport {
        candidates,
        warnings,
    })
}

fn canonical_project_roots(options: &DiscoveryOptions) -> Result<Vec<PathBuf>, Error> {
    if !options.scope.includes(CacheScope::Project) {
        return Ok(Vec::new());
    }
    let mut roots: Vec<PathBuf> = options
        .roots
        .iter()
        .map(|root| {
            let canonical = root
                .canonicalize()
                .map_err(|_| Error::InvalidPath { path: root.clone() })?;
            if !canonical.is_dir() {
                return Err(Error::InvalidPath { path: root.clone() });
            }
            Ok(canonical)
        })
        .collect::<Result<_, _>>()?;
    roots.sort();
    roots.dedup();
    let mut collapsed: Vec<PathBuf> = Vec::with_capacity(roots.len());
    for root in roots {
        if !collapsed.iter().any(|parent| root.starts_with(parent)) {
            collapsed.push(root);
        }
    }
    Ok(collapsed)
}

fn measurement_task_limit() -> usize {
    rayon::current_num_threads().saturating_mul(2).clamp(2, 32)
}

fn prioritized_candidate<F>(
    path: &Path,
    roots: &[PathBuf],
    globals: &[GlobalPath],
    scope: crate::ScopeFilter,
    wants_kind: &F,
) -> Option<RawCandidate>
where
    F: Fn(&str) -> bool,
{
    let metadata = fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return None;
    }

    if scope.includes(CacheScope::Global)
        && let Some(global) = globals.iter().find(|global| global.path == path)
        && wants_kind(global.kind)
    {
        let anchor = global
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| global.path.clone());
        return Some(RawCandidate {
            kind: global.kind.to_owned(),
            ecosystem: global.ecosystem.to_owned(),
            scope: CacheScope::Global,
            path: global.path.clone(),
            anchor,
            network_restore: global.network_restore,
            cleanable: global.cleanable,
            minimum_bytes: global.minimum_bytes,
        });
    }

    if !scope.includes(CacheScope::Project) {
        return None;
    }
    let canonical = path.canonicalize().ok()?;
    let anchor = roots
        .iter()
        .filter(|root| canonical != **root && canonical.starts_with(root))
        .max_by_key(|root| root.components().count())?;
    let found = identify_project_cache(&canonical)?;
    if !wants_kind(found.kind) {
        return None;
    }
    Some(RawCandidate {
        kind: found.kind.to_owned(),
        ecosystem: found.ecosystem.to_owned(),
        scope: CacheScope::Project,
        path: canonical,
        anchor: anchor.clone(),
        network_restore: found.network_restore,
        cleanable: true,
        minimum_bytes: 0,
    })
}

fn discovered_cache(raw: &RawCandidate) -> DiscoveredCache {
    DiscoveredCache {
        kind: raw.kind.clone(),
        ecosystem: raw.ecosystem.clone(),
        scope: raw.scope,
        path: raw.path.clone(),
        cleanable: raw.cleanable,
    }
}

fn validate_kinds(kinds: &[String]) -> Result<(), Error> {
    if kinds.is_empty() {
        return Ok(());
    }
    let valid: HashSet<&str> = catalog().into_iter().map(|entry| entry.kind).collect();
    if let Some(kind) = kinds.iter().find(|kind| !valid.contains(kind.as_str())) {
        return Err(Error::InvalidInput {
            message: format!("unknown cache kind {kind:?}; run `cacheferret catalog`"),
        });
    }
    Ok(())
}

fn scan_project_root<F>(
    root: &Path,
    wants_kind: &F,
    raw: &mut Vec<RawCandidate>,
    warnings: &mut Vec<String>,
) where
    F: Fn(&str) -> bool,
{
    let mut walker = WalkDir::new(root).follow_links(false).into_iter();
    while let Some(item) = walker.next() {
        let entry = match item {
            Ok(entry) => entry,
            Err(error) => {
                warnings.push(error.to_string());
                continue;
            }
        };
        if !entry.file_type().is_dir() {
            continue;
        }
        let path = entry.path();
        if path != root && is_control_directory(path) {
            walker.skip_current_dir();
            continue;
        }
        if entry.file_type().is_symlink() {
            walker.skip_current_dir();
            continue;
        }
        if let Some(found) = identify_project_cache(path) {
            if wants_kind(found.kind) {
                raw.push(RawCandidate {
                    kind: found.kind.to_owned(),
                    ecosystem: found.ecosystem.to_owned(),
                    scope: CacheScope::Project,
                    path: path.to_path_buf(),
                    anchor: root.to_path_buf(),
                    network_restore: found.network_restore,
                    cleanable: true,
                    minimum_bytes: 0,
                });
            }
            walker.skip_current_dir();
        }
    }
}

fn scan_project_root_progressive<F, W>(
    root: &Path,
    wants_kind: &F,
    on_candidate: &mut impl FnMut(RawCandidate),
    on_warning: &mut W,
) where
    F: Fn(&str) -> bool,
    W: FnMut(String),
{
    let mut walker = WalkDir::new(root).follow_links(false).into_iter();
    while let Some(item) = walker.next() {
        let entry = match item {
            Ok(entry) => entry,
            Err(error) => {
                on_warning(error.to_string());
                continue;
            }
        };
        if !entry.file_type().is_dir() {
            continue;
        }
        let path = entry.path();
        if path != root && is_control_directory(path) {
            walker.skip_current_dir();
            continue;
        }
        if entry.file_type().is_symlink() {
            walker.skip_current_dir();
            continue;
        }
        if let Some(found) = identify_project_cache(path) {
            if wants_kind(found.kind) {
                on_candidate(RawCandidate {
                    kind: found.kind.to_owned(),
                    ecosystem: found.ecosystem.to_owned(),
                    scope: CacheScope::Project,
                    path: path.to_path_buf(),
                    anchor: root.to_path_buf(),
                    network_restore: found.network_restore,
                    cleanable: true,
                    minimum_bytes: 0,
                });
            }
            walker.skip_current_dir();
        }
    }
}

fn is_control_directory(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(".git" | ".hg" | ".svn")
    )
}

fn deduplicate(raw: &mut Vec<RawCandidate>) {
    let mut seen = HashSet::new();
    raw.retain(|candidate| {
        let key = candidate
            .path
            .canonicalize()
            .unwrap_or_else(|_| candidate.path.clone());
        seen.insert(key)
    });
}

fn sort_candidates(candidates: &mut [CacheCandidate]) {
    candidates.sort_by(|left, right| {
        right
            .bytes
            .cmp(&left.bytes)
            .then_with(|| left.path.cmp(&right.path))
    });
}

fn measure_candidate(raw: RawCandidate, protect_days: u64, now: u64) -> Option<CacheCandidate> {
    let metadata = fs::symlink_metadata(&raw.path).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return None;
    }
    let identity = identity(&metadata);
    let measurement = measure_tree(&raw.path);
    if measurement.bytes < raw.minimum_bytes {
        return None;
    }
    let modified_unix = measurement.modified_unix;
    let age_days = modified_unix.map(|modified| now.saturating_sub(modified) / 86_400);
    let protected = age_days.is_none_or(|days| days < protect_days);

    Some(CacheCandidate {
        kind: raw.kind,
        ecosystem: raw.ecosystem,
        scope: raw.scope,
        path: raw.path,
        bytes: measurement.bytes,
        allocated_bytes: measurement.allocated_bytes,
        modified_unix,
        age_days,
        protected,
        network_restore: raw.network_restore,
        cleanable: raw.cleanable,
        anchor: raw.anchor,
        identity,
        tree_fingerprint: measurement.fingerprint,
    })
}

struct TreeMeasurement {
    bytes: u64,
    allocated_bytes: u64,
    modified_unix: Option<u64>,
    fingerprint: TreeFingerprint,
}

fn measure_tree(root: &Path) -> TreeMeasurement {
    let mut bytes = 0_u64;
    let mut allocated_bytes = 0_u64;
    let mut entries = 0_u64;
    let mut latest_modified_nanos = None;
    #[cfg(unix)]
    let mut seen_files = HashSet::new();
    for entry in WalkDir::new(root).follow_links(false).into_iter().flatten() {
        let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        entries = entries.saturating_add(1);
        if metadata.is_file() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if seen_files.insert((metadata.dev(), metadata.ino())) {
                    bytes = bytes.saturating_add(metadata.len());
                    allocated_bytes =
                        allocated_bytes.saturating_add(metadata.blocks().saturating_mul(512));
                }
            }
            #[cfg(not(unix))]
            {
                bytes = bytes.saturating_add(metadata.len());
                allocated_bytes = allocated_bytes.saturating_add(metadata.len());
            }
        } else if metadata.is_dir() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                allocated_bytes =
                    allocated_bytes.saturating_add(metadata.blocks().saturating_mul(512));
            }
        }
        if let Ok(modified) = metadata.modified()
            && let Ok(since_epoch) = modified.duration_since(UNIX_EPOCH)
        {
            latest_modified_nanos = Some(
                latest_modified_nanos.map_or(since_epoch.as_nanos(), |old: u128| {
                    old.max(since_epoch.as_nanos())
                }),
            );
        }
    }
    TreeMeasurement {
        bytes,
        allocated_bytes,
        modified_unix: latest_modified_nanos.map(|nanos| (nanos / 1_000_000_000) as u64),
        fingerprint: TreeFingerprint {
            bytes,
            allocated_bytes,
            entries,
            latest_modified_nanos,
        },
    }
}

/// Revalidate and remeasure a candidate against the current filesystem state.
///
/// Interactive callers use this immediately before deciding whether a delete
/// needs confirmation. The candidate keeps its original identity and safety
/// anchor so the final cleanup revalidation still detects replacement or
/// containment changes.
pub fn refresh_candidate(
    candidate: &CacheCandidate,
    protect_days: u64,
) -> Result<CacheCandidate, Error> {
    revalidate(candidate).map_err(|message| Error::Conflict { message })?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let measurement = measure_tree(&candidate.path);
    let bytes = measurement.bytes;
    let modified_unix = measurement.modified_unix;
    let age_days = modified_unix.map(|modified| now.saturating_sub(modified) / 86_400);

    let mut refreshed = candidate.clone();
    refreshed.bytes = bytes;
    refreshed.allocated_bytes = measurement.allocated_bytes;
    refreshed.modified_unix = modified_unix;
    refreshed.age_days = age_days;
    refreshed.protected = age_days.is_none_or(|days| days < protect_days);
    refreshed.tree_fingerprint = measurement.fingerprint;
    Ok(refreshed)
}

#[cfg(unix)]
fn identity(metadata: &fs::Metadata) -> FileIdentity {
    use std::os::unix::fs::MetadataExt;
    FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

#[cfg(not(unix))]
fn identity(metadata: &fs::Metadata) -> FileIdentity {
    FileIdentity {
        modified_unix: metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs()),
    }
}

pub(crate) fn revalidate(candidate: &CacheCandidate) -> Result<(), String> {
    let metadata = fs::symlink_metadata(&candidate.path)
        .map_err(|error| format!("cannot stat path: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("path is no longer a real directory".to_owned());
    }
    if identity(&metadata) != candidate.identity {
        return Err("filesystem identity changed after scanning".to_owned());
    }

    let canonical_path = candidate
        .path
        .canonicalize()
        .map_err(|error| format!("cannot resolve path: {error}"))?;
    let canonical_anchor = candidate
        .anchor
        .canonicalize()
        .map_err(|error| format!("cannot resolve safety anchor: {error}"))?;
    if canonical_path == canonical_anchor || !canonical_path.starts_with(&canonical_anchor) {
        return Err("path escaped its scan root".to_owned());
    }

    match candidate.scope {
        CacheScope::Project => {
            let current = identify_project_cache(&candidate.path)
                .ok_or_else(|| "cache ownership markers disappeared".to_owned())?;
            if current.kind != candidate.kind {
                return Err("cache kind changed after scanning".to_owned());
            }
        }
        CacheScope::Global => {
            let still_known = global_paths()
                .into_iter()
                .any(|known| known.kind == candidate.kind && known.path == candidate.path);
            if !still_known {
                return Err("path is no longer a configured global cache".to_owned());
            }
        }
    }
    Ok(())
}

pub(crate) fn revalidate_for_delete(candidate: &CacheCandidate) -> Result<(), String> {
    revalidate(candidate)?;
    if requires_quiescent_tree(&candidate.kind) && temporary_tree_changed(candidate) {
        return Err("temporary storage changed after it was measured; scan again".to_owned());
    }
    Ok(())
}

fn requires_quiescent_tree(kind: &str) -> bool {
    matches!(
        kind,
        "macos-chrome-signing-clones" | "macos-temporary-build-cache" | "macos-temporary-workspace"
    )
}

fn temporary_tree_changed(candidate: &CacheCandidate) -> bool {
    measure_tree(&candidate.path).fingerprint != candidate.tree_fingerprint
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use crate::ScopeFilter;

    use super::*;

    #[test]
    fn discovers_cargo_target_and_skips_git_metadata() {
        let temp = tempdir().unwrap();
        let project = temp.path().join("demo");
        fs::create_dir_all(project.join("target/debug")).unwrap();
        fs::write(project.join("Cargo.toml"), "[package]\nname='demo'\n").unwrap();
        fs::write(project.join("target/debug/app"), [0_u8; 32]).unwrap();
        fs::create_dir_all(project.join(".git/target")).unwrap();

        let report = discover(&DiscoveryOptions {
            roots: vec![temp.path().to_path_buf()],
            scope: ScopeFilter::Project,
            kinds: Vec::new(),
            protect_days: 7,
        })
        .unwrap();

        assert_eq!(report.candidates.len(), 1);
        assert_eq!(report.candidates[0].kind, "cargo-target");
        assert_eq!(report.candidates[0].bytes, 32);
        assert!(report.candidates[0].allocated_bytes >= 32);
    }

    #[test]
    fn progressive_discovery_reports_recognition_before_measurement() {
        let temp = tempdir().unwrap();
        let project = temp.path().join("demo");
        let target = project.join("target");
        fs::create_dir_all(&target).unwrap();
        fs::write(project.join("Cargo.toml"), "[package]\nname='demo'\n").unwrap();
        fs::write(target.join("artifact"), [0_u8; 32]).unwrap();
        let events = Mutex::new(Vec::new());

        let report = discover_with_progress(
            &DiscoveryOptions {
                roots: vec![temp.path().to_path_buf()],
                scope: ScopeFilter::Project,
                kinds: Vec::new(),
                protect_days: 7,
            },
            |event| {
                events.lock().unwrap().push(match event {
                    DiscoveryEvent::Discovered(cache) => {
                        format!("discovered:{}", cache.path.display())
                    }
                    DiscoveryEvent::Measured(cache) => {
                        format!("measured:{}", cache.path.display())
                    }
                    DiscoveryEvent::Skipped { path } => format!("skipped:{}", path.display()),
                    DiscoveryEvent::Warning(warning) => format!("warning:{warning}"),
                });
            },
        )
        .unwrap();

        let events = events.into_inner().unwrap();
        let canonical_target = temp.path().canonicalize().unwrap().join("demo/target");
        assert_eq!(report.candidates.len(), 1);
        assert_eq!(
            events,
            vec![
                format!("discovered:{}", canonical_target.display()),
                format!("measured:{}", canonical_target.display()),
            ]
        );
    }

    #[test]
    fn progressive_discovery_deduplicates_overlapping_roots_before_emitting() {
        let temp = tempdir().unwrap();
        let project = temp.path().join("demo");
        fs::create_dir_all(project.join("target")).unwrap();
        fs::write(project.join("Cargo.toml"), "[package]\nname='demo'\n").unwrap();
        let discoveries = Mutex::new(0_usize);

        let options = DiscoveryOptions {
            roots: vec![temp.path().to_path_buf(), project],
            scope: ScopeFilter::Project,
            kinds: Vec::new(),
            protect_days: 7,
        };
        assert_eq!(canonical_project_roots(&options).unwrap().len(), 1);

        let report = discover_with_progress(&options, |event| {
            if matches!(event, DiscoveryEvent::Discovered(_)) {
                *discoveries.lock().unwrap() += 1;
            }
        })
        .unwrap();

        assert_eq!(report.candidates.len(), 1);
        assert_eq!(discoveries.into_inner().unwrap(), 1);
    }

    #[test]
    fn atomic_discovery_also_collapses_overlapping_roots() {
        let temp = tempdir().unwrap();
        let project = temp.path().join("demo");
        fs::create_dir_all(project.join("target")).unwrap();
        fs::write(project.join("Cargo.toml"), "[package]\nname='demo'\n").unwrap();

        let report = discover(&DiscoveryOptions {
            roots: vec![temp.path().to_path_buf(), project],
            scope: ScopeFilter::Project,
            kinds: Vec::new(),
            protect_days: 7,
        })
        .unwrap();

        assert_eq!(report.candidates.len(), 1);
    }

    #[test]
    fn remembered_paths_are_measured_before_the_project_crawl() {
        let temp = tempdir().unwrap();
        let first_project = temp.path().join("alpha");
        let first_target = first_project.join("target");
        let priority_project = temp.path().join("zulu");
        let priority_target = priority_project.join("target");
        for (project, target) in [
            (&first_project, &first_target),
            (&priority_project, &priority_target),
        ] {
            fs::create_dir_all(target).unwrap();
            fs::write(project.join("Cargo.toml"), "[package]\nname='demo'\n").unwrap();
            fs::write(target.join("artifact"), [0_u8; 32]).unwrap();
        }
        let discoveries = Mutex::new(Vec::new());

        let report = discover_with_progress_prioritized(
            &DiscoveryOptions {
                roots: vec![temp.path().to_path_buf()],
                scope: ScopeFilter::Project,
                kinds: Vec::new(),
                protect_days: 7,
            },
            &[priority_target.clone(), PathBuf::from("/outside/untrusted")],
            |event| {
                if let DiscoveryEvent::Discovered(cache) = event {
                    discoveries.lock().unwrap().push(cache.path);
                }
            },
        )
        .unwrap();

        assert_eq!(report.candidates.len(), 2);
        assert_eq!(
            discoveries.lock().unwrap().first(),
            Some(&priority_target.canonicalize().unwrap())
        );
    }

    #[test]
    fn progressive_measurement_pool_is_adaptive_and_bounded() {
        assert!((2..=32).contains(&measurement_task_limit()));
    }

    #[test]
    fn progressive_pipeline_completes_more_candidates_than_its_task_window() {
        let temp = tempdir().unwrap();
        let candidate_count = rayon::current_num_threads().max(1) * 3;
        for index in 0..candidate_count {
            let project = temp.path().join(format!("project-{index}"));
            fs::create_dir_all(project.join("target")).unwrap();
            fs::write(project.join("Cargo.toml"), "[package]\nname='demo'\n").unwrap();
        }
        let measured = Mutex::new(0_usize);

        let report = discover_with_progress(
            &DiscoveryOptions {
                roots: vec![temp.path().to_path_buf()],
                scope: ScopeFilter::Project,
                kinds: Vec::new(),
                protect_days: 7,
            },
            |event| {
                if matches!(event, DiscoveryEvent::Measured(_)) {
                    *measured.lock().unwrap() += 1;
                }
            },
        )
        .unwrap();

        assert_eq!(report.candidates.len(), candidate_count);
        assert_eq!(measured.into_inner().unwrap(), candidate_count);
    }

    #[cfg(unix)]
    #[test]
    fn hard_links_are_counted_once_in_tree_sizes() {
        let temp = tempdir().unwrap();
        let project = temp.path().join("demo");
        fs::create_dir_all(project.join("target")).unwrap();
        fs::write(project.join("Cargo.toml"), "[package]\nname='demo'\n").unwrap();
        fs::write(project.join("target/object"), [0_u8; 128]).unwrap();
        fs::hard_link(
            project.join("target/object"),
            project.join("target/object-link"),
        )
        .unwrap();

        let report = discover(&DiscoveryOptions {
            roots: vec![temp.path().to_path_buf()],
            scope: ScopeFilter::Project,
            kinds: Vec::new(),
            protect_days: 0,
        })
        .unwrap();

        assert_eq!(report.candidates[0].bytes, 128);
    }

    #[test]
    fn refuses_unknown_kind() {
        let error = discover(&DiscoveryOptions {
            roots: Vec::new(),
            scope: ScopeFilter::Global,
            kinds: vec!["imaginary".to_owned()],
            protect_days: 7,
        })
        .unwrap_err();
        assert_eq!(error.kind(), "invalid_input");
    }

    #[test]
    fn refresh_candidate_observes_new_files_and_recent_activity() {
        let temp = tempdir().unwrap();
        let project = temp.path().join("demo");
        let target = project.join("target");
        fs::create_dir_all(&target).unwrap();
        fs::write(project.join("Cargo.toml"), "[package]\nname='demo'\n").unwrap();

        let mut report = discover(&DiscoveryOptions {
            roots: vec![temp.path().to_path_buf()],
            scope: ScopeFilter::Project,
            kinds: Vec::new(),
            protect_days: 0,
        })
        .unwrap();
        report.candidates[0].age_days = Some(30);
        report.candidates[0].bytes = 0;
        fs::write(target.join("new-object"), [7_u8; 128]).unwrap();

        let refreshed = refresh_candidate(&report.candidates[0], 7).unwrap();

        assert_eq!(refreshed.bytes, 128);
        assert_eq!(refreshed.age_days, Some(0));
        assert!(refreshed.protected);
    }

    #[test]
    fn recognizes_major_ecosystem_project_caches() {
        let temp = tempdir().unwrap();
        let fixtures: &[(&str, &str, &[&str])] = &[
            ("node", "package.json", &["node_modules"]),
            ("gradle", "build.gradle.kts", &["build", ".gradle"]),
            ("maven", "pom.xml", &["target"]),
            ("dotnet", "demo.csproj", &["bin", "obj"]),
            ("php", "composer.json", &["vendor"]),
            ("swift", "Package.swift", &[".build"]),
            ("zig", "build.zig", &[".zig-cache"]),
            ("dart", "pubspec.yaml", &[".dart_tool", "build"]),
            ("elixir", "mix.exs", &["_build", "deps"]),
            ("stack", "stack.yaml", &[".stack-work"]),
            ("terraform", "main.tf", &[".terraform"]),
        ];
        for (project_name, marker, caches) in fixtures {
            let project = temp.path().join(project_name);
            fs::create_dir_all(&project).unwrap();
            fs::write(project.join(marker), "fixture").unwrap();
            for cache in *caches {
                fs::create_dir_all(project.join(cache)).unwrap();
            }
        }

        let python = temp.path().join("python");
        fs::create_dir_all(python.join(".venv")).unwrap();
        fs::write(python.join(".venv/pyvenv.cfg"), "home = /python").unwrap();
        for cache in ["__pycache__", ".pytest_cache", ".mypy_cache", ".ruff_cache"] {
            fs::create_dir_all(python.join(cache)).unwrap();
        }

        let ruby = temp.path().join("ruby");
        fs::create_dir_all(ruby.join("vendor/bundle")).unwrap();
        fs::write(ruby.join("Gemfile"), "source 'https://example.invalid'").unwrap();

        let cmake = temp.path().join("cmake");
        fs::create_dir_all(cmake.join("build")).unwrap();
        fs::write(cmake.join("build/CMakeCache.txt"), "fixture").unwrap();

        let cabal = temp.path().join("cabal");
        fs::create_dir_all(cabal.join("dist-newstyle")).unwrap();
        fs::write(cabal.join("demo.cabal"), "name: demo").unwrap();

        let renv = temp.path().join("r");
        fs::create_dir_all(renv.join("renv/library")).unwrap();
        fs::write(renv.join("renv.lock"), "{}").unwrap();

        let tagged = temp.path().join("tagged-cache");
        fs::create_dir_all(&tagged).unwrap();
        fs::write(
            tagged.join("CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55\n",
        )
        .unwrap();

        let report = discover(&DiscoveryOptions {
            roots: vec![temp.path().to_path_buf()],
            scope: ScopeFilter::Project,
            kinds: Vec::new(),
            protect_days: 7,
        })
        .unwrap();
        let kinds: HashSet<&str> = report
            .candidates
            .iter()
            .map(|candidate| candidate.kind.as_str())
            .collect();

        for expected in [
            "node-modules",
            "gradle-build",
            "gradle-project-cache",
            "maven-target",
            "dotnet-bin",
            "dotnet-obj",
            "composer-vendor",
            "swift-build",
            "zig-project-cache",
            "dart-tool",
            "dart-build",
            "mix-build",
            "mix-deps",
            "stack-work",
            "terraform-modules",
            "python-venv",
            "python-bytecode",
            "pytest-cache",
            "mypy-cache",
            "ruff-cache",
            "bundler-vendor",
            "cmake-build",
            "cabal-dist",
            "renv-library",
            "cachedir-tag",
        ] {
            assert!(kinds.contains(expected), "missing {expected}: {kinds:?}");
        }
    }

    #[test]
    fn minimum_size_hides_small_diagnostic_locations() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        fs::write(workspace.join("artifact"), [0_u8; 32]).unwrap();

        let raw = RawCandidate {
            kind: "macos-temporary-workspace".to_owned(),
            ecosystem: "macos".to_owned(),
            scope: CacheScope::Global,
            path: workspace,
            anchor: temp.path().to_path_buf(),
            network_restore: false,
            cleanable: false,
            minimum_bytes: 64,
        };

        assert!(measure_candidate(raw, 7, 0).is_none());
    }

    #[test]
    fn temporary_candidates_must_stay_unchanged_until_deletion() {
        assert!(requires_quiescent_tree("macos-temporary-workspace"));

        let temp = tempdir().unwrap();
        let target = temp.path().join("cache");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("artifact"), [0_u8; 32]).unwrap();
        let metadata = fs::symlink_metadata(&target).unwrap();
        let measurement = measure_tree(&target);
        let candidate = CacheCandidate {
            kind: "macos-temporary-build-cache".to_owned(),
            ecosystem: "macos".to_owned(),
            scope: CacheScope::Global,
            path: target.clone(),
            bytes: measurement.bytes,
            allocated_bytes: measurement.allocated_bytes,
            modified_unix: measurement.modified_unix,
            age_days: Some(0),
            protected: true,
            network_restore: false,
            cleanable: true,
            anchor: temp.path().to_path_buf(),
            identity: identity(&metadata),
            tree_fingerprint: measurement.fingerprint,
        };

        assert!(!temporary_tree_changed(&candidate));
        fs::write(target.join("new-artifact"), [0_u8; 64]).unwrap();
        assert!(temporary_tree_changed(&candidate));
    }
}
