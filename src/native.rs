//! Cache kinds whose storage is owned by a native tool.
//!
//! Rust toolchains, simulator devices and simulator runtimes are never removed
//! by deleting files: rustup and simctl track them in their own state. Each
//! kind is discovered by asking its owner, measured, and removed by the owner's
//! own command after a fresh inspection right before the mutation.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rayon::prelude::*;

use crate::model::{FileIdentity, TreeFingerprint};
use crate::process::{self, RunError};
use crate::scanner::{identity, measure_tree};
use crate::{CacheCandidate, CacheScope, DiscoveredCache, DiscoveryEvent, rustup, simulator};

/// How the last-use time of a measured tree is derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgeSignal {
    /// Newest access or modification time of any regular file.
    FileUse,
    /// Simulator device: newest modification time of a regular file under
    /// `data`. A device without installed apps is not listed at all.
    DeviceData,
}

/// Where the size and age of an entry come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Sizing {
    /// Walk the directory tree. Entries smaller than `minimum_bytes` hold
    /// nothing worth listing.
    Tree { age: AgeSignal, minimum_bytes: u64 },
    /// The owning tool reports the size and, when it knows it, the last use.
    Reported {
        bytes: u64,
        last_used_unix: Option<u64>,
    },
}

/// A native entry that is recognized but not yet measured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Planned {
    pub kind: &'static str,
    pub ecosystem: &'static str,
    pub path: PathBuf,
    pub label: Option<String>,
    pub network_restore: bool,
    /// False when the owning tool says the entry cannot be removed.
    pub cleanable: bool,
    pub sizing: Sizing,
}

/// What a successful removal achieved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Removal {
    /// The entry is gone and its space is released.
    Complete,
    /// The owning tool accepted the removal but still lists the entry, or its
    /// state could not be confirmed, so no space can be counted as freed yet.
    Deferred,
}

/// The identifier a native command is invoked with, taken from a fresh
/// inspection rather than from the candidate.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Target {
    Toolchain(String),
    Device(String),
    Runtime(String),
}

/// Result of inspecting a candidate natively right before acting on it.
#[derive(Debug)]
pub(crate) struct Resolved {
    pub target: Target,
    pub planned: Planned,
}

/// Why a native tool could not be consulted.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Unavailable {
    NotInstalled,
    Problem(String),
}

impl Unavailable {
    /// A scan warning, or `None` when the tool is simply not installed.
    pub fn warning(&self, tool: &str, consequence: &str) -> Option<String> {
        match self {
            Unavailable::NotInstalled => None,
            Unavailable::Problem(message) => Some(format!("{tool}: {message}; {consequence}")),
        }
    }

    pub fn message(&self, tool: &str) -> String {
        match self {
            Unavailable::NotInstalled => format!("{tool} is not installed or is not on PATH"),
            Unavailable::Problem(message) => message.clone(),
        }
    }
}

/// Run a native tool and require a successful exit.
pub(crate) fn run_tool(
    program: &str,
    args: &[&str],
    timeout: Duration,
    action: &str,
) -> Result<process::Output, Unavailable> {
    let output = process::run(program, args, timeout).map_err(|error| match error {
        RunError::NotFound => Unavailable::NotInstalled,
        other => Unavailable::Problem(other.describe(program, action)),
    })?;
    if output.success {
        Ok(output)
    } else {
        Err(Unavailable::Problem(format!(
            "{program} {action} failed: {}",
            output.failure_detail()
        )))
    }
}

pub(crate) fn handles(kind: &str) -> bool {
    matches!(
        kind,
        rustup::KIND | simulator::DEVICE_KIND | simulator::RUNTIME_KIND
    )
}

/// Ask each wanted native tool what it manages. Tools that are not installed
/// contribute nothing; tools that fail contribute a warning.
pub(crate) fn plan(wants: &dyn Fn(&str) -> bool) -> (Vec<Planned>, Vec<String>) {
    let mut planned = Vec::new();
    let mut warnings = Vec::new();
    let mut take = |(items, notes): (Vec<Planned>, Vec<String>)| {
        planned.extend(items);
        warnings.extend(notes);
    };
    if wants(rustup::KIND) {
        take(rustup::plan());
    }
    if wants(simulator::DEVICE_KIND) {
        take(simulator::plan_devices());
    }
    if wants(simulator::RUNTIME_KIND) {
        take(simulator::plan_runtimes());
    }
    (planned, warnings)
}

pub(crate) fn discovered(planned: &Planned) -> DiscoveredCache {
    DiscoveredCache {
        kind: planned.kind.to_owned(),
        ecosystem: planned.ecosystem.to_owned(),
        scope: CacheScope::Global,
        path: planned.path.clone(),
        cleanable: planned.cleanable,
    }
}

/// Discover and measure every wanted native entry.
pub(crate) fn discover(
    wants: &dyn Fn(&str) -> bool,
    protect_days: u64,
    now: u64,
) -> (Vec<CacheCandidate>, Vec<String>) {
    let (planned, warnings) = plan(wants);
    let candidates = planned
        .into_par_iter()
        .filter_map(|planned| measure(planned, protect_days, now))
        .collect();
    (candidates, warnings)
}

/// Discover and measure while reporting each entry as it is recognized and
/// again when it has been measured.
pub(crate) fn discover_with_progress(
    wants: &dyn Fn(&str) -> bool,
    protect_days: u64,
    now: u64,
    progress: &(dyn Fn(DiscoveryEvent) + Sync),
) -> (Vec<CacheCandidate>, Vec<String>) {
    let (planned, warnings) = plan(wants);
    for warning in &warnings {
        progress(DiscoveryEvent::Warning(warning.clone()));
    }
    for item in &planned {
        progress(DiscoveryEvent::Discovered(discovered(item)));
    }
    let candidates = planned
        .into_par_iter()
        .filter_map(|item| {
            let path = item.path.clone();
            let measured = measure(item, protect_days, now);
            match &measured {
                Some(candidate) => progress(DiscoveryEvent::Measured(candidate.clone())),
                None => progress(DiscoveryEvent::Skipped { path }),
            }
            measured
        })
        .collect();
    (candidates, warnings)
}

/// Turn a planned entry into a candidate, or `None` when it is gone, is not a
/// real directory where one is expected, or holds too little to list.
pub(crate) fn measure(planned: Planned, protect_days: u64, now: u64) -> Option<CacheCandidate> {
    let (bytes, allocated_bytes, last_used_unix, identity, fingerprint) = match planned.sizing {
        Sizing::Tree { age, minimum_bytes } => {
            let metadata = fs::symlink_metadata(&planned.path).ok()?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return None;
            }
            let measurement = measure_tree(&planned.path);
            if measurement.bytes < minimum_bytes {
                return None;
            }
            let last_used = match age {
                AgeSignal::FileUse => newest_file_use_unix(&planned.path),
                AgeSignal::DeviceData => {
                    if !has_installed_apps(&planned.path) {
                        return None;
                    }
                    newest_file_modified_unix(&planned.path.join("data"))
                }
            };
            (
                measurement.bytes,
                measurement.allocated_bytes,
                last_used,
                identity(&metadata),
                measurement.fingerprint,
            )
        }
        Sizing::Reported {
            bytes,
            last_used_unix,
        } => (
            bytes,
            bytes,
            last_used_unix,
            detached_identity(),
            TreeFingerprint {
                bytes,
                allocated_bytes: bytes,
                entries: 0,
                latest_modified_nanos: None,
            },
        ),
    };
    let age_days = last_used_unix.map(|used| now.saturating_sub(used) / 86_400);
    Some(CacheCandidate {
        kind: planned.kind.to_owned(),
        ecosystem: planned.ecosystem.to_owned(),
        scope: CacheScope::Global,
        anchor: planned
            .path
            .parent()
            .map_or_else(|| planned.path.clone(), Path::to_path_buf),
        path: planned.path,
        label: planned.label,
        bytes,
        allocated_bytes,
        modified_unix: last_used_unix,
        age_days,
        protected: age_days.is_none_or(|days| days < protect_days),
        network_restore: planned.network_restore,
        cleanable: planned.cleanable,
        identity,
        tree_fingerprint: fingerprint,
    })
}

/// Entries the owning tool reports without a directory of their own are
/// inspected natively, so their filesystem identity is never compared.
fn detached_identity() -> FileIdentity {
    #[cfg(unix)]
    {
        FileIdentity {
            device: 0,
            inode: 0,
        }
    }
    #[cfg(not(unix))]
    {
        FileIdentity {
            modified_unix: None,
        }
    }
}

fn unix_seconds(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

/// Newest access or modification time among regular files. Reading a file
/// counts as use, so a toolchain that is run but never modified stays recent.
fn newest_file_use_unix(root: &Path) -> Option<u64> {
    walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .flatten()
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| entry.metadata().ok())
        .flat_map(|metadata| [metadata.accessed().ok(), metadata.modified().ok()])
        .flatten()
        .filter_map(unix_seconds)
        .max()
}

/// Whether a simulator device has at least one installed app. A device without
/// one is a pristine Xcode destination that holds nothing to reclaim.
fn has_installed_apps(device: &Path) -> bool {
    fs::read_dir(device.join("data/Containers/Bundle/Application"))
        .ok()
        .and_then(|mut entries| entries.next())
        .is_some_and(|entry| entry.is_ok())
}

/// Newest modification time of any regular file under `root`; `None` when there
/// is none. Directory times are ignored because the system rewrites them
/// without the contents being used, so a tree without files reads as unknown
/// rather than as freshly used.
fn newest_file_modified_unix(root: &Path) -> Option<u64> {
    walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .flatten()
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| entry.metadata().ok())
        .filter_map(|metadata| metadata.modified().ok())
        .filter_map(unix_seconds)
        .max()
}

/// Inspect a candidate with its native tool and return what must be used to
/// act on it. Fails when the entry is gone, is in use, or cannot be verified.
pub(crate) fn resolve(candidate: &CacheCandidate) -> Result<Resolved, String> {
    match candidate.kind.as_str() {
        rustup::KIND => rustup::resolve(candidate),
        simulator::DEVICE_KIND => simulator::resolve_device(candidate),
        simulator::RUNTIME_KIND => simulator::resolve_runtime(candidate),
        kind => Err(format!("{kind} is not a native cache kind")),
    }
}

pub(crate) fn revalidate(candidate: &CacheCandidate) -> Result<(), String> {
    resolve(candidate).map(|_| ())
}

/// Re-inspect and remeasure a candidate.
pub(crate) fn refresh(
    candidate: &CacheCandidate,
    protect_days: u64,
    now: u64,
) -> Result<CacheCandidate, String> {
    let resolved = resolve(candidate)?;
    measure(resolved.planned, protect_days, now)
        .ok_or_else(|| "the entry no longer holds any data".to_owned())
}

/// Remove a candidate with its owner's own command, after inspecting it again
/// so the in-use check happens immediately before the mutation.
pub(crate) fn remove(candidate: &CacheCandidate) -> io::Result<Removal> {
    let resolved = resolve(candidate).map_err(io::Error::other)?;
    match resolved.target {
        Target::Toolchain(name) => {
            rustup::uninstall(&name, &candidate.path).map(|()| Removal::Complete)
        }
        Target::Device(udid) => {
            simulator::delete_device(&udid, &candidate.path).map(|()| Removal::Complete)
        }
        Target::Runtime(identifier) => simulator::delete_runtime(&identifier),
    }
    .map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_use_counts_reads_not_just_writes() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("bin");
        fs::write(&file, b"x").unwrap();
        let handle = fs::File::options().write(true).open(&file).unwrap();
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        handle
            .set_times(
                fs::FileTimes::new()
                    .set_modified(old)
                    .set_accessed(old + Duration::from_secs(86_400)),
            )
            .unwrap();

        assert_eq!(newest_file_use_unix(temp.path()), Some(1_086_400));
    }

    #[test]
    fn tree_entries_below_the_minimum_are_not_listed() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("file"), [0_u8; 16]).unwrap();
        let planned = |minimum_bytes| Planned {
            kind: rustup::KIND,
            ecosystem: "rust",
            path: temp.path().to_path_buf(),
            label: None,
            network_restore: true,
            cleanable: true,
            sizing: Sizing::Tree {
                age: AgeSignal::FileUse,
                minimum_bytes,
            },
        };

        assert!(measure(planned(17), 7, 0).is_none());
        let listed = measure(planned(16), 7, u64::MAX / 2).unwrap();
        assert_eq!(listed.bytes, 16);
        assert!(!listed.protected);
    }

    #[test]
    fn a_device_age_is_the_newest_regular_file_not_a_directory() {
        use std::time::{Duration, UNIX_EPOCH};
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        assert_eq!(newest_file_modified_unix(&data), None, "no tree");
        fs::create_dir_all(data.join("Containers/Data")).unwrap();
        assert_eq!(newest_file_modified_unix(&data), None, "no files");

        let file = data.join("Containers/Data/state");
        fs::write(&file, b"x").unwrap();
        fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_000))
            .unwrap();
        // The system touches directories without anyone using the device.
        for dir in [
            data.join("Containers/Data"),
            data.join("Containers"),
            data.clone(),
        ] {
            fs::File::open(dir)
                .unwrap()
                .set_modified(SystemTime::now())
                .unwrap();
        }
        assert_eq!(newest_file_modified_unix(&data), Some(1_700_000_000));
    }

    #[test]
    fn only_devices_with_an_installed_app_are_listed() {
        let temp = tempfile::tempdir().unwrap();
        let device = temp.path();
        assert!(!has_installed_apps(device), "no containers");
        let bundles = device.join("data/Containers/Bundle/Application");
        fs::create_dir_all(&bundles).unwrap();
        assert!(!has_installed_apps(device), "empty bundle directory");
        fs::create_dir(bundles.join("11111111-1111-4111-8111-111111111111")).unwrap();
        assert!(has_installed_apps(device));
    }

    #[test]
    fn reported_entries_use_the_reported_size_and_age() {
        let now = 2_000_000_000;
        let planned = |last_used_unix, cleanable| Planned {
            kind: simulator::RUNTIME_KIND,
            ecosystem: "xcode",
            path: PathBuf::from("/example/runtime.dmg"),
            label: Some("iOS 26.5 (23F77)".to_owned()),
            network_restore: true,
            cleanable,
            sizing: Sizing::Reported {
                bytes: 8_000_000_000,
                last_used_unix,
            },
        };

        let used = measure(planned(Some(now - 10 * 86_400), true), 7, now).unwrap();
        assert_eq!(used.bytes, 8_000_000_000);
        assert_eq!(used.allocated_bytes, 8_000_000_000);
        assert_eq!(used.age_days, Some(10));
        assert!(!used.protected);
        assert!(used.cleanable);
        assert_eq!(used.label.as_deref(), Some("iOS 26.5 (23F77)"));

        let unknown = measure(planned(None, false), 7, now).unwrap();
        assert_eq!(unknown.modified_unix, None);
        assert_eq!(unknown.age_days, None);
        assert!(unknown.protected, "an unknown age is protected by default");
        assert!(!unknown.cleanable);
    }
}
