//! Driver-backed, per-item queue stores.
//!
//! Each durable queue item is a versioned TSD document. Atomic claims,
//! visibility leases and fenced settlement coordinate independent consumers.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::list_store::StoreError;
use tinystoragedrivers_core::{Owner, ScopeClass, StorageError, schema::Schema};

mod list;
mod queue_store;

#[cfg(all(test, feature = "storage"))]
#[path = "storage_tests.rs"]
mod tests;

pub use list::DriverListStore;
pub(super) use list::Stored;
pub use queue_store::{DriverQueueStore, QueueImportReport};

/// Queued background jobs, one document per job.
pub const JOBS: &str = "tinymemory.jobs";
/// Job execution history, one document per run.
pub const JOB_RUNS: &str = "tinymemory.job_runs";
/// Queued tool writes, one document per write.
pub const TOOL_WRITES: &str = "tinymemory.tool_writes";
/// Queued deletions, one document per deletion occurrence.
pub const DELETIONS: &str = "tinymemory.deletions";

/// TinyMemory queue collections. TSD adds its lease and counter collections.
#[must_use]
pub fn storage_schema() -> Result<Schema, StorageError> {
    Ok(Schema::new(Owner::new("tinymemory")?)
        .collection(JOBS, 1, ScopeClass::Tenant)
        .collection(JOB_RUNS, 1, ScopeClass::Tenant)
        .collection(TOOL_WRITES, 1, ScopeClass::Tenant)
        .collection(DELETIONS, 1, ScopeClass::Tenant))
}

/// Counts from an idempotent import that leaves its source unchanged.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// Source items inserted into the target.
    pub imported: usize,
    /// Existing target items with identical contents.
    pub identical: usize,
    /// Existing target items with different contents; target rows won.
    pub conflicts: usize,
}

/// Per-family outcomes from importing a legacy whole-state job snapshot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueueImportReport {
    /// Pending job outcomes.
    pub jobs: ImportReport,
    /// History row outcomes.
    pub runs: ImportReport,
}

static HOLDERS: AtomicU64 = AtomicU64::new(1);

fn holder_id() -> String {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    format!(
        "{}-{nonce}-{}",
        std::process::id(),
        HOLDERS.fetch_add(1, Ordering::Relaxed)
    )
}

fn system_clock() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

pub(super) fn error(error: impl std::fmt::Display) -> StoreError {
    StoreError(error.to_string())
}
