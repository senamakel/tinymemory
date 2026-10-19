//! The queue's persisted shapes and the host storage port.

use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::background::BackgroundJob;
use crate::list_store::ItemClaim;

/// Most finished runs kept for the UI.
pub const MAX_HISTORY: usize = 50;

/// Attempts before a failing job is dropped.
pub const MAX_ATTEMPTS: u32 = 5;

/// One queued job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueuedJob {
    /// Stable id: a digest of the job and its root, so a duplicate merges.
    pub id: String,
    /// The layout root it runs under.
    pub root: String,
    /// The work.
    pub job: BackgroundJob,
    /// When it was first queued.
    pub queued_at: DateTime<Utc>,
    /// Failed runs so far.
    #[serde(default)]
    pub attempts: u32,
    /// The last failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// One finished run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobRun {
    /// The job's id.
    pub id: String,
    /// `build_beliefs` or `ingest_brain`.
    pub job: String,
    /// The layout root.
    pub root: String,
    /// When it ran.
    pub ran_at: DateTime<Utc>,
    /// `done`, `started`, `scheduled`, `skipped` or `failed`.
    pub outcome: String,
    /// Why it was skipped or failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Beliefs built, when the engine says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub built: Option<usize>,
    /// Items stored by an ingest.
    #[serde(default)]
    pub stored: usize,
}

/// The persisted queue.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QueueState {
    /// Jobs waiting to run.
    #[serde(default)]
    pub pending: Vec<QueuedJob>,
    /// The latest finished runs, newest first.
    #[serde(default)]
    pub history: Vec<JobRun>,
}

/// Why a queue run was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QueueError {
    /// [`Selection::One`] named a job that is not queued.
    #[error("no queued job `{0}`")]
    NoSuchJob(String),
    /// The storage port refused a claim or settlement.
    #[error("job queue storage unavailable: {0}")]
    StoreUnavailable(String),
}

/// Which jobs a run takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// Those at least the build delay old.
    Due,
    /// Every pending job, now.
    All,
    /// One job by id, now.
    One(String),
}

/// Where the host keeps the queue (a file, a document, a stream).
///
/// Synchronous host port for legacy snapshots and queue operations. Local
/// providers may use the default snapshot-based operations; multi-consumer
/// providers override enqueue, claim, and settlement with atomic item-level
/// operations and may reject snapshot replacement. A missing or unreadable
/// queue loads as empty; the in-memory provider persists snapshots directly.
pub trait QueueStore: Send + Sync {
    /// The saved queue, empty when there is none.
    fn load(&self) -> QueueState;
    /// Saves the queue.
    ///
    /// # Errors
    /// A message the queue logs; it never aborts a run.
    fn save(&self, state: &QueueState) -> Result<(), String>;

    /// Enqueue a single job. Driver-backed stores override this with an
    /// insert into that job's document; the default preserves older local
    /// stores.
    fn enqueue_item(&self, job: QueuedJob) -> Result<bool, String> {
        let mut state = self.load();
        if state.pending.iter().any(|known| known.id == job.id) {
            return Ok(false);
        }
        state.pending.push(job);
        self.save(&state)?;
        Ok(true)
    }

    /// Claim one matching job in FIFO order. `excluded` prevents one run call
    /// from retrying a failed job repeatedly. Durable stores override this
    /// with an atomic document claim and visibility lease.
    fn claim_next(
        &self,
        selection: &Selection,
        cutoff: chrono::DateTime<Utc>,
        excluded: &[String],
    ) -> Result<Option<(QueuedJob, ItemClaim<QueuedJob>)>, String> {
        let state = self.load();
        let Some((position, job)) = state.pending.into_iter().enumerate().find(|(_, job)| {
            !excluded.iter().any(|id| id == &job.id)
                && match selection {
                    Selection::Due => job.queued_at <= cutoff,
                    Selection::All => true,
                    Selection::One(id) => &job.id == id,
                }
        }) else {
            return Ok(None);
        };
        let token = ItemClaim {
            id: job.id.clone(),
            item: job.clone(),
            version: 0,
            epoch: 0,
            holder: String::new(),
            position,
            #[cfg(feature = "storage")]
            lease_key: String::new(),
        };
        Ok(Some((job, token)))
    }

    /// Settle one claimed job and add its history record. `replacement` keeps
    /// a failed job queued; `None` acknowledges it. Durable stores perform
    /// the job mutation and history insert through fenced item operations.
    fn settle(
        &self,
        claim: &ItemClaim<QueuedJob>,
        replacement: Option<QueuedJob>,
        run: JobRun,
        follow_ups: Vec<QueuedJob>,
    ) -> Result<bool, String> {
        let mut state = self.load();
        let Some(index) = state.pending.iter().position(|job| job.id == claim.id) else {
            return Ok(false);
        };
        if let Some(replacement) = replacement {
            state.pending[index] = replacement;
        } else {
            state.pending.remove(index);
        }
        state.history.insert(0, run);
        state.history.truncate(MAX_HISTORY);
        for follow_up in follow_ups {
            if !state.pending.iter().any(|known| known.id == follow_up.id) {
                state.pending.push(follow_up);
            }
        }
        self.save(&state)?;
        Ok(true)
    }
}

/// A [`QueueStore`] that keeps the queue in memory.
#[derive(Debug, Default)]
pub struct MemoryQueueStore(Mutex<QueueState>);

impl QueueStore for MemoryQueueStore {
    fn load(&self) -> QueueState {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn save(&self, state: &QueueState) -> Result<(), String> {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = state.clone();
        Ok(())
    }
}
