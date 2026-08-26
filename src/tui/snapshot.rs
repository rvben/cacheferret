use std::collections::HashMap;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use cacheferret::{CacheCandidate, CacheScope, DiscoveredCache, ScanReport, ScopeFilter};
use serde::{Deserialize, Serialize};

use super::Options;

const SNAPSHOT_VERSION: u32 = 1;
const MAX_SNAPSHOT_BYTES: u64 = 1024 * 1024;
const MAX_ENTRIES: usize = 4096;
const MAX_AGE_SECONDS: u64 = 30 * 24 * 60 * 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct RememberedCache {
    pub kind: String,
    pub ecosystem: String,
    pub scope: CacheScope,
    pub path: PathBuf,
    pub bytes: u64,
    pub allocated_bytes: u64,
    pub modified_unix: Option<u64>,
    pub cleanable: bool,
    pub(super) observed_unix: u64,
}

impl RememberedCache {
    pub(super) fn discovered(&self) -> DiscoveredCache {
        DiscoveredCache {
            kind: self.kind.clone(),
            ecosystem: self.ecosystem.clone(),
            scope: self.scope,
            path: self.path.clone(),
            cleanable: self.cleanable,
        }
    }

    pub(super) fn age_days(&self) -> Option<u64> {
        self.modified_unix
            .map(|modified| now_unix().saturating_sub(modified) / 86_400)
    }

    pub(super) fn observed_age(&self) -> String {
        let seconds = now_unix().saturating_sub(self.observed_unix);
        if seconds < 60 {
            "moments ago".to_owned()
        } else if seconds < 60 * 60 {
            format!("{}m ago", seconds / 60)
        } else if seconds < 24 * 60 * 60 {
            format!("{}h ago", seconds / (60 * 60))
        } else {
            format!("{}d ago", seconds / (24 * 60 * 60))
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Snapshot {
    version: u32,
    entries: Vec<RememberedCache>,
}

pub(super) fn load(options: &Options) -> Vec<RememberedCache> {
    let Some(path) = snapshot_path() else {
        return Vec::new();
    };
    load_from(&path, options, now_unix()).unwrap_or_default()
}

pub(super) fn save(options: &Options, report: &ScanReport) {
    let Some(path) = snapshot_path() else {
        return;
    };
    let _ = save_to(&path, options, report, now_unix());
}

fn snapshot_path() -> Option<PathBuf> {
    if env::var_os("CACHEFERRET_NO_CACHE").is_some() {
        return None;
    }
    if let Some(root) = env::var_os("XDG_CACHE_HOME").map(PathBuf::from)
        && root.is_absolute()
    {
        return Some(root.join("cacheferret/scan-v1.json"));
    }
    let home = env::var_os("HOME").map(PathBuf::from)?;
    #[cfg(target_os = "macos")]
    let root = home.join("Library/Caches");
    #[cfg(not(target_os = "macos"))]
    let root = home.join(".cache");
    Some(root.join("cacheferret/scan-v1.json"))
}

fn load_from(path: &Path, options: &Options, now: u64) -> io::Result<Vec<RememberedCache>> {
    let roots = canonical_roots(&options.roots);
    let mut entries = read_snapshot(path)?
        .entries
        .into_iter()
        .filter(|entry| {
            entry.path.is_absolute()
                && now.saturating_sub(entry.observed_unix) <= MAX_AGE_SECONDS
                && option_covers(options, &roots, entry)
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        right
            .bytes
            .cmp(&left.bytes)
            .then_with(|| left.path.cmp(&right.path))
    });
    let mut seen = std::collections::HashSet::new();
    entries.retain(|entry| seen.insert(entry.path.clone()));
    entries.truncate(MAX_ENTRIES);
    Ok(entries)
}

fn save_to(path: &Path, options: &Options, report: &ScanReport, now: u64) -> io::Result<()> {
    let roots = canonical_roots(&options.roots);
    let mut entries: HashMap<PathBuf, RememberedCache> = read_snapshot(path)
        .map(|snapshot| snapshot.entries)
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| now.saturating_sub(entry.observed_unix) <= MAX_AGE_SECONDS)
        .filter(|entry| !option_covers(options, &roots, entry))
        .map(|entry| (entry.path.clone(), entry))
        .collect();

    for candidate in &report.candidates {
        let entry = remembered(candidate, now);
        entries.insert(entry.path.clone(), entry);
    }

    let mut entries = entries.into_values().collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        right
            .observed_unix
            .cmp(&left.observed_unix)
            .then_with(|| right.bytes.cmp(&left.bytes))
            .then_with(|| left.path.cmp(&right.path))
    });
    entries.truncate(MAX_ENTRIES);

    let mut snapshot = Snapshot {
        version: SNAPSHOT_VERSION,
        entries,
    };
    let mut payload = encode(&snapshot)?;
    while payload.len() as u64 > MAX_SNAPSHOT_BYTES && !snapshot.entries.is_empty() {
        if snapshot.entries.len() == 1 {
            snapshot.entries.clear();
        } else {
            snapshot.entries.truncate(snapshot.entries.len() * 3 / 4);
        }
        payload = encode(&snapshot)?;
    }
    if payload.len() as u64 > MAX_SNAPSHOT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "scan snapshot exceeds its size limit",
        ));
    }

    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "snapshot path has no parent")
    })?;
    fs::create_dir_all(parent)?;
    let parent_metadata = fs::symlink_metadata(parent)?;
    if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "snapshot directory is not a real directory",
        ));
    }
    set_private_directory_permissions(parent)?;
    let temporary = path.with_extension(format!("tmp-{}-{}", std::process::id(), now_unix_nanos()));
    let write_result = (|| {
        let mut file = private_file(&temporary)?;
        file.write_all(&payload)?;
        file.sync_all()
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    let _ = File::open(parent).and_then(|directory| directory.sync_all());
    Ok(())
}

fn read_snapshot(path: &Path) -> io::Result<Snapshot> {
    let file = File::open(path)?;
    if file.metadata()?.len() > MAX_SNAPSHOT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "scan snapshot exceeds its size limit",
        ));
    }
    let mut payload = Vec::new();
    file.take(MAX_SNAPSHOT_BYTES + 1)
        .read_to_end(&mut payload)?;
    if payload.len() as u64 > MAX_SNAPSHOT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "scan snapshot exceeds its size limit",
        ));
    }
    let snapshot: Snapshot = serde_json::from_slice(&payload)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if snapshot.version != SNAPSHOT_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported scan snapshot version",
        ));
    }
    Ok(snapshot)
}

fn encode(snapshot: &Snapshot) -> io::Result<Vec<u8>> {
    serde_json::to_vec(snapshot).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn remembered(candidate: &CacheCandidate, observed_unix: u64) -> RememberedCache {
    RememberedCache {
        kind: candidate.kind.clone(),
        ecosystem: candidate.ecosystem.clone(),
        scope: candidate.scope,
        path: candidate.path.clone(),
        bytes: candidate.bytes,
        allocated_bytes: candidate.allocated_bytes,
        modified_unix: candidate.modified_unix,
        cleanable: candidate.cleanable,
        observed_unix,
    }
}

fn option_covers(options: &Options, roots: &[PathBuf], entry: &RememberedCache) -> bool {
    scope_includes(options.scope, entry.scope)
        && (options.kinds.is_empty() || options.kinds.contains(&entry.kind))
        && (entry.scope == CacheScope::Global
            || roots
                .iter()
                .any(|root| entry.path != *root && entry.path.starts_with(root)))
}

fn scope_includes(filter: ScopeFilter, scope: CacheScope) -> bool {
    matches!(filter, ScopeFilter::All)
        || matches!((filter, scope), (ScopeFilter::Project, CacheScope::Project))
        || matches!((filter, scope), (ScopeFilter::Global, CacheScope::Global))
}

fn canonical_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    roots
        .iter()
        .map(|root| root.canonicalize().unwrap_or_else(|_| root.clone()))
        .collect()
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn now_unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

#[cfg(unix)]
fn private_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn private_file(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use cacheferret::{DiscoveryOptions, discover};
    use tempfile::tempdir;

    use super::*;

    fn options(root: &Path) -> Options {
        Options {
            roots: vec![root.to_path_buf()],
            scope: ScopeFilter::Project,
            kinds: Vec::new(),
        }
    }

    #[test]
    fn snapshot_round_trip_is_sorted_and_scoped() {
        let temp = tempdir().unwrap();
        let project = temp.path().join("demo");
        let target = project.join("target");
        fs::create_dir_all(&target).unwrap();
        fs::write(project.join("Cargo.toml"), "[package]\nname='demo'\n").unwrap();
        fs::write(target.join("artifact"), [7_u8; 64]).unwrap();
        let options = options(temp.path());
        let report = discover(&DiscoveryOptions {
            roots: options.roots.clone(),
            scope: options.scope,
            kinds: Vec::new(),
            protect_days: 0,
        })
        .unwrap();
        let path = temp.path().join("state/scan-v1.json");

        save_to(&path, &options, &report, 10_000).unwrap();
        let loaded = load_from(&path, &options, 10_001).unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].path, target.canonicalize().unwrap());
        assert_eq!(loaded[0].bytes, 64);
        assert!(fs::metadata(path).unwrap().len() <= MAX_SNAPSHOT_BYTES);
    }

    #[test]
    fn stale_or_out_of_scope_entries_are_not_loaded() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("scan-v1.json");
        let snapshot = Snapshot {
            version: SNAPSHOT_VERSION,
            entries: vec![RememberedCache {
                kind: "cargo-target".to_owned(),
                ecosystem: "rust".to_owned(),
                scope: CacheScope::Project,
                path: PathBuf::from("/elsewhere/target"),
                bytes: 10,
                allocated_bytes: 10,
                modified_unix: None,
                cleanable: true,
                observed_unix: 1,
            }],
        };
        fs::write(&path, encode(&snapshot).unwrap()).unwrap();

        assert!(
            load_from(&path, &options(temp.path()), MAX_AGE_SECONDS + 2)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn successful_scan_replaces_only_the_covered_snapshot_partition() {
        let temp = tempdir().unwrap();
        let first_root = temp.path().join("first");
        let second_root = temp.path().join("second");
        fs::create_dir_all(&first_root).unwrap();
        fs::create_dir_all(&second_root).unwrap();
        let first_root = first_root.canonicalize().unwrap();
        let second_root = second_root.canonicalize().unwrap();
        let path = temp.path().join("scan-v1.json");
        let entry = |root: &Path| RememberedCache {
            kind: "cargo-target".to_owned(),
            ecosystem: "rust".to_owned(),
            scope: CacheScope::Project,
            path: root.join("demo/target"),
            bytes: 10,
            allocated_bytes: 10,
            modified_unix: None,
            cleanable: true,
            observed_unix: 10_000,
        };
        fs::write(
            &path,
            encode(&Snapshot {
                version: SNAPSHOT_VERSION,
                entries: vec![entry(&first_root), entry(&second_root)],
            })
            .unwrap(),
        )
        .unwrap();

        save_to(
            &path,
            &options(&first_root),
            &ScanReport {
                candidates: Vec::new(),
                warnings: Vec::new(),
            },
            10_001,
        )
        .unwrap();

        let entries = read_snapshot(&path).unwrap().entries;
        assert_eq!(entries.len(), 1);
        assert!(entries[0].path.starts_with(second_root));
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_file_and_directory_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().unwrap();
        let path = temp.path().join("private/scan-v1.json");
        save_to(
            &path,
            &options(temp.path()),
            &ScanReport {
                candidates: Vec::new(),
                warnings: Vec::new(),
            },
            1,
        )
        .unwrap();

        assert_eq!(
            fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_refuses_a_symlinked_state_directory() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let real = temp.path().join("real");
        let linked = temp.path().join("linked");
        fs::create_dir(&real).unwrap();
        symlink(&real, &linked).unwrap();

        let error = save_to(
            &linked.join("scan-v1.json"),
            &options(temp.path()),
            &ScanReport {
                candidates: Vec::new(),
                warnings: Vec::new(),
            },
            1,
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(!real.join("scan-v1.json").exists());
    }
}
