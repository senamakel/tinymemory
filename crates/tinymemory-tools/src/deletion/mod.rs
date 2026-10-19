//! Deletions that must reach the engine, whenever it is reachable.
//!
//! Deleting a chat thread or removing a source asks the engine to delete for
//! good. When the engine cannot be reached, or a deletion fails, it is
//! recorded in a [`DeletionQueue`] (durable through a host [`ListStore`]) and
//! [`DeletionQueue::drain`] runs it again — on the next sign-in, and on every
//! background tick — until it succeeds.
//!
//! A deletion that fails is retried with exponential backoff
//! ([`Backoff`]): a tick skips it until it is due again, while a forced
//! drain (a sign-in) runs everything now.
//!
//! The crate links no executor and does no I/O: the host supplies the store
//! and the [`DeletionExecutor`] that does the forget.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use futures::lock::Mutex as AsyncMutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::list_store::ListStore;

/// One deletion still owed to the engine.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PendingDeletion {
    /// A deleted chat thread's conversation memory.
    Thread {
        /// The thread.
        thread_id: String,
    },
    /// Every item a removed memory source stored.
    Source {
        /// The source id.
        source_id: String,
    },
}

impl PendingDeletion {
    /// A short, content-free label for logs.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Thread { .. } => "thread",
            Self::Source { .. } => "source",
        }
    }
}

/// Exponential backoff between retries of one failing deletion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    /// The wait after the first failure.
    pub base: Duration,
    /// The longest wait.
    pub max: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            base: Duration::seconds(30),
            max: Duration::hours(1),
        }
    }
}

impl Backoff {
    /// The wait after `failures` consecutive failures (at least one).
    #[must_use]
    pub fn delay(&self, failures: u32) -> Duration {
        let shift = failures.saturating_sub(1).min(20);
        self.base
            .checked_mul(1 << shift)
            .map_or(self.max, |delay| delay.min(self.max))
    }
}

/// Runs one deletion against the engine.
#[async_trait]
pub trait DeletionExecutor: Send + Sync {
    /// Whether the engine can be reached at all. When not, a drain stops
    /// before running anything.
    fn is_reachable(&self) -> bool;

    /// Runs `deletion`, returning how many items went.
    ///
    /// # Errors
    /// A stable error code; the deletion stays queued.
    async fn run(&self, deletion: &PendingDeletion) -> Result<usize, String>;
}

/// When a drain runs a deletion that is backing off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drain {
    /// Only deletions that are due (a background tick).
    Due,
    /// Everything now (a sign-in).
    Force,
}

#[derive(Debug, Clone, Copy)]
struct Retry {
    failures: u32,
    not_before: DateTime<Utc>,
}

fn deletion_id(deletion: &PendingDeletion) -> String {
    let mut digest = Sha256::new();
    digest.update(serde_json::to_vec(deletion).unwrap_or_default());
    digest.finalize()[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A durable queue of [`PendingDeletion`]s.
pub struct DeletionQueue<S: ListStore<PendingDeletion>> {
    store: S,
    backoff: Backoff,
    /// Serialises read-modify-writes of the store.
    lock: Mutex<()>,
    /// Serialises drains, so two triggers never run one deletion twice.
    draining: AsyncMutex<()>,
    retries: Mutex<std::collections::HashMap<PendingDeletion, Retry>>,
}

impl<S: ListStore<PendingDeletion>> std::fmt::Debug for DeletionQueue<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeletionQueue")
            .field("backoff", &self.backoff)
            .finish_non_exhaustive()
    }
}

impl<S: ListStore<PendingDeletion>> DeletionQueue<S> {
    /// A queue persisted in `store`.
    #[must_use]
    pub fn new(store: S, backoff: Backoff) -> Self {
        Self {
            store,
            backoff,
            lock: Mutex::new(()),
            draining: AsyncMutex::new(()),
            retries: Mutex::new(HashMap::new()),
        }
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, ()> {
        self.lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn read(&self) -> Vec<PendingDeletion> {
        self.store.load().unwrap_or_else(|error| {
            log::warn!("[tinymemory:deletion] pending deletions unreadable: {error}");
            Vec::new()
        })
    }

    /// Records `deletion` to run on the next drain. One already queued is not
    /// queued twice.
    pub fn enqueue(&self, deletion: PendingDeletion) {
        let _guard = self.locked();
        let all = self.read();
        if all.contains(&deletion) {
            return;
        }
        log::info!(
            "[tinymemory:deletion] {} deletion queued until memory is reachable",
            deletion.label()
        );
        let id = deletion_id(&deletion);
        if let Err(error) = self.store.append(&id, deletion) {
            log::warn!("[tinymemory:deletion] writing pending deletion failed: {error}");
        }
    }

    /// Every deletion still owed, oldest first.
    #[must_use]
    pub fn pending(&self) -> Vec<PendingDeletion> {
        let _guard = self.locked();
        self.read()
    }

    fn retries(&self) -> std::sync::MutexGuard<'_, HashMap<PendingDeletion, Retry>> {
        self.retries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Runs queued deletions, settling each that succeeds. Stops at once when
    /// the engine is unreachable, and keeps a failing deletion (backing off
    /// its next [`Drain::Due`] run). Returns how many settled.
    pub async fn drain<E: DeletionExecutor>(
        &self,
        executor: &E,
        mode: Drain,
        now: DateTime<Utc>,
    ) -> usize {
        let _draining = self.draining.lock().await;
        let queued = self.pending();
        if queued.is_empty() || !executor.is_reachable() {
            if !queued.is_empty() {
                log::debug!(
                    "[tinymemory:deletion] memory off; {} pending deletions wait",
                    queued.len()
                );
            }
            return 0;
        }
        let mut settled = 0;
        let mut excluded: Vec<String> = if mode == Drain::Due {
            let retries = self.retries();
            queued
                .iter()
                .filter(|deletion| {
                    retries
                        .get(*deletion)
                        .is_some_and(|retry| retry.not_before > now)
                })
                .map(|deletion| deletion_id(deletion))
                .collect()
        } else {
            Vec::new()
        };
        for _ in 0..queued.len() {
            let eligible_at = if mode == Drain::Force {
                i64::MAX
            } else {
                now.timestamp_millis()
            };
            let claim = self
                .store
                .claim_oldest(&excluded, eligible_at, &deletion_id);
            let Ok(Some(claim)) = claim else { break };
            let deletion = claim.item.clone();
            excluded.push(claim.id.clone());
            match executor.run(&deletion).await {
                Ok(forgotten) => {
                    if !self.store.acknowledge(&claim).unwrap_or(false) {
                        continue;
                    }
                    self.retries().remove(&deletion);
                    settled += 1;
                    log::info!(
                        "[tinymemory:deletion] {} deletion completed forgotten={forgotten}",
                        deletion.label()
                    );
                }
                Err(code) => {
                    let mut retries = self.retries();
                    let failures = retries.get(&deletion).map_or(0, |r| r.failures) + 1;
                    let not_before = now + self.backoff.delay(failures);
                    retries.insert(
                        deletion.clone(),
                        Retry {
                            failures,
                            not_before,
                        },
                    );
                    let _ = self.store.retry(
                        &claim,
                        deletion.clone(),
                        Some(not_before.timestamp_millis()),
                    );
                    log::warn!(
                        "[tinymemory:deletion] {} deletion failed code={code} failures={failures}; kept",
                        deletion.label()
                    );
                }
            }
        }
        settled
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
