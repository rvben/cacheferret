use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rayon::prelude::*;
use walkdir::WalkDir;

use crate::catalog::{global_paths, identify_project_cache};
use crate::model::FileIdentity;
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
    let (bytes, modified_unix) = measure_tree(&raw.path);
    let age_days = modified_unix.map(|modified| now.saturating_sub(modified) / 86_400);
    let protected = age_days.is_none_or(|days| days < protect_days);

    Some(CacheCandidate {
        kind: raw.kind,
        ecosystem: raw.ecosystem,
        scope: raw.scope,
        path: raw.path,
        bytes,
        modified_unix,
        age_days,
        protected,
        network_restore: raw.network_restore,
        cleanable: raw.cleanable,
        anchor: raw.anchor,
        identity,
    })
}

fn measure_tree(root: &Path) -> (u64, Option<u64>) {
    let mut bytes = 0_u64;
    let mut latest = None;
    for entry in WalkDir::new(root).follow_links(false).into_iter().flatten() {
        let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_file() {
            bytes = bytes.saturating_add(metadata.len());
        }
        if let Ok(modified) = metadata.modified()
            && let Ok(since_epoch) = modified.duration_since(UNIX_EPOCH)
        {
            latest = Some(latest.map_or(since_epoch.as_secs(), |old: u64| {
                old.max(since_epoch.as_secs())
            }));
        }
    }
    (bytes, latest)
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
}
