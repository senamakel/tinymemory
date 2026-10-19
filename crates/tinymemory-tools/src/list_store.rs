//! The host port the durable queues ([`crate::pending`], [`crate::deletion`])
//! keep their entries in.

use std::sync::Mutex;

/// A durable store failed. The message is content-free; callers map it to
/// their own "queue unavailable" error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("queue store unavailable: {0}")]
pub struct StoreError(pub String);

/// One item returned while a consumer holds its visibility lease.
#[derive(Debug, Clone)]
pub struct ItemClaim<T> {
    /// The stable item id in the store.
    pub id: String,
    /// The item at the time it was claimed.
    pub item: T,
    /// Driver version used to fence settlement.
    pub version: u64,
    /// Lease epoch used to fence settlement.
    pub epoch: u64,
    /// Lease holder id.
    pub holder: String,
    /// Position in a legacy list snapshot. Driver-backed stores ignore it.
    pub(crate) position: usize,
    /// Store-specific opaque scope used for ack/nack.
    #[cfg(feature = "storage")]
    pub(crate) lease_key: String,
}

impl<T> ItemClaim<T> {
    /// Create a claim token for a custom queue provider.
    ///
    /// `version`, `epoch`, and `holder` are provider-owned fencing data; a
    /// provider should validate them when acknowledging or retrying the item.
    /// The legacy position and native TSD lease key are initialized privately.
    #[must_use]
    pub fn new(id: String, item: T, version: u64, epoch: u64, holder: String) -> Self {
        Self {
            id,
            item,
            version,
            epoch,
            holder,
            position: 0,
            #[cfg(feature = "storage")]
            lease_key: String::new(),
        }
    }
}

/// Where the host keeps one queue's entries (a file, a document, a stream),
/// in order, oldest first.
///
/// Synchronous host port for queue snapshots and per-item mutations. Local
/// file and memory providers may implement the default read-modify-write
/// methods; multi-consumer providers override append, claim, settlement, and
/// removal with atomic item operations. `load` returns an empty list when
/// nothing is saved; an unreadable store is `Err` so a queue never overwrites
/// entries it could not read. A host that sets a corrupt file aside returns
/// empty after doing so.
pub trait ListStore<T>: Send + Sync {
    /// The saved entries.
    ///
    /// # Errors
    /// The store could not be read.
    fn load(&self) -> Result<Vec<T>, StoreError>;
    /// Replaces the saved entries. An empty list clears the store.
    ///
    /// # Errors
    /// The store could not be written; the caller treats the write as not made.
    fn save(&self, entries: &[T]) -> Result<(), StoreError>;

    /// Append one item under a caller-supplied stable id.
    ///
    /// The default keeps compatibility with older local stores. Durable
    /// multi-consumer stores override this with an item-level insert.
    ///
    /// # Errors
    /// Returns the load or save error from the underlying store.
    fn append(&self, _id: &str, item: T) -> Result<bool, StoreError> {
        let mut entries = self.load()?;
        entries.push(item);
        self.save(&entries)?;
        Ok(true)
    }

    /// Claim the oldest available item, excluding ids already inspected by
    /// this drain and honoring its retry cutoff.
    ///
    /// # Errors
    /// Returns the load error from the underlying store.
    fn claim_oldest(
        &self,
        excluded: &[String],
        _eligible_at_ms: i64,
        id_of: &dyn Fn(&T) -> String,
    ) -> Result<Option<ItemClaim<T>>, StoreError>
    where
        T: Clone,
    {
        let entries = self.load()?;
        let Some((position, item)) = entries
            .into_iter()
            .enumerate()
            .find(|(_, item)| !excluded.contains(&id_of(item)))
        else {
            return Ok(None);
        };
        Ok(Some(ItemClaim {
            id: id_of(&item),
            item,
            version: 0,
            epoch: 0,
            holder: String::new(),
            position,
            #[cfg(feature = "storage")]
            lease_key: String::new(),
        }))
    }

    /// Acknowledge and remove a successfully processed item.
    ///
    /// # Errors
    /// Returns the load or save error from the underlying store.
    fn acknowledge(&self, claim: &ItemClaim<T>) -> Result<bool, StoreError> {
        let mut entries = self.load()?;
        if entries.get(claim.position).is_none() {
            return Ok(false);
        }
        entries.remove(claim.position);
        self.save(&entries)?;
        let _ = claim;
        Ok(true)
    }

    /// Retain an item after failure, recording an optional retry deadline.
    ///
    /// # Errors
    /// Returns the load or save error from the underlying store.
    fn retry(
        &self,
        claim: &ItemClaim<T>,
        item: T,
        _not_before_ms: Option<i64>,
    ) -> Result<bool, StoreError> {
        let mut entries = self.load()?;
        if entries.get(claim.position).is_none() {
            return Ok(false);
        }
        entries[claim.position] = item;
        self.save(&entries)?;
        let _ = claim;
        Ok(true)
    }

    /// Remove one item by its stable id, used for erasure fencing.
    ///
    /// # Errors
    /// Returns an error if removal could not be completed.
    fn remove(&self, _id: &str) -> Result<bool, StoreError> {
        Ok(false)
    }

    /// Remove items matched by an owner predicate. Driver stores should
    /// override `remove` for stable-id compare-and-delete semantics.
    ///
    /// # Errors
    /// Returns the load or save error from the underlying store.
    fn remove_matching(&self, predicate: &dyn Fn(&T) -> bool) -> Result<usize, StoreError> {
        let mut entries = self.load()?;
        let before = entries.len();
        entries.retain(|entry| !predicate(entry));
        let removed = before - entries.len();
        if removed > 0 {
            self.save(&entries)?;
        }
        Ok(removed)
    }
}

/// A [`ListStore`] that keeps entries in memory.
#[derive(Debug)]
pub struct MemoryListStore<T>(Mutex<Vec<T>>);

impl<T> Default for MemoryListStore<T> {
    fn default() -> Self {
        Self(Mutex::new(Vec::new()))
    }
}

impl<T: Clone + Send> ListStore<T> for MemoryListStore<T> {
    fn load(&self) -> Result<Vec<T>, StoreError> {
        Ok(self.0.lock().unwrap_or_else(|e| e.into_inner()).clone())
    }

    fn save(&self, entries: &[T]) -> Result<(), StoreError> {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = entries.to_vec();
        Ok(())
    }
}
