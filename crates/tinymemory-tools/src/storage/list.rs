//! Driver-backed generic FIFO list adapter.

use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tinystoragedrivers_core::{
    Blocking, Clock, Counters, DocumentStore, DocumentStoreExt, ErrorKind, Filter, Leases,
    Precondition, Query, Scope, Sort, StorageBackend, StorageError,
};

use crate::list_store::{ItemClaim, ListStore, StoreError};

use super::{error, holder_id, system_clock};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Stored<T> {
    pub(super) sequence: i64,
    pub(super) item: T,
    #[serde(default)]
    pub(super) claimed_epoch: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) not_before_ms: Option<i64>,
}

/// A list-shaped queue backed by one TSD document per item.
///
/// The injected TSD clock controls lease acquisition and renewal in tests. The
/// default clock is the system clock; distributed deployments therefore need
/// reasonably synchronized clocks for expiration boundaries. A crashed
/// consumer's lease eventually expires and the job is delivered again, so
/// effects are at-least-once across crashes and a long operation may overlap
/// its retry after its visibility lease expires.
pub struct DriverListStore<T> {
    pub(super) backend: Arc<dyn StorageBackend>,
    pub(super) scope: Scope,
    pub(super) blocking: Blocking,
    pub(super) clock: Clock,
    holder: String,
    collection: &'static str,
    pub(super) lease_ms: i64,
    id_of: Arc<dyn Fn(&T) -> String + Send + Sync>,
    _item: PhantomData<fn() -> T>,
}

impl<T> std::fmt::Debug for DriverListStore<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DriverListStore")
            .field("scope", &self.scope)
            .field("collection", &self.collection)
            .finish_non_exhaustive()
    }
}

impl<T> DriverListStore<T>
where
    T: Clone + Serialize + DeserializeOwned + Send + Sync + 'static,
{
    /// Open a list store over a host-owned backend and scope.
    ///
    /// `id_of` must return a stable, unique id for this occurrence. In
    /// particular, callers importing a FIFO file must supply occurrence ids
    /// so identical values at distinct positions remain distinct.
    pub fn new(
        backend: Arc<dyn StorageBackend>,
        scope: Scope,
        collection: &'static str,
        clock: Clock,
        lease: Duration,
        id_of: impl Fn(&T) -> String + Send + Sync + 'static,
    ) -> Result<Self, StoreError> {
        let lease_ms = i64::try_from(lease.as_millis())
            .ok()
            .filter(|millis| *millis > 0)
            .ok_or_else(|| StoreError("lease duration must be positive".into()))?;
        let blocking = Blocking::new().map_err(error)?;
        Ok(Self {
            backend,
            scope,
            blocking,
            clock,
            holder: holder_id(),
            collection,
            lease_ms,
            id_of: Arc::new(id_of),
            _item: PhantomData,
        })
    }

    /// Use the TSD system clock and a 30-second visibility lease.
    pub fn system_clock(
        backend: Arc<dyn StorageBackend>,
        scope: Scope,
        collection: &'static str,
        id_of: impl Fn(&T) -> String + Send + Sync + 'static,
    ) -> Result<Self, StoreError> {
        Self::new(
            backend,
            scope,
            collection,
            Arc::new(system_clock),
            Duration::from_secs(30),
            id_of,
        )
    }

    pub(super) fn storage(&self) -> Result<tinystoragedrivers_core::ScopedStorage, StoreError> {
        self.backend.for_scope(&self.scope).map_err(error)
    }

    pub(super) fn run<R: Send + 'static>(
        &self,
        future: impl std::future::Future<Output = R> + Send + 'static,
    ) -> Result<R, StoreError> {
        self.blocking.run(future).map_err(error)
    }

    fn lease_key(&self, id: &str) -> String {
        format!("{}:{id}", self.collection)
    }

    fn leases(&self) -> Result<Leases, StoreError> {
        let documents = self.storage()?.documents().clone();
        Ok(Leases::new(&documents).with_clock(self.clock.clone()))
    }

    async fn all_documents(
        documents: Arc<dyn DocumentStore>,
        collection: &'static str,
    ) -> tinystoragedrivers_core::Result<Vec<Versioned<Stored<T>>>> {
        let mut query = Query::all().sort(Sort::asc("sequence")).limit(256);
        let mut items = Vec::new();
        loop {
            let page = documents.query_as::<Stored<T>>(collection, &query).await?;
            items.extend(page.items);
            let Some(cursor) = page.next else { break };
            query = query.after(cursor);
        }
        Ok(items)
    }

    fn append_item(&self, id: String, item: T) -> Result<bool, StoreError> {
        let documents = self.storage()?.documents().clone();
        let counters = Counters::new(&documents);
        let collection = self.collection;
        let owner = self.collection.to_owned();
        self.run(async move {
            if documents.get(collection, &id).await?.is_some() {
                return Ok(false);
            }
            let sequence = i64::try_from(counters.next("tinymemory", &owner).await?)
                .map_err(|_| StorageError::invalid_input("queue sequence out of range"))?;
            documents
                .put_as(
                    collection,
                    &id,
                    &Stored {
                        sequence,
                        item,
                        claimed_epoch: 0,
                        not_before_ms: None,
                    },
                    Precondition::Absent,
                )
                .await?;
            Ok(true)
        })
        .map_err(error)?
        .map_err(error)
    }

    /// Import decoded legacy items without modifying their source. Stable ids
    /// are supplied by the reader, and imported rows sort before newer native
    /// appends. Existing target rows are kept and conflicts are counted.
    pub fn import_items(
        &self,
        entries: impl IntoIterator<Item = (String, T)>,
    ) -> Result<ImportReport, StoreError> {
        let entries: Vec<_> = entries.into_iter().collect();
        let len = i64::try_from(entries.len())
            .map_err(|_| StoreError("legacy queue is too large".into()))?;
        let storage = self.storage()?;
        let blocking = self.blocking.clone();
        let collection = self.collection;
        blocking
            .run(async move {
                let documents = storage.documents().clone();
                let mut report = ImportReport::default();
                for (index, (id, item)) in entries.into_iter().enumerate() {
                    match documents.get_as::<Stored<T>>(collection, &id).await? {
                        Some(existing) => {
                            if serde_json::to_value(existing.doc.item)?
                                == serde_json::to_value(item)?
                            {
                                report.identical += 1;
                            } else {
                                report.conflicts += 1;
                            }
                        }
                        None => {
                            let sequence = i64::try_from(index).map_err(|_| {
                                StorageError::invalid_input("legacy order out of range")
                            })? - len;
                            documents
                                .put_as(
                                    collection,
                                    &id,
                                    &Stored {
                                        sequence,
                                        item,
                                        claimed_epoch: 0,
                                        not_before_ms: None,
                                    },
                                    Precondition::Absent,
                                )
                                .await?;
                            report.imported += 1;
                        }
                    }
                }
                Ok::<_, StorageError>(report)
            })
            .map_err(error)?
            .map_err(error)
    }

    fn claim_item(
        &self,
        excluded: Vec<String>,
        eligible_at_ms: i64,
        eligible: impl Fn(&T) -> bool + Send + Sync + 'static,
    ) -> Result<Option<ItemClaim<T>>, StoreError> {
        self.claim_item_with_hook(excluded, eligible_at_ms, eligible, Arc::new(|_, _| {}))
    }

    fn claim_item_with_hook(
        &self,
        excluded: Vec<String>,
        eligible_at_ms: i64,
        eligible: impl Fn(&T) -> bool + Send + Sync + 'static,
        hook: Arc<dyn Fn(&str, ClaimPoint) + Send + Sync>,
    ) -> Result<Option<ItemClaim<T>>, StoreError> {
        let documents = self.storage()?.documents().clone();
        let leases = Leases::new(&documents).with_clock(self.clock.clone());
        let backend = Arc::clone(&self.backend);
        let scope = self.scope.clone();
        let collection = self.collection;
        let holder = self.holder.clone();
        let lease_ms = self.lease_ms;
        let candidates = Self::all_documents(Arc::clone(&documents), collection);
        self.run(async move {
            let candidates = candidates.await?;
            for candidate in candidates {
                if excluded.iter().any(|id| id == &candidate.id)
                    || candidate
                        .doc
                        .not_before_ms
                        .is_some_and(|deadline| deadline > eligible_at_ms)
                    || !eligible(&candidate.doc.item)
                {
                    continue;
                }
                hook(&candidate.id, ClaimPoint::CandidateRead);
                let lease_key = format!("{collection}:{}", candidate.id);
                let Some(lease) = leases
                    .acquire("tinymemory.queue", &lease_key, &holder, lease_ms)
                    .await?
                else {
                    continue;
                };
                hook(&candidate.id, ClaimPoint::LeaseAcquired);
                // The candidate query is only a hint. Another consumer may
                // have acknowledged, removed, or retried this item before we
                // acquired its lease, so eligibility and the observed state
                // must be refreshed under ownership.
                let fenced = match backend.for_scope_fenced(&scope, &lease.fence(&scope)) {
                    Ok(fenced) => fenced,
                    Err(error) if error.kind() == ErrorKind::Fenced => continue,
                    Err(error) => return Err(error),
                };
                let documents = fenced.documents();
                let Some(current) = documents
                    .get_as::<Stored<T>>(collection, &candidate.id)
                    .await?
                else {
                    let _ = leases
                        .release("tinymemory.queue", &lease_key, &holder, lease.epoch)
                        .await?;
                    continue;
                };
                if excluded.iter().any(|id| id == &current.id)
                    || current
                        .doc
                        .not_before_ms
                        .is_some_and(|deadline| deadline > eligible_at_ms)
                    || !eligible(&current.doc.item)
                {
                    let _ = leases
                        .release("tinymemory.queue", &lease_key, &holder, lease.epoch)
                        .await?;
                    continue;
                }
                let patch = json!({"claimed_epoch": lease.epoch});
                let not_before_filter = match current.doc.not_before_ms {
                    Some(deadline) => Filter::eq("not_before_ms", deadline),
                    None => Filter::exists("not_before_ms", false),
                };
                let item = serde_json::to_value(&current.doc.item)?;
                let claimed = match documents
                    .claim(
                        collection,
                        &Filter::eq("_id", current.id.clone())
                            .and(Filter::eq("sequence", current.doc.sequence))
                            .and(Filter::eq("item", item))
                            .and(not_before_filter),
                        &[],
                        &patch,
                    )
                    .await
                {
                    Ok(claimed) => claimed,
                    Err(error) if error.kind() == ErrorKind::Fenced => return Ok(None),
                    Err(error) => return Err(error),
                };
                let Some(claimed) = claimed else {
                    let _ = leases
                        .release("tinymemory.queue", &lease_key, &holder, lease.epoch)
                        .await?;
                    continue;
                };
                let stored: Stored<T> = serde_json::from_value(claimed.doc).map_err(|error| {
                    StorageError::serialization(format!("queue item decode failed: {error}"))
                })?;
                return Ok(Some(ItemClaim {
                    id: claimed.id,
                    item: stored.item,
                    version: claimed.version.0,
                    epoch: lease.epoch,
                    holder,
                    position: 0,
                    lease_key,
                }));
            }
            Ok(None)
        })
        .map_err(error)?
        .map_err(error)
    }

    /// Claim the first stored item satisfying a caller's typed eligibility
    /// rule. The predicate only filters the read candidates; item ownership
    /// is still acquired through TSD leases and an atomic document claim.
    pub fn claim_matching(
        &self,
        excluded: &[String],
        eligible_at_ms: i64,
        eligible: impl Fn(&T) -> bool + Send + Sync + 'static,
    ) -> Result<Option<ItemClaim<T>>, StoreError> {
        self.claim_item(excluded.to_vec(), eligible_at_ms, eligible)
    }

    fn finish_item(
        &self,
        claim: &ItemClaim<T>,
        replacement: Option<(T, Option<i64>)>,
    ) -> Result<bool, StoreError> {
        let documents = self.storage()?.documents().clone();
        let leases = Leases::new(&documents).with_clock(self.clock.clone());
        let backend = Arc::clone(&self.backend);
        let scope = self.scope.clone();
        let holder = claim.holder.clone();
        let lease_key = claim.lease_key.clone();
        let id = claim.id.clone();
        let epoch = claim.epoch;
        let version = Version(claim.version);
        let ttl = self.lease_ms;
        let collection = self.collection;
        let replacement = replacement.map(|(item, not_before_ms)| (item, not_before_ms));
        self.run(async move {
            let Some(lease) = leases
                .renew("tinymemory.queue", &lease_key, &holder, epoch, ttl)
                .await?
            else {
                return Ok(false);
            };
            let fenced = backend.for_scope_fenced(&scope, &lease.fence(&scope))?;
            let documents = fenced.documents();
            let result = match replacement {
                Some((item, not_before_ms)) => {
                    let Some(mut current) = documents.get_as::<Stored<T>>(collection, &id).await?
                    else {
                        return Ok(false);
                    };
                    if current.version != version {
                        return Ok(false);
                    }
                    current.doc.item = item;
                    current.doc.not_before_ms = not_before_ms;
                    current.doc.claimed_epoch = epoch;
                    documents
                        .put_as(
                            collection,
                            &id,
                            &current.doc,
                            Precondition::Version(version),
                        )
                        .await?;
                    true
                }
                None => {
                    documents
                        .delete(collection, &id, Precondition::Version(version))
                        .await?
                }
            };
            let _ = leases
                .release("tinymemory.queue", &lease_key, &holder, epoch)
                .await?;
            Ok(result)
        })
        .map_err(error)?
        .map_err(error)
    }
}

#[derive(Clone, Copy)]
enum ClaimPoint {
    CandidateRead,
    LeaseAcquired,
}

impl<T> ListStore<T> for DriverListStore<T>
where
    T: Clone + Serialize + DeserializeOwned + Send + Sync + 'static,
{
    fn load(&self) -> Result<Vec<T>, StoreError> {
        let documents = self.storage()?.documents().clone();
        let collection = self.collection;
        self.run(async move {
            Ok(Self::all_documents(documents, collection)
                .await?
                .into_iter()
                .map(|row| row.doc.item)
                .collect())
        })
        .map_err(error)?
        .map_err(error)
    }

    fn save(&self, entries: &[T]) -> Result<(), StoreError> {
        // Compatibility snapshot importer. Live consumers use append/claim/
        // acknowledge/retry below and never replace the whole collection.
        let mut used = Vec::with_capacity(entries.len());
        for item in entries {
            let id = (self.id_of)(item);
            if used.iter().any(|known: &String| known == &id) {
                return Err(StoreError("list item ids must be unique".into()));
            }
            used.push(id);
        }
        let existing = self.load()?;
        let existing_ids: Vec<_> = existing.iter().map(|item| (self.id_of)(item)).collect();
        for id in existing_ids {
            if !used.contains(&id) {
                let storage = self.storage()?;
                let blocking = self.blocking.clone();
                let collection = self.collection;
                blocking
                    .run(async move {
                        let docs = storage.documents();
                        if let Some(row) = docs.get(collection, &id).await? {
                            docs.delete(collection, &id, Precondition::Version(row.version))
                                .await?;
                        }
                        Ok::<_, StorageError>(())
                    })
                    .map_err(error)?
                    .map_err(error)?;
            }
        }
        for item in entries {
            let id = (self.id_of)(item);
            let _ = self.append_item(id, item.clone())?;
        }
        Ok(())
    }

    fn append(&self, id: &str, item: T) -> Result<bool, StoreError> {
        self.append_item(id.to_owned(), item)
    }

    fn claim_oldest(
        &self,
        excluded: &[String],
        eligible_at_ms: i64,
        _id_of: &dyn Fn(&T) -> String,
    ) -> Result<Option<ItemClaim<T>>, StoreError> {
        self.claim_item(excluded.to_vec(), eligible_at_ms, |_| true)
    }

    fn acknowledge(&self, claim: &ItemClaim<T>) -> Result<bool, StoreError> {
        self.finish_item(claim, None)
    }

    fn retry(
        &self,
        claim: &ItemClaim<T>,
        item: T,
        not_before_ms: Option<i64>,
    ) -> Result<bool, StoreError> {
        self.finish_item(claim, Some((item, not_before_ms)))
    }

    fn remove(&self, id: &str) -> Result<bool, StoreError> {
        let storage = self.storage()?;
        let blocking = self.blocking.clone();
        let collection = self.collection;
        let id = id.to_owned();
        blocking
            .run(async move {
                let docs = storage.documents();
                let Some(row) = docs.get(collection, &id).await? else {
                    return Ok(false);
                };
                docs.delete(collection, &id, Precondition::Version(row.version))
                    .await
            })
            .map_err(error)?
            .map_err(error)
    }

    fn remove_matching(&self, predicate: &dyn Fn(&T) -> bool) -> Result<usize, StoreError> {
        let items = self
            .load()?
            .into_iter()
            .filter(|item| predicate(item))
            .map(|item| (self.id_of)(&item))
            .collect::<Vec<_>>();
        let storage = self.storage()?;
        let blocking = self.blocking.clone();
        let collection = self.collection;
        blocking
            .run(async move {
                let docs = storage.documents();
                let mut removed = 0;
                for id in items {
                    if let Some(row) = docs.get(collection, &id).await?
                        && docs
                            .delete(collection, &id, Precondition::Version(row.version))
                            .await?
                    {
                        removed += 1;
                    }
                }
                Ok::<_, StorageError>(removed)
            })
            .map_err(error)?
            .map_err(error)
    }
}

