//! Rust toolchains installed by rustup.
//!
//! Each directory under `<rustup home>/toolchains` is offered as its own
//! cache entry unless rustup itself would still select it: the default
//! toolchain, any directory override and `RUSTUP_TOOLCHAIN`, each of which may
//! name a toolchain or give its absolute directory (resolved before comparing).
//! A selection that is neither makes the whole kind unavailable. Removal goes
//! through `rustup toolchain uninstall`, which also drops the toolchain from
//! rustup's own bookkeeping; the directory is never deleted directly.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::CacheCandidate;
use crate::native::{self, AgeSignal, Planned, Resolved, Sizing, Target};

pub(crate) const KIND: &str = "rust-toolchain";
const ECOSYSTEM: &str = "rust";
const INSPECTION_TIMEOUT: Duration = Duration::from_secs(10);
const UNINSTALL_TIMEOUT: Duration = Duration::from_secs(300);

/// Every removable toolchain, or the warnings explaining why none are offered.
pub(crate) fn plan() -> (Vec<Planned>, Vec<String>) {
    let home = match rustup_home() {
        Ok(home) => home,
        Err(unavailable) => {
            return (
                Vec::new(),
                unavailable
                    .warning("rustup", "toolchains are not offered")
                    .into_iter()
                    .collect(),
            );
        }
    };
    match removable(&home, std::env::var("RUSTUP_TOOLCHAIN").ok().as_deref()) {
        Ok(planned) => (planned, Vec::new()),
        Err(message) => (
            Vec::new(),
            vec![format!("rustup: {message}; toolchains are not offered")],
        ),
    }
}

/// Re-derive the candidate from rustup's current state. The candidate must
/// still be a removable toolchain at the exact path it was scanned at.
pub(crate) fn resolve(candidate: &CacheCandidate) -> Result<Resolved, String> {
    let home = rustup_home().map_err(|unavailable| unavailable.message("rustup"))?;
    let planned = removable(&home, std::env::var("RUSTUP_TOOLCHAIN").ok().as_deref())?
        .into_iter()
        .find(|planned| planned.path == candidate.path)
        .ok_or_else(|| {
            "toolchain is no longer installed, or is now the default or an override".to_owned()
        })?;
    let name = file_name(&planned.path)
        .ok_or_else(|| "toolchain has no usable directory name".to_owned())?;
    Ok(Resolved {
        target: Target::Toolchain(name),
        planned,
    })
}

pub(crate) fn uninstall(name: &str, path: &Path) -> Result<(), String> {
    native::run_tool(
        "rustup",
        &["toolchain", "uninstall", name],
        UNINSTALL_TIMEOUT,
        "toolchain uninstall",
    )
    .map_err(|unavailable| unavailable.message("rustup"))?;
    if fs::symlink_metadata(path).is_ok() {
        return Err("rustup reported success but the toolchain directory is still present".into());
    }
    Ok(())
}

fn rustup_home() -> Result<PathBuf, native::Unavailable> {
    let output = native::run_tool("rustup", &["show", "home"], INSPECTION_TIMEOUT, "show home")?;
    let text = output.stdout_text();
    let line = text.trim();
    let home = PathBuf::from(line);
    if line.is_empty() || line.contains('\n') || !home.is_absolute() {
        return Err(native::Unavailable::Problem(
            "rustup show home did not print a single absolute path".to_owned(),
        ));
    }
    Ok(home)
}

/// Toolchains under `home` that nothing selects. Fails closed: when the
/// settings file is missing or not fully understood nothing is offered,
/// because the default toolchain could not be told apart from the rest.
fn removable(home: &Path, env_toolchain: Option<&str>) -> Result<Vec<Planned>, String> {
    let settings = fs::read_to_string(home.join("settings.toml"))
        .map_err(|error| format!("settings.toml cannot be read: {error}"))?;
    let mut in_use = toolchains_in_use(&settings)
        .ok_or_else(|| "settings.toml has a shape that is not understood".to_owned())?;
    if let Some(selected) = env_toolchain.filter(|selected| !selected.is_empty()) {
        in_use.insert(selected.to_owned());
    }
    let selections = Selections::from(in_use)?;

    let root = home.join("toolchains");
    let Ok(entries) = fs::read_dir(&root) else {
        return Ok(Vec::new());
    };
    let mut planned: Vec<Planned> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if !valid_name(&name) || names_match(&name, &selections.names) {
                return None;
            }
            let kind = entry.file_type().ok()?;
            if kind.is_symlink() || !kind.is_dir() {
                return None;
            }
            if selections.covers(&entry.path()) {
                return None;
            }
            Some(Planned {
                kind: KIND,
                ecosystem: ECOSYSTEM,
                label: Some(label(&name)),
                path: entry.path(),
                network_restore: true,
                cleanable: true,
                unprotected_because: None,
                sizing: Sizing::Tree {
                    age: AgeSignal::FileUse,
                    minimum_bytes: 1,
                },
            })
        })
        .collect();
    planned.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(planned)
}

/// What rustup can be told to select: a toolchain name, or (in
/// `RUSTUP_TOOLCHAIN` and override values) the absolute path of a toolchain
/// directory.
struct Selections {
    names: HashSet<String>,
    /// Canonical paths of the directories selected by path.
    paths: Vec<PathBuf>,
}

impl Selections {
    /// Fails when a selection is neither a recognizable name nor an absolute
    /// path whose location can be determined, since a selection that cannot be
    /// interpreted could be protecting any toolchain.
    fn from(selected: HashSet<String>) -> Result<Self, String> {
        let mut names = HashSet::new();
        let mut paths = Vec::new();
        for selection in selected {
            if Path::new(&selection).is_absolute() {
                match fs::canonicalize(&selection) {
                    Ok(path) => paths.push(path),
                    // Nothing exists there, so it selects no installed toolchain.
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(format!(
                            "the selected toolchain path {selection:?} cannot be resolved: {error}"
                        ));
                    }
                }
            } else if valid_name(&selection) {
                names.insert(selection);
            } else {
                return Err(format!(
                    "the selected toolchain {selection:?} is neither a toolchain name nor an absolute path"
                ));
            }
        }
        Ok(Self { names, paths })
    }

    /// Whether a path selection is this toolchain directory or lies inside it.
    fn covers(&self, directory: &Path) -> bool {
        let Ok(directory) = fs::canonicalize(directory) else {
            // Unresolvable, so it cannot be shown unselected.
            return true;
        };
        self.paths.iter().any(|path| path.starts_with(&directory))
    }
}

/// Names reach rustup's argv, and hidden or `__` names are rustup's own
/// bookkeeping.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(['.', '-'])
        && !name.starts_with("__")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+'))
}

fn file_name(path: &Path) -> Option<String> {
    path.file_name()?.to_str().map(str::to_owned)
}

/// rustup records full names (`stable-aarch64-apple-darwin`), but a short name
/// such as `stable` also selects the full directory it expands to.
fn names_match(directory: &str, in_use: &HashSet<String>) -> bool {
    in_use.iter().any(|used| {
        directory == used
            || directory
                .strip_prefix(used.as_str())
                .is_some_and(|rest| rest.starts_with('-'))
    })
}

/// The toolchain name without its host triple, so `1.88.0-aarch64-apple-darwin`
/// reads as `1.88.0`.
fn label(directory: &str) -> String {
    const HOSTS: [&str; 2] = ["aarch64-apple-darwin", "x86_64-apple-darwin"];
    HOSTS
        .iter()
        .find_map(|host| directory.strip_suffix(host)?.strip_suffix('-'))
        .filter(|name| !name.is_empty())
        .unwrap_or(directory)
        .to_owned()
}

#[derive(PartialEq)]
enum Section {
    Top,
    Overrides,
    Other,
}

/// The little of `settings.toml` that decides which toolchains are in use: the
/// top-level `default_toolchain` and every value in the `[overrides]` table.
///
/// rustup writes the file itself in a fixed, flat shape, so it is read line by
/// line. The reader fails closed: any header, key or value touching either
/// setting that it does not fully understand returns `None`, since a default or
/// override it failed to read would otherwise be offered for removal like any
/// unused toolchain. Unrelated keys and tables are skipped.
fn toolchains_in_use(text: &str) -> Option<HashSet<String>> {
    // A multi-line string can hold lines that look like headers or keys.
    if text.contains("\"\"\"") || text.contains("'''") {
        return None;
    }
    let mut in_use = HashSet::new();
    let mut section = Section::Top;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mentions_in_use = line.contains("overrides") || line.contains("default_toolchain");
        if line.starts_with('[') {
            let header: String = line
                .split('#')
                .next()
                .unwrap_or_default()
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            if matches!(
                header.as_str(),
                "[overrides]" | "[\"overrides\"]" | "['overrides']"
            ) {
                section = Section::Overrides;
            } else if mentions_in_use {
                return None;
            } else {
                section = Section::Other;
            }
            continue;
        }
        match section {
            Section::Other => {}
            Section::Top => match split_key(line) {
                None if mentions_in_use => return None,
                None => {}
                Some(("default_toolchain", rest)) => {
                    in_use.insert(string_value(rest)?);
                }
                Some((key, _))
                    if key.contains("overrides") || key.contains("default_toolchain") =>
                {
                    return None;
                }
                Some(_) => {}
            },
            Section::Overrides => {
                let (_, rest) = split_key(line)?;
                in_use.insert(string_value(rest)?);
            }
        }
    }
    Some(in_use)
}

/// Split `key = rest` where the key is bare or a single quoted string holding no
/// escape. A bare dotted key keeps its dots, so it never equals a plain name it
/// merely contains, and a key of any other shape is not split at all.
fn split_key(line: &str) -> Option<(&str, &str)> {
    let (key, rest) = match line.chars().next()? {
        quote @ ('"' | '\'') => {
            let body = &line[1..];
            let close = body.find(quote)?;
            let key = &body[..close];
            if key.contains('\\') {
                return None;
            }
            (key, &body[close + 1..])
        }
        _ => {
            let end = line
                .find(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '.')))
                .unwrap_or(line.len());
            if end == 0 {
                return None;
            }
            (&line[..end], &line[end..])
        }
    };
    let rest = rest.trim_start_matches([' ', '\t']).strip_prefix('=')?;
    Some((key, rest))
}

/// A basic or literal TOML string holding no escape, optionally followed by a
/// comment. Toolchain names never contain one, so anything else is not
/// understood.
fn string_value(raw: &str) -> Option<String> {
    let value = raw.trim();
    let quote = value.chars().next().filter(|c| matches!(c, '"' | '\''))?;
    let body = &value[1..];
    let close = body.find(quote)?;
    let name = &body[..close];
    let rest = body[close + 1..].trim();
    (!name.is_empty() && !name.contains('\\') && (rest.is_empty() || rest.starts_with('#')))
        .then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT_ONLY: &str = "default_toolchain = \"stable-aarch64-apple-darwin\"\nprofile = \"default\"\nversion = \"12\"\n";

    fn set(names: &[&str]) -> HashSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn reads_the_default_toolchain() {
        assert_eq!(
            toolchains_in_use(DEFAULT_ONLY),
            Some(set(&["stable-aarch64-apple-darwin"]))
        );
    }

    #[test]
    fn reads_every_override_value() {
        let text = format!(
            "{DEFAULT_ONLY}\n[overrides]\n\"/Users/example/app\" = \"nightly-2026-01-01-aarch64-apple-darwin\"\n'/Users/example/lib' = '1.80.0'  # pinned\n\n[other]\nkey = \"x\"\n"
        );
        assert_eq!(
            toolchains_in_use(&text),
            Some(set(&[
                "stable-aarch64-apple-darwin",
                "nightly-2026-01-01-aarch64-apple-darwin",
                "1.80.0"
            ]))
        );
    }

    #[test]
    fn unrelated_keys_and_tables_are_skipped() {
        let text = "version = \"12\"\nfuture_flag = true\n[telemetry]\nmode = 'off'\n";
        assert_eq!(toolchains_in_use(text), Some(HashSet::new()));
    }

    #[test]
    fn a_shape_that_touches_the_selection_but_is_not_understood_fails_closed() {
        for text in [
            "default_toolchain = stable\n",
            "default_toolchain = [\"stable\"]\n",
            "default_toolchain = \"sta\\\"ble\"\n",
            "default_toolchain = \"\"\n",
            "default_toolchain = \"stable\" trailing\n",
            "overrides.\"/p\" = \"stable\"\n",
            "\"default_toolchain\" extra = \"stable\"\n",
            "[overrides.nested]\nkey = \"stable\"\n",
            "[[overrides]]\nkey = \"stable\"\n",
            "[overrides]\n\"/p\" = 3\n",
            "[overrides]\n\"/p\" = \"a\" \"b\"\n",
            "overrides_x = 1\n",
            "note = \"\"\"\n[overrides]\n\"\"\"\n",
            "note = '''\ndefault_toolchain = \"x\"\n'''\n",
        ] {
            assert_eq!(toolchains_in_use(text), None, "{text}");
        }
    }

    #[test]
    fn a_short_name_selects_the_full_directory_it_expands_to() {
        let in_use = set(&["stable", "1.80.0"]);
        assert!(names_match("stable-aarch64-apple-darwin", &in_use));
        assert!(names_match("1.80.0", &in_use));
        assert!(names_match("1.80.0-aarch64-apple-darwin", &in_use));
        assert!(!names_match("stable2-aarch64-apple-darwin", &in_use));
        assert!(!names_match("nightly-aarch64-apple-darwin", &in_use));
    }

    #[test]
    fn labels_drop_the_host_triple_only() {
        assert_eq!(label("1.88.0-aarch64-apple-darwin"), "1.88.0");
        assert_eq!(
            label("nightly-2026-01-01-x86_64-apple-darwin"),
            "nightly-2026-01-01"
        );
        assert_eq!(label("custom"), "custom");
        assert_eq!(label("aarch64-apple-darwin"), "aarch64-apple-darwin");
    }

    fn rustup_home(settings: Option<&str>, toolchains: &[&str]) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        if let Some(settings) = settings {
            fs::write(home.path().join("settings.toml"), settings).unwrap();
        }
        for name in toolchains {
            let directory = home.path().join("toolchains").join(name);
            fs::create_dir_all(directory.join("bin")).unwrap();
            fs::write(directory.join("bin/rustc"), [0_u8; 64]).unwrap();
        }
        home
    }

    fn names(planned: &[Planned]) -> Vec<String> {
        planned
            .iter()
            .map(|planned| file_name(&planned.path).unwrap())
            .collect()
    }

    #[test]
    fn default_override_and_environment_toolchains_are_never_offered() {
        let settings = format!("{DEFAULT_ONLY}[overrides]\n\"/Users/example/app\" = \"1.80.0\"\n");
        let home = rustup_home(
            Some(&settings),
            &[
                "stable-aarch64-apple-darwin",
                "1.80.0-aarch64-apple-darwin",
                "1.70.0-aarch64-apple-darwin",
                "nightly-aarch64-apple-darwin",
                "beta-aarch64-apple-darwin",
            ],
        );

        let planned = removable(home.path(), Some("beta")).unwrap();
        assert_eq!(
            names(&planned),
            [
                "1.70.0-aarch64-apple-darwin",
                "nightly-aarch64-apple-darwin"
            ]
        );
        assert_eq!(planned[0].label.as_deref(), Some("1.70.0"));
        assert!(planned[0].network_restore);
    }

    #[test]
    fn a_toolchain_selected_by_absolute_path_is_never_offered() {
        let home = rustup_home(
            Some(DEFAULT_ONLY),
            &[
                "1.70.0-aarch64-apple-darwin",
                "nightly-aarch64-apple-darwin",
                "beta-aarch64-apple-darwin",
            ],
        );
        let root = home.path().join("toolchains");
        let by_env = root.join("nightly-aarch64-apple-darwin");
        let planned = removable(home.path(), by_env.to_str()).unwrap();
        assert_eq!(
            names(&planned),
            ["1.70.0-aarch64-apple-darwin", "beta-aarch64-apple-darwin"]
        );

        let settings = format!(
            "{DEFAULT_ONLY}[overrides]\n\"/Users/example/app\" = \"{}\"\n",
            root.join("beta-aarch64-apple-darwin").display()
        );
        fs::write(home.path().join("settings.toml"), settings).unwrap();
        let planned = removable(home.path(), None).unwrap();
        assert_eq!(
            names(&planned),
            [
                "1.70.0-aarch64-apple-darwin",
                "nightly-aarch64-apple-darwin"
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_path_selection_through_a_symlink_protects_the_real_directory() {
        let home = rustup_home(Some(DEFAULT_ONLY), &["nightly-aarch64-apple-darwin"]);
        let alias = tempfile::tempdir().unwrap();
        let link = alias.path().join("selected");
        std::os::unix::fs::symlink(
            home.path().join("toolchains/nightly-aarch64-apple-darwin"),
            &link,
        )
        .unwrap();
        assert!(removable(home.path(), link.to_str()).unwrap().is_empty());
    }

    #[test]
    fn selections_that_are_not_names_or_absolute_paths_fail_closed() {
        let home = rustup_home(Some(DEFAULT_ONLY), &["1.70.0-aarch64-apple-darwin"]);
        for selection in ["../nightly", "relative/path", "-flag", "a b"] {
            assert!(
                removable(home.path(), Some(selection)).is_err(),
                "{selection:?} must fail closed"
            );
        }
        let missing = home.path().join("gone");
        assert_eq!(
            removable(home.path(), missing.to_str()).unwrap().len(),
            1,
            "a path that does not exist selects nothing installed"
        );
    }

    #[test]
    fn an_empty_environment_selection_selects_nothing() {
        let home = rustup_home(Some(DEFAULT_ONLY), &["1.70.0-aarch64-apple-darwin"]);
        assert_eq!(removable(home.path(), Some("")).unwrap().len(), 1);
    }

    #[test]
    fn nothing_is_offered_without_understood_settings() {
        let toolchains = ["1.70.0-aarch64-apple-darwin"];
        let missing = rustup_home(None, &toolchains);
        assert!(removable(missing.path(), None).is_err());
        let unknown = rustup_home(Some("default_toolchain = stable\n"), &toolchains);
        assert!(removable(unknown.path(), None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn linked_hidden_and_non_directory_entries_are_skipped() {
        let home = rustup_home(Some(DEFAULT_ONLY), &["1.70.0-aarch64-apple-darwin"]);
        let root = home.path().join("toolchains");
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("file"), b"x").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("linked")).unwrap();
        for hidden in [".hidden", "__bookkeeping", "-flag"] {
            fs::create_dir_all(root.join(hidden).join("bin")).unwrap();
        }
        fs::write(root.join("plain-file"), b"x").unwrap();

        let planned = removable(home.path(), None).unwrap();
        assert_eq!(names(&planned), ["1.70.0-aarch64-apple-darwin"]);
    }

    #[test]
    fn empty_toolchain_directories_are_measured_away() {
        let home = rustup_home(Some(DEFAULT_ONLY), &[]);
        fs::create_dir_all(home.path().join("toolchains/empty-aarch64-apple-darwin")).unwrap();
        let planned = removable(home.path(), None).unwrap();
        assert_eq!(planned.len(), 1);
        assert!(native::measure(planned.into_iter().next().unwrap(), 7, 0).is_none());
    }

    #[test]
    fn the_catalog_describes_the_kind() {
        assert!(crate::catalog().iter().any(|entry| entry.kind == KIND));
    }
}
