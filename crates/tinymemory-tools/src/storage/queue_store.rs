//! Driver-backed background job queue adapter.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tinystoragedrivers_core::{
    Clock, Counters, DocumentStoreExt, ErrorKind, Leases, Precondition, Query, Scope, Sort,
    StorageBackend, StorageError, Version, WriteOp, WriteOptions,
};

use crate::list_store::{ItemClaim, ListStore, StoreError};
use crate::queue::{JobRun, QueueState, QueueStore, QueuedJob, Selection};

use super::{DriverListStore, ImportReport, JOB_RUNS, JOBS, Stored, error, system_clock};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredRun {
    sequence: i64,
    run: JobRun,
}

/// A background job queue backed by one document per job and run.
pub struct DriverQueueStore {
    jobs: DriverListStore<QueuedJob>,
}

impl std::fmt::Debug for DriverQueueStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("DriverQueueStore").field(&self.jobs).finish()
    }
}

impl DriverQueueStore {
    /// Open the driver-backed job queue for one scope.
    pub fn new(
        backend: Arc<dyn StorageBackend>,
        scope: Scope,
        clock: Clock,
        lease: Duration,
    ) -> Result<Self, StoreError> {
        Ok(Self {
            jobs: DriverListStore::new(backend, scope, JOBS, clock, lease, |job| job.id.clone())?,
        })
    }

    /// Open with the standard system clock and a 30-second visibility lease.
    pub fn system_clock(
        backend: Arc<dyn StorageBackend>,
        scope: Scope,
    ) -> Result<Self, StoreError> {
        Self::new(
            backend,
            scope,
            Arc::new(system_clock),
            Duration::from_secs(30),
        )
    }

    /// Import a decoded legacy snapshot without changing its source file.
    /// Stable job ids are retained, duplicate imports are harmless, and
    /// conflicting target rows win and are reported.
    pub fn import_legacy(&self, state: &QueueState) -> Result<QueueImportReport, StoreError> {
        let jobs = self.jobs.import_items(
            state
                .pending
                .iter()
                .map(|job| (job.id.clone(), job.clone())),
        )?;
        let history = state.history.clone();
        let storage = self.jobs.storage()?;
        let blocking = self.jobs.blocking.clone();
        blocking
            .run(async move {
                let docs = storage.documents().clone();
                let mut report = ImportReport::default();
                for (index, run) in history.into_iter().enumerate() {
                    let body = serde_json::to_value(&run)?;
                    let digest = Sha256::digest(serde_json::to_vec(&(index, &run))?);
                    let suffix = digest[..8]
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>();
                    let id = format!("legacy-{index}-{suffix}");
                    match docs.get_as::<StoredRun>(JOB_RUNS, &id).await? {
                        Some(found) => {
                            if serde_json::to_value(found.doc.run)? == body {
                                report.identical += 1;
                            } else {
                                report.conflicts += 1;
                            }
                        }
                        None => {
                            let sequence = -i64::try_from(index).map_err(|_| {
                                StorageError::invalid_input("legacy order out of range")
                            })? - 1;
                            let value = StoredRun { sequence, run };
                            match docs
                                .put_as(JOB_RUNS, &id, &value, Precondition::Absent)
                                .await
                            {
                                Ok(_) => report.imported += 1,
                                Err(error) if error.kind() == ErrorKind::Conflict => {
                                    let Some(found) =
                                        docs.get_as::<StoredRun>(JOB_RUNS, &id).await?
                                    else {
                                        return Err(error);
                                    };
                                    if serde_json::to_value(found.doc.run)? == body {
                                        report.identical += 1;
                                    } else {
                                        report.conflicts += 1;
                                    }
                                }
                                Err(error) => return Err(error),
                            }
                        }
                    }
                }
                Ok::<_, StorageError>(report)
            })
            .map(|runs| QueueImportReport { jobs, runs })
            .map_err(error)?
            .map_err(error)
    }

    fn load_runs(&self) -> Result<Vec<JobRun>, StoreError> {
        let storage = self.jobs.storage()?;
        let blocking = self.jobs.blocking.clone();
        blocking
            .run(async move {
                let documents = storage.documents().clone();
                let mut query = Query::all().sort(Sort::desc("sequence")).limit(256);
                let mut runs = Vec::new();
                loop {
                    let page = documents.query_as::<StoredRun>(JOB_RUNS, &query).await?;
                    runs.extend(page.items.into_iter().map(|row| row.doc.run));
                    let Some(cursor) = page.next else { break };
                    query = query.after(cursor);
                }
                runs.truncate(crate::queue::MAX_HISTORY);
                Ok::<_, StorageError>(runs)
            })
            .map_err(error)?
            .map_err(error)
    }

    fn settle_claim(
        &self,
        claim: &ItemClaim<QueuedJob>,
        replacement: Option<QueuedJob>,
        run: JobRun,
        follow_ups: Vec<QueuedJob>,
    ) -> Result<bool, String> {
        let storage = self.jobs.storage().map_err(|err| err.to_string())?;
        let documents = storage.documents().clone();
        let leases = Leases::new(&documents).with_clock(self.jobs.clock.clone());
        let backend = Arc::clone(&self.jobs.backend);
        let scope = self.jobs.scope.clone();
        let collection = JOBS;
        let id = claim.id.clone();
        let lease_key = claim.lease_key.clone();
        let holder = claim.holder.clone();
        let epoch = claim.epoch;
        let job_version = Version(claim.version);
        let ttl_ms = self.jobs.lease_ms;
        let run_id = format!("{}-{epoch}", claim.id);
        let result = self.jobs.run(async move {
            let Some(lease) = leases
                .renew("tinymemory.queue", &lease_key, &holder, epoch, ttl_ms)
                .await?
            else {
                return Ok(false);
            };
            let fenced = backend.for_scope_fenced(&scope, &lease.fence(&scope))?;
            let docs = fenced.documents();
            let current = docs
                .get_as::<Stored<QueuedJob>>(collection, &id)
                .await?
                .ok_or_else(|| StorageError::conflict("claimed job disappeared"))?;
            if current.version != job_version {
                return Ok(false);
            }
            let sequence =
                i64::try_from(Counters::new(docs).next("tinymemory", JOB_RUNS).await?)
                    .map_err(|_| StorageError::invalid_input("history sequence out of range"))?;
            let run_doc = serde_json::to_value(StoredRun { sequence, run })?;
            let mut ops = vec![WriteOp::Put {
                collection: JOB_RUNS.to_owned(),
                id: run_id,
                doc: run_doc,
                precondition: Precondition::Absent,
                options: WriteOptions::default(),
            }];
            let history = docs
                .query_as::<StoredRun>(
                    JOB_RUNS,
                    &Query::all()
                        .sort(Sort::desc("sequence"))
                        .limit(crate::queue::MAX_HISTORY),
                )
                .await?;
            if history.items.len() == crate::queue::MAX_HISTORY {
                let oldest = history.items.last().ok_or_else(|| {
                    StorageError::backend("history page unexpectedly had no last row")
                })?;
                ops.push(WriteOp::Delete {
                    collection: JOB_RUNS.to_owned(),
                    id: oldest.id.clone(),
                    precondition: Precondition::Version(oldest.version),
                });
            }
            match replacement {
                Some(job) => {
                    let mut stored = current.doc;
                    stored.item = job;
                    stored.claimed_epoch = epoch;
                    ops.push(WriteOp::Put {
                        collection: collection.to_owned(),
                        id,
                        doc: serde_json::to_value(stored)?,
                        precondition: Precondition::Version(job_version),
                        options: WriteOptions::default(),
                    });
                }
                None => ops.push(WriteOp::Delete {
                    collection: collection.to_owned(),
                    id,
                    precondition: Precondition::Version(job_version),
                }),
            }
            let counters = Counters::new(docs);
            for job in follow_ups {
                if docs.get(collection, &job.id).await?.is_some() {
                    continue;
                }
                let sequence = i64::try_from(counters.next("tinymemory", collection).await?)
                    .map_err(|_| StorageError::invalid_input("queue sequence out of range"))?;
                ops.push(WriteOp::Put {
                    collection: collection.to_owned(),
                    id: job.id.clone(),
                    doc: serde_json::to_value(Stored {
                        sequence,
                        item: job,
                        claimed_epoch: 0,
                        not_before_ms: None,
                    })?,
                    precondition: Precondition::Absent,
                    options: WriteOptions::default(),
                });
            }
            docs.atomic_batch(ops).await?;
            let _ = leases
                .release("tinymemory.queue", &lease_key, &holder, epoch)
                .await?;
            Ok(true)
        });
        result
            .map_err(|err| err.to_string())?
            .map_err(|err| err.to_string())
    }
}

impl QueueStore for DriverQueueStore {
    fn load(&self) -> QueueState {
        let pending = self.jobs.load().unwrap_or_else(|_| Vec::new());
        let history = self.load_runs().unwrap_or_else(|_| Vec::new());
        QueueState { pending, history }
    }

    fn save(&self, _state: &QueueState) -> Result<(), String> {
        Err("snapshot replacement is unsupported for DriverQueueStore; use enqueue_item, claim_next/settle, or import_legacy".into())
    }

    fn enqueue_item(&self, job: QueuedJob) -> Result<bool, String> {
        self.jobs
            .append(&job.id, job)
            .map_err(|err| err.to_string())
    }

    fn claim_next(
        &self,
        selection: &Selection,
        cutoff: chrono::DateTime<chrono::Utc>,
        excluded: &[String],
    ) -> Result<Option<(QueuedJob, ItemClaim<QueuedJob>)>, String> {
        let selection = selection.clone();
        let claim = self
            .jobs
            .claim_matching(excluded, i64::MAX, move |job| match &selection {
                Selection::Due => job.queued_at <= cutoff,
                Selection::All => true,
                Selection::One(id) => &job.id == id,
            })
            .map_err(|err| err.to_string())?;
        Ok(claim.map(|claim| (claim.item.clone(), claim)))
    }

    fn settle(
        &self,
        claim: &ItemClaim<QueuedJob>,
        replacement: Option<QueuedJob>,
        run: JobRun,
        follow_ups: Vec<QueuedJob>,
    ) -> Result<bool, String> {
        self.settle_claim(claim, replacement, run, follow_ups)
    }
}
