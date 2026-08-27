use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Output selected by the caller or TTY detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Text,
    Json,
}

/// Where a cache lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

/// A recognized cache that is waiting for its filesystem tree to be measured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredCache {
    pub kind: String,
    pub ecosystem: String,
    pub scope: CacheScope,
    pub path: PathBuf,
    pub cleanable: bool,
}

/// Incremental progress emitted while discovery and measurement overlap.
#[derive(Debug, Clone)]
pub enum DiscoveryEvent {
    Discovered(DiscoveredCache),
    Measured(CacheCandidate),
    Skipped { path: PathBuf },
    Warning(String),
}

/// A discovered cache directory.
#[derive(Debug, Clone, Serialize)]
pub struct CacheCandidate {
    pub kind: String,
    pub ecosystem: String,
    pub scope: CacheScope,
    pub path: PathBuf,
    pub bytes: u64,
    /// Filesystem blocks attributed to this tree. Shared clone blocks may make
    /// this larger than the space a deletion can actually reclaim.
    pub allocated_bytes: u64,
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
    pub allocated_bytes: u64,
    pub entries: u64,
    pub latest_modified_nanos: Option<u128>,
}

/// Complete scan result before output pagination.
#[derive(Debug, Clone)]
pub struct ScanReport {
    pub candidates: Vec<CacheCandidate>,
    pub warnings: Vec<String>,
}

/// Storage managed by a native tool rather than a filesystem path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NativeResource {
    pub provider: String,
    pub scope: String,
    pub kind: String,
    pub label: String,
    pub total_count: u64,
    pub active_count: u64,
    pub bytes: u64,
    pub reclaimable_bytes: u64,
    pub cleanable: bool,
}

/// A bounded, actionable problem reported by a native storage provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NativeDiagnostic {
    pub provider: String,
    pub kind: String,
    pub message: String,
    pub retryable: bool,
}

/// Complete read-only inspection result for a native storage provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NativeReport {
    pub provider: String,
    pub available: bool,
    pub resources: Vec<NativeResource>,
    pub diagnostics: Vec<NativeDiagnostic>,
}

/// Result of a Docker-owned build-cache preview or prune operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NativeCleanReport {
    pub provider: String,
    pub kind: String,
    pub changed: bool,
    pub dry_run: bool,
    pub confirmed: bool,
    pub before: NativeResource,
    pub after: Option<NativeResource>,
    pub reported_reclaimed_bytes: Option<u64>,
    pub estimated_removed_bytes: u64,
    pub diagnostics: Vec<NativeDiagnostic>,
}

impl NativeCleanReport {
    pub fn preview(before: NativeResource, dry_run: bool, confirmed: bool) -> Self {
        Self {
            provider: before.provider.clone(),
            kind: before.kind.clone(),
            changed: false,
            dry_run,
            confirmed,
            before,
            after: None,
            reported_reclaimed_bytes: None,
            estimated_removed_bytes: 0,
            diagnostics: Vec::new(),
        }
    }
}

impl NativeReport {
    pub fn total_bytes(&self) -> u64 {
        self.resources
            .iter()
            .fold(0, |total, resource| total.saturating_add(resource.bytes))
    }

    pub fn total_reclaimable_bytes(&self) -> u64 {
        self.resources.iter().fold(0, |total, resource| {
            total.saturating_add(resource.reclaimable_bytes)
        })
    }
}

impl ScanReport {
    pub fn total_bytes(&self) -> u64 {
        self.candidates
            .iter()
            .map(|candidate| candidate.bytes)
            .sum()
    }

    pub fn total_allocated_bytes(&self) -> u64 {
        self.candidates
            .iter()
            .map(|candidate| candidate.allocated_bytes)
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
    /// Legacy alias for `apparent_bytes_selected`.
    pub bytes_selected: u64,
    pub apparent_bytes_selected: u64,
    pub allocated_bytes_selected: u64,
    /// Legacy alias for `apparent_bytes_removed`.
    pub bytes_reclaimed_estimate: u64,
    pub apparent_bytes_removed: u64,
    pub allocated_bytes_removed_estimate: u64,
    /// Free-space changes observed around deletion, kept per filesystem to
    /// avoid double-counting storage pools shared by multiple volumes.
    pub filesystem_deltas: Vec<FilesystemSpaceDelta>,
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
    pub allocated_bytes: u64,
    pub network_restore: bool,
}

impl From<&CacheCandidate> for CleanTarget {
    fn from(candidate: &CacheCandidate) -> Self {
        Self {
            kind: candidate.kind.clone(),
            path: candidate.path.clone(),
            bytes: candidate.bytes,
            allocated_bytes: candidate.allocated_bytes,
            network_restore: candidate.network_restore,
        }
    }
}

/// An observed before/after free-space measurement for one filesystem.
#[derive(Debug, Clone, Serialize)]
pub struct FilesystemSpaceDelta {
    pub probe_path: PathBuf,
    pub free_bytes_before: u64,
    pub free_bytes_after: u64,
    pub delta_bytes: i64,
}

impl CleanReport {
    /// Return a directly observable net change when cleanup touched exactly one
    /// measured filesystem. Multiple deltas are intentionally not summed:
    /// APFS volumes can share the same underlying storage pool.
    pub fn observed_free_bytes_delta(&self) -> Option<i64> {
        (self.filesystem_deltas.len() == 1).then(|| self.filesystem_deltas[0].delta_bytes)
    }
}

/// Candidate refused during final validation or deletion.
#[derive(Debug, Clone, Serialize)]
pub struct SkippedPath {
    pub path: PathBuf,
    pub reason: String,
}
