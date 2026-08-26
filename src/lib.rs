//! CacheFerret finds and safely removes rebuildable developer caches.

#![recursion_limit = "256"]

mod catalog;
mod cleaner;
mod error;
#[cfg(any(target_os = "macos", test))]
mod macos;
mod model;
mod scanner;
pub mod schema;

pub use catalog::{catalog, default_roots};
pub use cleaner::clean_candidates;
pub use error::Error;
pub use model::{
    CacheCandidate, CacheScope, CatalogEntry, CleanReport, CleanTarget, DiscoveryOptions,
    FilesystemSpaceDelta, OutputFormat, ScanReport, ScopeFilter,
};
pub use scanner::{discover, refresh_candidate};

/// Render a byte count compactly for terminal output.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Render a signed byte delta, including an explicit sign for positive values.
pub fn format_signed_bytes(bytes: i64) -> String {
    if bytes >= 0 {
        format!("+{}", format_bytes(bytes as u64))
    } else {
        format!("-{}", format_bytes(bytes.unsigned_abs()))
    }
}
