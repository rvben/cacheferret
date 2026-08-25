use std::path::PathBuf;

use serde::Serialize;

/// Output selected by the caller or TTY detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Text,
    Json,
}

/// Where a cache lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheScope {
    Project,
    Global,
}

/// Scope accepted by discovery commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeFilter {
    All,
    Project,
    Global,
}

impl ScopeFilter {
    pub(crate) fn includes(self, scope: CacheScope) -> bool {
        matches!(self, ScopeFilter::All)
            || matches!((self, scope), (ScopeFilter::Project, CacheScope::Project))
            || matches!((self, scope), (ScopeFilter::Global, CacheScope::Global))
    }
}

/// Common discovery controls.
#[derive(Debug, Clone)]
pub struct DiscoveryOptions {
    pub roots: Vec<PathBuf>,
    pub scope: ScopeFilter,
    pub kinds: Vec<String>,
    pub protect_days: u64,
}

/// A discovered cache directory.
#[derive(Debug, Clone, Serialize)]
pub struct CacheCandidate {
    pub kind: String,
    pub ecosystem: String,
    pub scope: CacheScope,
    pub path: PathBuf,
    pub bytes: u64,
    pub modified_unix: Option<u64>,
    pub age_days: Option<u64>,
    pub protected: bool,
    pub network_restore: bool,
    pub cleanable: bool,
    #[serde(skip)]
    pub(crate) anchor: PathBuf,
    #[serde(skip)]
    pub(crate) identity: FileIdentity,
    #[serde(skip)]
    pub(crate) tree_fingerprint: TreeFingerprint,
}

/// Filesystem identity captured during discovery and checked again before removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileIdentity {
    #[cfg(unix)]
    pub device: u64,
    #[cfg(unix)]
    pub inode: u64,
    #[cfg(not(unix))]
    pub modified_unix: Option<u64>,
}

/// Volatile-tree state captured during measurement and checked before cleanup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TreeFingerprint {
    pub bytes: u64,
    pub entries: u64,
    pub latest_modified_nanos: Option<u128>,
}

/// Complete scan result before output pagination.
#[derive(Debug, Clone)]
pub struct ScanReport {
    pub candidates: Vec<CacheCandidate>,
    pub warnings: Vec<String>,
}

impl ScanReport {
    pub fn total_bytes(&self) -> u64 {
        self.candidates
            .iter()
            .map(|candidate| candidate.bytes)
            .sum()
    }
}

/// One supported cache kind shown by `catalog`.
#[derive(Debug, Clone, Serialize)]
pub struct CatalogEntry {
    pub kind: &'static str,
    pub ecosystem: &'static str,
    pub scope: CacheScope,
    pub description: &'static str,
    pub network_restore: bool,
    pub cleanable: bool,
}

/// Result of an attempted clean batch.
#[derive(Debug, Clone, Serialize)]
pub struct CleanReport {
    pub changed: bool,
    pub dry_run: bool,
    pub confirmed: bool,
    pub selected: usize,
    pub cleaned: usize,
    pub skipped: usize,
    pub protected_skipped: usize,
    pub policy_skipped: usize,
    pub network_restore_selected: usize,
    pub bytes_selected: u64,
    pub bytes_reclaimed_estimate: u64,
    pub selected_targets: Vec<CleanTarget>,
    pub cleaned_paths: Vec<PathBuf>,
    pub skipped_paths: Vec<SkippedPath>,
}

/// A compact, reviewable description of a cleanup target.
#[derive(Debug, Clone, Serialize)]
pub struct CleanTarget {
    pub kind: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub network_restore: bool,
}

impl From<&CacheCandidate> for CleanTarget {
    fn from(candidate: &CacheCandidate) -> Self {
        Self {
            kind: candidate.kind.clone(),
            path: candidate.path.clone(),
            bytes: candidate.bytes,
            network_restore: candidate.network_restore,
        }
    }
}

/// Candidate refused during final validation or deletion.
#[derive(Debug, Clone, Serialize)]
pub struct SkippedPath {
    pub path: PathBuf,
    pub reason: String,
}
