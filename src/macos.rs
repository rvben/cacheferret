use std::fs;
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
use std::env;

use crate::catalog::{GlobalPath, valid_cachedir_tag};

const LARGE_TEMP_WORKSPACE_BYTES: u64 = 100 * 1024 * 1024;

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
        } else if looks_like_temporary_workspace(&path) {
            paths.push(candidate(
                path,
                "macos-temporary-workspace",
                false,
                LARGE_TEMP_WORKSPACE_BYTES,
            ));
        }
    }
    paths
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
    if valid_cachedir_tag(path) {
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
    fn chrome_clone_root_rejects_unrecognized_contents() {
        let fixture = tempdir().unwrap();
        let chrome = fixture.path().join("X/com.google.Chrome.code_sign_clone");
        fs::create_dir_all(chrome.join("code_sign_clone.abc123")).unwrap();
        fs::create_dir(chrome.join("keep-me")).unwrap();
        let owner = owner_id(fixture.path()).unwrap();

        assert!(!valid_chrome_signing_clone_root(&chrome, owner));
    }
}
