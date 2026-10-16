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

use crate::process::{self, RunError};
use crate::scanner::{identity, measure_tree};
use crate::{CacheCandidate, CacheScope, DiscoveredCache, DiscoveryEvent, rustup};

/// How the last-use time of a measured tree is derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgeSignal {
    /// Newest access or modification time of any regular file.
    FileUse,
}

/// Where the size and age of an entry come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Sizing {
    /// Walk the directory tree. Entries smaller than `minimum_bytes` hold
    /// nothing worth listing.
    Tree { age: AgeSignal, minimum_bytes: u64 },
}

/// A native entry that is recognized but not yet measured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Planned {
    pub kind: &'static str,
    pub ecosystem: &'static str,
    pub path: PathBuf,
    pub label: Option<String>,
    pub network_restore: bool,
    pub sizing: Sizing,
}

/// The identifier a native command is invoked with, taken from a fresh
/// inspection rather than from the candidate.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Target {
    Toolchain(String),
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
    kind == rustup::KIND
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
    (planned, warnings)
}

pub(crate) fn discovered(planned: &Planned) -> DiscoveredCache {
    DiscoveredCache {
        kind: planned.kind.to_owned(),
        ecosystem: planned.ecosystem.to_owned(),
        scope: CacheScope::Global,
        path: planned.path.clone(),
        cleanable: true,
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
    let Sizing::Tree { age, minimum_bytes } = planned.sizing;
    let metadata = fs::symlink_metadata(&planned.path).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return None;
    }
    let measurement = measure_tree(&planned.path);
    if measurement.bytes < minimum_bytes {
        return None;
    }
    let (bytes, allocated_bytes) = (measurement.bytes, measurement.allocated_bytes);
    let last_used_unix = match age {
        AgeSignal::FileUse => newest_file_use_unix(&planned.path),
    };
    let (identity, fingerprint) = (identity(&metadata), measurement.fingerprint);
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
        cleanable: true,
        identity,
        tree_fingerprint: fingerprint,
    })
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

/// Inspect a candidate with its native tool and return what must be used to
/// act on it. Fails when the entry is gone, is in use, or cannot be verified.
pub(crate) fn resolve(candidate: &CacheCandidate) -> Result<Resolved, String> {
    match candidate.kind.as_str() {
        rustup::KIND => rustup::resolve(candidate),
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
pub(crate) fn remove(candidate: &CacheCandidate) -> io::Result<()> {
    let resolved = resolve(candidate).map_err(io::Error::other)?;
    match resolved.target {
        Target::Toolchain(name) => rustup::uninstall(&name, &candidate.path),
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
}
