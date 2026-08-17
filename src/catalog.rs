use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::{CacheScope, CatalogEntry};

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProjectKind {
    pub kind: &'static str,
    pub ecosystem: &'static str,
    pub network_restore: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct GlobalPath {
    pub path: PathBuf,
    pub kind: &'static str,
    pub ecosystem: &'static str,
    pub network_restore: bool,
    pub cleanable: bool,
}

macro_rules! entry {
    ($kind:literal, $ecosystem:literal, $scope:ident, $description:literal, $network:literal) => {
        CatalogEntry {
            kind: $kind,
            ecosystem: $ecosystem,
            scope: CacheScope::$scope,
            description: $description,
            network_restore: $network,
            cleanable: true,
        }
    };
}

macro_rules! blocked_entry {
    ($kind:literal, $ecosystem:literal, $scope:ident, $description:literal, $network:literal) => {
        CatalogEntry {
            kind: $kind,
            ecosystem: $ecosystem,
            scope: CacheScope::$scope,
            description: $description,
            network_restore: $network,
            cleanable: false,
        }
    };
}

/// Supported cache kinds. This is deliberately a closed, reviewable safety catalog.
pub fn catalog() -> Vec<CatalogEntry> {
    vec![
        entry!(
            "cargo-target",
            "rust",
            Project,
            "Cargo target directory",
            false
        ),
        entry!(
            "cargo-registry",
            "rust",
            Global,
            "Cargo downloaded crate archives",
            true
        ),
        entry!(
            "cargo-git",
            "rust",
            Global,
            "Cargo git dependency checkouts",
            true
        ),
        entry!(
            "python-venv",
            "python",
            Project,
            "Python virtual environment",
            true
        ),
        entry!(
            "python-bytecode",
            "python",
            Project,
            "Python __pycache__ bytecode",
            false
        ),
        entry!(
            "pytest-cache",
            "python",
            Project,
            "pytest result cache",
            false
        ),
        entry!(
            "mypy-cache",
            "python",
            Project,
            "mypy analysis cache",
            false
        ),
        entry!(
            "ruff-cache",
            "python",
            Project,
            "Ruff analysis cache",
            false
        ),
        entry!("tox-env", "python", Project, "tox environments", true),
        entry!("nox-env", "python", Project, "nox environments", true),
        entry!(
            "pip-cache",
            "python",
            Global,
            "pip download and wheel cache",
            true
        ),
        entry!("uv-cache", "python", Global, "uv package cache", true),
        entry!(
            "node-modules",
            "javascript",
            Project,
            "Node dependency installation",
            true
        ),
        entry!(
            "npm-cache",
            "javascript",
            Global,
            "npm content-addressed cache",
            true
        ),
        entry!(
            "pnpm-store",
            "javascript",
            Global,
            "pnpm package store",
            true
        ),
        entry!("bun-cache", "javascript", Global, "Bun package cache", true),
        entry!(
            "deno-cache",
            "javascript",
            Global,
            "Deno dependency cache",
            true
        ),
        entry!(
            "go-build-cache",
            "go",
            Global,
            "Go compiler build cache",
            false
        ),
        entry!(
            "go-module-cache",
            "go",
            Global,
            "Go downloaded module cache",
            true
        ),
        entry!("gradle-build", "jvm", Project, "Gradle build output", false),
        entry!(
            "gradle-project-cache",
            "jvm",
            Project,
            "Per-project Gradle cache",
            false
        ),
        entry!(
            "maven-target",
            "jvm",
            Project,
            "Maven target directory",
            false
        ),
        entry!(
            "gradle-global-cache",
            "jvm",
            Global,
            "Shared Gradle caches",
            true
        ),
        blocked_entry!(
            "maven-repository",
            "jvm",
            Global,
            "Local Maven repository (scan only; may contain unpublished artifacts)",
            true
        ),
        entry!(
            "dotnet-bin",
            "dotnet",
            Project,
            ".NET build binaries",
            false
        ),
        entry!(
            "dotnet-obj",
            "dotnet",
            Project,
            ".NET intermediate objects",
            false
        ),
        entry!(
            "nuget-packages",
            "dotnet",
            Global,
            "NuGet global packages",
            true
        ),
        entry!(
            "bundler-vendor",
            "ruby",
            Project,
            "Bundler vendored gems",
            true
        ),
        entry!(
            "gem-cache",
            "ruby",
            Global,
            "RubyGems package archives",
            true
        ),
        entry!(
            "composer-vendor",
            "php",
            Project,
            "Composer dependencies",
            true
        ),
        entry!(
            "composer-cache",
            "php",
            Global,
            "Composer package cache",
            true
        ),
        entry!(
            "swift-build",
            "swift",
            Project,
            "Swift Package Manager build output",
            false
        ),
        entry!(
            "swiftpm-cache",
            "swift",
            Global,
            "SwiftPM shared cache",
            true
        ),
        entry!(
            "xcode-derived-data",
            "swift",
            Global,
            "Xcode DerivedData",
            false
        ),
        entry!(
            "cmake-build",
            "cpp",
            Project,
            "CMake generated build tree",
            false
        ),
        entry!("ccache", "cpp", Global, "ccache compiler cache", false),
        entry!(
            "zig-project-cache",
            "zig",
            Project,
            "Zig project build cache",
            false
        ),
        entry!("zig-global-cache", "zig", Global, "Zig shared cache", true),
        entry!(
            "dart-tool",
            "dart",
            Project,
            "Dart and Flutter tool state",
            true
        ),
        entry!(
            "dart-build",
            "dart",
            Project,
            "Dart and Flutter build output",
            false
        ),
        entry!("pub-cache", "dart", Global, "Dart pub package cache", true),
        entry!("mix-build", "elixir", Project, "Mix build output", false),
        entry!("mix-deps", "elixir", Project, "Mix dependencies", true),
        entry!("hex-cache", "elixir", Global, "Hex package cache", true),
        entry!(
            "stack-work",
            "haskell",
            Project,
            "Stack project build state",
            false
        ),
        entry!(
            "cabal-dist",
            "haskell",
            Project,
            "Cabal new-build output",
            false
        ),
        entry!(
            "stack-cache",
            "haskell",
            Global,
            "Stack shared programs and snapshots",
            true
        ),
        entry!(
            "cabal-store",
            "haskell",
            Global,
            "Cabal package store",
            true
        ),
        entry!(
            "terraform-modules",
            "terraform",
            Project,
            "Terraform providers and modules",
            true
        ),
        entry!(
            "terraform-plugin-cache",
            "terraform",
            Global,
            "Configured Terraform provider cache",
            true
        ),
        entry!("renv-library", "r", Project, "renv project library", true),
        blocked_entry!(
            "renv-cache",
            "r",
            Global,
            "renv shared package cache (scan only; may back project libraries)",
            true
        ),
        entry!(
            "cachedir-tag",
            "other",
            Project,
            "Directory carrying a valid CACHEDIR.TAG",
            false
        ),
    ]
}

pub(crate) fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

/// Conservative default project roots: common source directories, then cwd.
pub fn default_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = home_dir() {
        for name in ["Projects", "Developer", "src", "code"] {
            let path = home.join(name);
            if path.is_dir() {
                roots.push(path);
            }
        }
    }
    if roots.is_empty()
        && let Ok(cwd) = env::current_dir()
    {
        roots.push(cwd);
    }
    roots
}

pub(crate) fn identify_project_cache(path: &Path) -> Option<ProjectKind> {
    let name = path.file_name()?.to_str()?;
    let parent = path.parent()?;

    let identified = match name {
        "target" if parent.join("Cargo.toml").is_file() => {
            Some(project("cargo-target", "rust", false))
        }
        "target" if parent.join("pom.xml").is_file() => Some(project("maven-target", "jvm", false)),
        ".venv" | "venv" if path.join("pyvenv.cfg").is_file() => {
            Some(project("python-venv", "python", true))
        }
        "__pycache__" => Some(project("python-bytecode", "python", false)),
        ".pytest_cache" => Some(project("pytest-cache", "python", false)),
        ".mypy_cache" => Some(project("mypy-cache", "python", false)),
        ".ruff_cache" => Some(project("ruff-cache", "python", false)),
        ".tox" if has_any_file(parent, &["tox.ini", "pyproject.toml"]) => {
            Some(project("tox-env", "python", true))
        }
        ".nox" if has_any_file(parent, &["noxfile.py", "pyproject.toml"]) => {
            Some(project("nox-env", "python", true))
        }
        "node_modules" if has_any_file(parent, &["package.json", "deno.json", "deno.jsonc"]) => {
            Some(project("node-modules", "javascript", true))
        }
        ".gradle" if is_gradle_project(parent) => {
            Some(project("gradle-project-cache", "jvm", false))
        }
        "build" if is_gradle_project(parent) => Some(project("gradle-build", "jvm", false)),
        "build" if parent.join("pubspec.yaml").is_file() => {
            Some(project("dart-build", "dart", false))
        }
        "build" if path.join("CMakeCache.txt").is_file() => {
            Some(project("cmake-build", "cpp", false))
        }
        value if value.starts_with("cmake-build-") && path.join("CMakeCache.txt").is_file() => {
            Some(project("cmake-build", "cpp", false))
        }
        "bin" if has_dotnet_project(parent) => Some(project("dotnet-bin", "dotnet", false)),
        "obj" if has_dotnet_project(parent) => Some(project("dotnet-obj", "dotnet", false)),
        "bundle"
            if parent.file_name().and_then(|part| part.to_str()) == Some("vendor")
                && parent
                    .parent()
                    .is_some_and(|root| root.join("Gemfile").is_file()) =>
        {
            Some(project("bundler-vendor", "ruby", true))
        }
        "vendor" if parent.join("composer.json").is_file() => {
            Some(project("composer-vendor", "php", true))
        }
        ".build" if parent.join("Package.swift").is_file() => {
            Some(project("swift-build", "swift", false))
        }
        "zig-cache" | ".zig-cache" if parent.join("build.zig").is_file() => {
            Some(project("zig-project-cache", "zig", false))
        }
        ".dart_tool" if parent.join("pubspec.yaml").is_file() => {
            Some(project("dart-tool", "dart", true))
        }
        "_build" if parent.join("mix.exs").is_file() => Some(project("mix-build", "elixir", false)),
        "deps" if parent.join("mix.exs").is_file() => Some(project("mix-deps", "elixir", true)),
        ".stack-work" if has_any_file(parent, &["stack.yaml", "package.yaml"]) => {
            Some(project("stack-work", "haskell", false))
        }
        "dist-newstyle"
            if parent.join("cabal.project").is_file() || has_extension(parent, "cabal") =>
        {
            Some(project("cabal-dist", "haskell", false))
        }
        ".terraform" if has_extension(parent, "tf") => {
            Some(project("terraform-modules", "terraform", true))
        }
        "library"
            if parent.file_name().and_then(|part| part.to_str()) == Some("renv")
                && parent
                    .parent()
                    .is_some_and(|root| root.join("renv.lock").is_file()) =>
        {
            Some(project("renv-library", "r", true))
        }
        _ => None,
    };

    identified.or_else(|| valid_cachedir_tag(path).then(|| project("cachedir-tag", "other", false)))
}

fn project(kind: &'static str, ecosystem: &'static str, network_restore: bool) -> ProjectKind {
    ProjectKind {
        kind,
        ecosystem,
        network_restore,
    }
}

fn has_any_file(parent: &Path, names: &[&str]) -> bool {
    names.iter().any(|name| parent.join(name).is_file())
}

fn is_gradle_project(parent: &Path) -> bool {
    has_any_file(
        parent,
        &[
            "build.gradle",
            "build.gradle.kts",
            "settings.gradle",
            "settings.gradle.kts",
        ],
    )
}

fn has_extension(parent: &Path, extension: &str) -> bool {
    fs::read_dir(parent).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            entry.path().extension().and_then(|value| value.to_str()) == Some(extension)
        })
    })
}

fn has_dotnet_project(parent: &Path) -> bool {
    ["csproj", "fsproj", "vbproj"]
        .into_iter()
        .any(|extension| has_extension(parent, extension))
}

fn valid_cachedir_tag(path: &Path) -> bool {
    const SIGNATURE: &str = "Signature: 8a477f597d28d172789f06886806bc55";
    fs::read_to_string(path.join("CACHEDIR.TAG"))
        .is_ok_and(|contents| contents.lines().next() == Some(SIGNATURE))
}

pub(crate) fn global_paths() -> Vec<GlobalPath> {
    let home = home_dir();
    let cache = env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|home| home.join(".cache")));
    let data = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|home| home.join(".local/share")));

    let mut paths = Vec::new();
    if let Some(home) = &home {
        paths.extend([
            global(
                home.join(".cargo/registry/cache"),
                "cargo-registry",
                "rust",
                true,
            ),
            global(home.join(".cargo/git/checkouts"), "cargo-git", "rust", true),
            global(home.join(".npm/_cacache"), "npm-cache", "javascript", true),
            global(
                home.join(".bun/install/cache"),
                "bun-cache",
                "javascript",
                true,
            ),
            global(
                home.join(".gradle/caches"),
                "gradle-global-cache",
                "jvm",
                true,
            ),
            global_blocked(home.join(".m2/repository"), "maven-repository", "jvm", true),
            global(
                home.join(".nuget/packages"),
                "nuget-packages",
                "dotnet",
                true,
            ),
            global(home.join(".pub-cache"), "pub-cache", "dart", true),
            global(home.join(".stack/programs"), "stack-cache", "haskell", true),
            global(
                home.join(".stack/snapshots"),
                "stack-cache",
                "haskell",
                true,
            ),
            global(home.join(".stack/indices"), "stack-cache", "haskell", true),
            global(home.join(".cabal/store"), "cabal-store", "haskell", true),
            global(home.join(".hex/packages"), "hex-cache", "elixir", true),
            global(home.join("go/pkg/mod"), "go-module-cache", "go", true),
            global(home.join(".swiftpm/cache"), "swiftpm-cache", "swift", true),
        ]);

        if cfg!(target_os = "macos") {
            let mac_caches = home.join("Library/Caches");
            paths.extend([
                global(mac_caches.join("pip"), "pip-cache", "python", true),
                global(mac_caches.join("uv"), "uv-cache", "python", true),
                global(mac_caches.join("deno"), "deno-cache", "javascript", true),
                global(mac_caches.join("go-build"), "go-build-cache", "go", false),
                global(mac_caches.join("composer"), "composer-cache", "php", true),
                global(mac_caches.join("ccache"), "ccache", "cpp", false),
                global(
                    home.join("Library/pnpm/store"),
                    "pnpm-store",
                    "javascript",
                    true,
                ),
                global(
                    home.join("Library/Developer/Xcode/DerivedData"),
                    "xcode-derived-data",
                    "swift",
                    false,
                ),
                global(
                    mac_caches.join("org.swift.swiftpm"),
                    "swiftpm-cache",
                    "swift",
                    true,
                ),
            ]);
        }

        add_ruby_gem_caches(home, &mut paths);
    }

    if let Some(cache) = &cache {
        paths.extend([
            global(cache.join("pip"), "pip-cache", "python", true),
            global(cache.join("uv"), "uv-cache", "python", true),
            global(cache.join("deno"), "deno-cache", "javascript", true),
            global(cache.join("go-build"), "go-build-cache", "go", false),
            global(cache.join("ccache"), "ccache", "cpp", false),
            global(cache.join("zig"), "zig-global-cache", "zig", true),
            global(cache.join("composer"), "composer-cache", "php", true),
            global_blocked(cache.join("R/renv"), "renv-cache", "r", true),
        ]);
    }
    if let Some(data) = &data {
        paths.push(global(
            data.join("pnpm/store"),
            "pnpm-store",
            "javascript",
            true,
        ));
    }

    for (variable, kind, ecosystem, network) in [
        ("DENO_DIR", "deno-cache", "javascript", true),
        ("GOCACHE", "go-build-cache", "go", false),
        ("GOMODCACHE", "go-module-cache", "go", true),
        (
            "TF_PLUGIN_CACHE_DIR",
            "terraform-plugin-cache",
            "terraform",
            true,
        ),
    ] {
        if let Some(value) = env::var_os(variable) {
            let path = PathBuf::from(value);
            if safe_dynamic_global_path(variable, &path, home.as_deref()) {
                paths.push(global(path, kind, ecosystem, network));
            }
        }
    }

    let mut seen = HashSet::new();
    paths.retain(|path| seen.insert(path.path.clone()));
    paths
}

fn safe_dynamic_global_path(variable: &str, path: &Path, home: Option<&Path>) -> bool {
    if !path.is_absolute()
        || path == Path::new("/")
        || home.is_some_and(|home| path == home || home.starts_with(path))
    {
        return false;
    }
    let leaf = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match variable {
        "DENO_DIR" => leaf.contains("deno"),
        "GOCACHE" => leaf.contains("go-build") || leaf.contains("gocache"),
        "GOMODCACHE" => leaf == "mod" || leaf.contains("gomod"),
        "TF_PLUGIN_CACHE_DIR" => leaf.contains("terraform") || leaf.contains("plugin"),
        _ => false,
    }
}

fn global(
    path: PathBuf,
    kind: &'static str,
    ecosystem: &'static str,
    network_restore: bool,
) -> GlobalPath {
    GlobalPath {
        path,
        kind,
        ecosystem,
        network_restore,
        cleanable: true,
    }
}

fn global_blocked(
    path: PathBuf,
    kind: &'static str,
    ecosystem: &'static str,
    network_restore: bool,
) -> GlobalPath {
    GlobalPath {
        path,
        kind,
        ecosystem,
        network_restore,
        cleanable: false,
    }
}

fn add_ruby_gem_caches(home: &Path, paths: &mut Vec<GlobalPath>) {
    let ruby_root = home.join(".gem/ruby");
    if let Ok(versions) = fs::read_dir(ruby_root) {
        for version in versions.flatten() {
            let path = version.path().join("cache");
            paths.push(global(path, "gem-cache", "ruby", true));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dynamic_cache_paths_reject_broad_or_unrelated_directories() {
        let home = Path::new("/home/developer");
        assert!(!safe_dynamic_global_path(
            "GOCACHE",
            Path::new("/"),
            Some(home)
        ));
        assert!(!safe_dynamic_global_path("GOCACHE", home, Some(home)));
        assert!(!safe_dynamic_global_path(
            "GOCACHE",
            Path::new("/tmp"),
            Some(home)
        ));
        assert!(safe_dynamic_global_path(
            "GOCACHE",
            Path::new("/var/cache/go-build"),
            Some(home)
        ));
        assert!(safe_dynamic_global_path(
            "GOCACHE",
            Path::new("/var/cache/go-build"),
            None
        ));
    }

    #[test]
    fn potentially_irreplaceable_shared_stores_are_scan_only() {
        let entries = catalog();
        for kind in ["maven-repository", "renv-cache"] {
            let entry = entries.iter().find(|entry| entry.kind == kind).unwrap();
            assert!(!entry.cleanable, "{kind} must remain scan-only");
        }
    }
}
