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
    /// Store-specific opaque scope used for ack/nack.
    pub(crate) lease_key: String,
}

/// Where the host keeps one queue's entries (a file, a document, a stream),
/// in order, oldest first.
///
/// Synchronous: the queues are small and read-modify-written under their own
/// lock. `load` returns an empty list when nothing is saved; an unreadable
/// store is `Err` so a queue never overwrites entries it could not read. A
/// host that sets a corrupt file aside returns empty after doing so.
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
    fn append(&self, _id: &str, item: T) -> Result<bool, StoreError> {
        let mut entries = self.load()?;
        entries.push(item);
        self.save(&entries)?;
        Ok(true)
    }

    /// Claim the oldest available item, excluding ids already inspected by
    /// this drain and honoring its retry cutoff.
    fn claim_oldest(
        &self,
        _excluded: &[String],
        _eligible_at_ms: i64,
    ) -> Result<Option<ItemClaim<T>>, StoreError>
    where
        T: Clone,
    {
        let entries = self.load()?;
        let Some(item) = entries.into_iter().next() else {
            return Ok(None);
        };
        Ok(Some(ItemClaim {
            id: String::new(),
            item,
            version: 0,
            epoch: 0,
            holder: String::new(),
            lease_key: String::new(),
        }))
    }

    /// Acknowledge and remove a successfully processed item.
    fn acknowledge(&self, claim: &ItemClaim<T>) -> Result<bool, StoreError> {
        let mut entries = self.load()?;
        if entries.is_empty() {
            return Ok(false);
        }
        entries.remove(0);
        self.save(&entries)?;
        let _ = claim;
        Ok(true)
    }

    /// Retain an item after failure, recording an optional retry deadline.
    fn retry(&self, claim: &ItemClaim<T>, item: T, _not_before_ms: Option<i64>) -> Result<bool, StoreError> {
        let mut entries = self.load()?;
        if entries.is_empty() {
            return Ok(false);
        }
        entries[0] = item;
        self.save(&entries)?;
        let _ = claim;
        Ok(true)
    }

    /// Remove one item by its stable id, used for erasure fencing.
    fn remove(&self, _id: &str) -> Result<bool, StoreError> {
        Ok(false)
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
