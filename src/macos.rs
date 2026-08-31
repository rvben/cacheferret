use std::fs;
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
use std::env;
use walkdir::WalkDir;

use crate::catalog::{GlobalPath, valid_cachedir_tag};

const LARGE_TEMP_WORKSPACE_BYTES: u64 = 100 * 1024 * 1024;
const TEMP_WORKSPACE_CACHE_DEPTH: usize = 4;

#[cfg(target_os = "macos")]
pub(crate) fn temporary_paths(home: &Path) -> Vec<GlobalPath> {
    let Some(user_temp_container) = env::temp_dir()
        .canonicalize()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
    else {
        return Vec::new();
    };
    temporary_paths_at(Path::new("/private/tmp"), &user_temp_container, home)
}

fn temporary_paths_at(
    private_tmp: &Path,
    user_temp_container: &Path,
    owner_hint: &Path,
) -> Vec<GlobalPath> {
    let Some(owner) = owner_id(owner_hint) else {
        return Vec::new();
    };
    let mut paths = Vec::new();

    let chrome_clones = user_temp_container
        .join("X")
        .join("com.google.Chrome.code_sign_clone");
    if valid_chrome_signing_clone_root(&chrome_clones, owner) {
        paths.push(candidate(
            chrome_clones,
            "macos-chrome-signing-clones",
            true,
            0,
        ));
    }

    let private_tmp = private_tmp
        .canonicalize()
        .unwrap_or_else(|_| private_tmp.to_path_buf());
    let Ok(entries) = fs::read_dir(&private_tmp) else {
        return paths;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !owned_real_directory(&path, owner) {
            continue;
        }
        if recognized_temporary_build_cache(&path) {
            paths.push(candidate(path, "macos-temporary-build-cache", true, 0));
        } else {
            let nested_caches = tagged_caches_in_temporary_tree(&path, owner);
            if !nested_caches.is_empty() {
                paths.extend(
                    nested_caches
                        .into_iter()
                        .map(|cache| candidate(cache, "macos-temporary-build-cache", true, 0)),
                );
            } else if looks_like_temporary_workspace(&path) {
                paths.push(candidate(
                    path,
                    "macos-temporary-workspace",
                    false,
                    LARGE_TEMP_WORKSPACE_BYTES,
                ));
            }
        }
    }
    paths
}

fn tagged_caches_in_temporary_tree(root: &Path, owner: u64) -> Vec<PathBuf> {
    let mut caches = Vec::new();
    let mut walker = WalkDir::new(root)
        .min_depth(1)
        .max_depth(TEMP_WORKSPACE_CACHE_DEPTH)
        .follow_links(false)
        .into_iter();

    while let Some(item) = walker.next() {
        let Ok(entry) = item else {
            continue;
        };
        if !entry.file_type().is_dir() {
            continue;
        }
        let path = entry.path();
        if entry.file_type().is_symlink() || is_control_directory(path) {
            walker.skip_current_dir();
            continue;
        }
        if !owned_real_directory(path, owner) {
            walker.skip_current_dir();
            continue;
        }
        if valid_cachedir_tag(path) {
            caches.push(path.to_path_buf());
            walker.skip_current_dir();
        }
    }

    caches
}

fn is_control_directory(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(".git" | ".hg" | ".svn")
    )
}

fn candidate(path: PathBuf, kind: &'static str, cleanable: bool, minimum_bytes: u64) -> GlobalPath {
    GlobalPath {
        path,
        kind,
        ecosystem: "macos",
        network_restore: false,
        cleanable,
        minimum_bytes,
    }
}

fn valid_chrome_signing_clone_root(path: &Path, owner: u64) -> bool {
    if !owned_real_directory(path, owner) {
        return false;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return false;
    };
    let mut found = false;
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        found = true;
        let child = entry.path();
        let name_matches = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with("code_sign_clone."));
        if !name_matches || !owned_real_directory(&child, owner) {
            return false;
        }
    }
    found
}

fn recognized_temporary_build_cache(path: &Path) -> bool {
    if valid_cachedir_tag(path) || is_go_build_cache(path) || is_xcode_derived_data(path) {
        return true;
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    [
        "build-cache",
        "go-build-cache",
        "go-cache",
        "gocache",
        "gomodcache",
        "go-mod-cache",
        "mod-cache",
        "test-cache",
        "uv-cache",
    ]
    .iter()
    .any(|token| name.contains(token))
        || ["-target", "-venv", "-go-build", "-go-mod", "-gomod"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

fn is_go_build_cache(path: &Path) -> bool {
    const README_FIRST_LINE: &str =
        "This directory holds cached build artifacts from the Go build system.";
    let valid_readme = fs::read_to_string(path.join("README"))
        .is_ok_and(|contents| contents.lines().next() == Some(README_FIRST_LINE));
    valid_readme
        && fs::read_dir(path).is_ok_and(|entries| {
            entries.flatten().any(|entry| {
                entry.file_type().is_ok_and(|file_type| file_type.is_dir())
                    && entry.file_name().to_str().is_some_and(is_hex_bucket)
            })
        })
}

fn is_hex_bucket(name: &str) -> bool {
    name.len() == 2 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_xcode_derived_data(path: &Path) -> bool {
    path.join("Build/Intermediates.noindex").is_dir()
        && path.join("Logs").is_dir()
        && (path.join("Build/Products").is_dir() || path.join("ModuleCache.noindex").is_dir())
}

fn looks_like_temporary_workspace(path: &Path) -> bool {
    if has_project_marker(path) {
        return true;
    }
    fs::read_dir(path).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            let child = entry.path();
            entry
                .file_type()
                .is_ok_and(|file_type| file_type.is_dir() && !file_type.is_symlink())
                && has_project_marker(&child)
        })
    })
}

fn has_project_marker(path: &Path) -> bool {
    [
        ".git",
        "Cargo.toml",
        "go.mod",
        "package.json",
        "pyproject.toml",
        "Package.swift",
        "build.gradle",
        "build.gradle.kts",
        "pom.xml",
    ]
    .iter()
    .any(|marker| path.join(marker).exists())
}

#[cfg(unix)]
fn owner_id(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    fs::symlink_metadata(path)
        .ok()
        .map(|metadata| metadata.uid() as u64)
}

#[cfg(not(unix))]
fn owner_id(_path: &Path) -> Option<u64> {
    Some(0)
}

#[cfg(unix)]
fn owned_real_directory(path: &Path, owner: u64) -> bool {
    use std::os::unix::fs::MetadataExt;
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.is_dir() && !metadata.file_type().is_symlink() && metadata.uid() as u64 == owner
    })
}

#[cfg(not(unix))]
fn owned_real_directory(path: &Path, _owner: u64) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn temporary_storage_is_conservatively_classified() {
        let fixture = tempdir().unwrap();
        let private_tmp = fixture.path().join("private-tmp");
        let user_temp = fixture.path().join("user-temp");
        fs::create_dir_all(&private_tmp).unwrap();

        let chrome = user_temp
            .join("X")
            .join("com.google.Chrome.code_sign_clone");
        fs::create_dir_all(chrome.join("code_sign_clone.abc123/Google Chrome.app.bundle")).unwrap();

        let build_cache = private_tmp.join("demo-go-build-cache");
        fs::create_dir(&build_cache).unwrap();
        let workspace = private_tmp.join("review-worktree");
        fs::create_dir_all(workspace.join("repo")).unwrap();
        fs::write(workspace.join("repo/Cargo.toml"), "[package]").unwrap();
        let unrelated = private_tmp.join("personal-notes");
        fs::create_dir(&unrelated).unwrap();

        let paths = temporary_paths_at(&private_tmp, &user_temp, fixture.path());
        let build_cache = build_cache.canonicalize().unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let unrelated = unrelated.canonicalize().unwrap();
        let chrome_candidate = paths
            .iter()
            .find(|path| path.kind == "macos-chrome-signing-clones")
            .unwrap();
        assert_eq!(chrome_candidate.path, chrome);
        assert!(chrome_candidate.cleanable);

        let cache_candidate = paths.iter().find(|path| path.path == build_cache).unwrap();
        assert_eq!(cache_candidate.kind, "macos-temporary-build-cache");
        assert!(cache_candidate.cleanable);

        let workspace_candidate = paths.iter().find(|path| path.path == workspace).unwrap();
        assert_eq!(workspace_candidate.kind, "macos-temporary-workspace");
        assert!(!workspace_candidate.cleanable);
        assert_eq!(
            workspace_candidate.minimum_bytes,
            LARGE_TEMP_WORKSPACE_BYTES
        );
        assert!(paths.iter().all(|path| path.path != unrelated));
    }

    #[test]
    fn tagged_cache_inside_temporary_workspace_is_independently_cleanable() {
        let fixture = tempdir().unwrap();
        let private_tmp = fixture.path().join("private-tmp");
        let user_temp = fixture.path().join("user-temp");
        let workspace = private_tmp.join("release-workspace");
        let target = workspace.join("repo/target");
        fs::create_dir_all(target.join("debug/deps")).unwrap();
        fs::write(workspace.join("repo/Cargo.toml"), "[package]").unwrap();
        fs::write(
            target.join("CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55\n",
        )
        .unwrap();
        fs::write(target.join("debug/deps/demo"), [0_u8; 32]).unwrap();

        let paths = temporary_paths_at(&private_tmp, &user_temp, fixture.path());
        let target = target.canonicalize().unwrap();
        let workspace = workspace.canonicalize().unwrap();

        let cache = paths.iter().find(|path| path.path == target).unwrap();
        assert_eq!(cache.kind, "macos-temporary-build-cache");
        assert!(cache.cleanable);
        assert!(paths.iter().all(|path| path.path != workspace));
    }

    #[test]
    fn tagged_cache_inside_unmarked_temporary_tree_is_cleanable() {
        let fixture = tempdir().unwrap();
        let private_tmp = fixture.path().join("private-tmp");
        let user_temp = fixture.path().join("user-temp");
        let target = private_tmp.join("automation-output/repo/target");
        fs::create_dir_all(target.join("debug/deps")).unwrap();
        fs::write(
            target.join("CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55\n",
        )
        .unwrap();

        let paths = temporary_paths_at(&private_tmp, &user_temp, fixture.path());
        let target = target.canonicalize().unwrap();

        let cache = paths.iter().find(|path| path.path == target).unwrap();
        assert_eq!(cache.kind, "macos-temporary-build-cache");
        assert!(cache.cleanable);
    }

    #[test]
    fn structural_go_and_xcode_caches_are_cleanable() {
        let fixture = tempdir().unwrap();
        let private_tmp = fixture.path().join("private-tmp");
        let user_temp = fixture.path().join("user-temp");
        let go_cache = private_tmp.join("opaque-review-output");
        fs::create_dir_all(go_cache.join("0a")).unwrap();
        fs::write(
            go_cache.join("README"),
            "This directory holds cached build artifacts from the Go build system.\n",
        )
        .unwrap();
        let derived_data = private_tmp.join("opaque-ios-output");
        fs::create_dir_all(derived_data.join("Build/Intermediates.noindex")).unwrap();
        fs::create_dir_all(derived_data.join("Build/Products")).unwrap();
        fs::create_dir_all(derived_data.join("Logs")).unwrap();

        let paths = temporary_paths_at(&private_tmp, &user_temp, fixture.path());
        let go_cache = go_cache.canonicalize().unwrap();
        let derived_data = derived_data.canonicalize().unwrap();

        for cache in [go_cache, derived_data] {
            let candidate = paths.iter().find(|path| path.path == cache).unwrap();
            assert_eq!(candidate.kind, "macos-temporary-build-cache");
            assert!(candidate.cleanable);
        }
    }

    #[test]
    fn partial_structural_signatures_are_rejected() {
        let fixture = tempdir().unwrap();
        let fake_go = fixture.path().join("fake-go");
        fs::create_dir_all(fake_go.join("not-a-bucket")).unwrap();
        fs::write(
            fake_go.join("README"),
            "This directory holds cached build artifacts from the Go build system.\n",
        )
        .unwrap();
        let fake_xcode = fixture.path().join("fake-xcode");
        fs::create_dir_all(fake_xcode.join("Build/Intermediates.noindex")).unwrap();

        assert!(!recognized_temporary_build_cache(&fake_go));
        assert!(!recognized_temporary_build_cache(&fake_xcode));
    }

    #[test]
    fn invalid_nested_tag_does_not_hide_scan_only_workspace() {
        let fixture = tempdir().unwrap();
        let private_tmp = fixture.path().join("private-tmp");
        let user_temp = fixture.path().join("user-temp");
        let workspace = private_tmp.join("review-workspace");
        let target = workspace.join("target");
        fs::create_dir_all(&target).unwrap();
        fs::write(workspace.join("Cargo.toml"), "[package]").unwrap();
        fs::write(target.join("CACHEDIR.TAG"), "not a valid signature\n").unwrap();

        let paths = temporary_paths_at(&private_tmp, &user_temp, fixture.path());
        let workspace = workspace.canonicalize().unwrap();
        let target = target.canonicalize().unwrap();

        let workspace_candidate = paths.iter().find(|path| path.path == workspace).unwrap();
        assert_eq!(workspace_candidate.kind, "macos-temporary-workspace");
        assert!(!workspace_candidate.cleanable);
        assert!(paths.iter().all(|path| path.path != target));
    }

    #[test]
    fn chrome_clone_root_rejects_unrecognized_contents() {
        let fixture = tempdir().unwrap();
        let chrome = fixture.path().join("X/com.google.Chrome.code_sign_clone");
        fs::create_dir_all(chrome.join("code_sign_clone.abc123")).unwrap();
        fs::create_dir(chrome.join("keep-me")).unwrap();
        let owner = owner_id(fixture.path()).unwrap();

        assert!(!valid_chrome_signing_clone_root(&chrome, owner));
    }
}
