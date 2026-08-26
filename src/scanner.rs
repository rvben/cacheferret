use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rayon::prelude::*;
use walkdir::WalkDir;

use crate::catalog::{global_paths, identify_project_cache};
use crate::model::{FileIdentity, TreeFingerprint};
use crate::{CacheCandidate, CacheScope, DiscoveryOptions, Error, ScanReport, catalog};

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
    let wanted: HashSet<&str> = options.kinds.iter().map(String::as_str).collect();
    let wants_kind = |kind: &str| wanted.is_empty() || wanted.contains(kind);

    let mut raw = Vec::new();
    let mut warnings = Vec::new();

    if options.scope.includes(CacheScope::Project) {
        for root in &options.roots {
            let canonical = root
                .canonicalize()
                .map_err(|_| Error::InvalidPath { path: root.clone() })?;
            if !canonical.is_dir() {
                return Err(Error::InvalidPath { path: root.clone() });
            }
            scan_project_root(&canonical, &wants_kind, &mut raw, &mut warnings);
        }
    }

    if options.scope.includes(CacheScope::Global) {
        for global in global_paths() {
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
