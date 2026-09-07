//! The remaining optional families: [`MemoryGoals`], [`MemoryToolMemory`],
//! [`MemorySourceSink`], and [`MemoryMaintenance`].
//!
//! Goals and tool memory are small curated record sets the agent reads on
//! nearly every turn. The source sink is the seam the host's sync machinery
//! writes through. Maintenance is the seam the host's scheduler drives.
//!
//! ## The host keeps the loop; the driver runs one step
//!
//! [`MemorySourceSink`] receives already-fetched items — the host owns
//! credentials, OAuth, rate limits, and the schedule. [`MemoryMaintenance`]
//! exposes the operations the host's existing scheduler calls; no driver
//! installs a background task of its own. Both follow the same rule as the
//! engine's `queue::run_once`, and both are why a driver never needs to see
//! configuration or a keychain.

use async_trait::async_trait;

use crate::capabilities::Capability;
use crate::error::MemoryError;
use crate::goals::GoalsDoc;
use crate::provider::diagnosis::{DegradedCapabilities, Diagnosis};
use crate::provider::types::{
    BackfillTreesOutcome, BackfillTreesRequest, FlushOutcome, ForgetOutcome, ForgetSelector,
    IngestOutcome, MaintenanceReport, PurgeOutcome, QueueFailure, QueueStats, ResetOutcome,
    SourceItem, StoreStats,
};
use crate::tool_memory::ToolMemoryRule;
use crate::types::MemoryTaint;

/// The agent's long-term goals document.
#[async_trait]
pub trait MemoryGoals: Send + Sync {
    /// Read the current goals document.
    ///
    /// A driver with no goals yet returns an empty [`GoalsDoc`], not
    /// [`MemoryError::NotFound`] — "no goals" is a valid state, not a missing
    /// record.
    ///
    /// # Errors
    ///
    /// Backend failures only.
    async fn goals(&self) -> Result<GoalsDoc, MemoryError>;

    /// Replace the goals document wholesale.
    ///
    /// Whole-document replacement rather than per-item add/edit/delete because
    /// the validating mutation surface (PII and secret predicates) is **host**
    /// policy: the host parses, validates, mutates, and hands back the result.
    /// Exposing per-item mutation here would put that policy behind a trait a
    /// third-party driver implements, where it could be skipped.
    ///
    /// # Errors
    ///
    /// [`MemoryError::Invalid`] for a document the driver refuses (e.g. over
    /// its own item cap), otherwise backend failures.
    async fn set_goals(&self, goals: GoalsDoc) -> Result<(), MemoryError>;
}

/// Per-tool learned rules — durable guidance attached to a specific tool.
#[async_trait]
pub trait MemoryToolMemory: Send + Sync {
    /// Rules for one tool, highest priority first.
    ///
    /// # Errors
    ///
    /// Backend failures only; a tool with no rules yields an empty vector.
    async fn tool_rules(&self, tool_name: &str) -> Result<Vec<ToolMemoryRule>, MemoryError>;

    /// Upsert one rule, keyed by [`ToolMemoryRule::id`].
    ///
    /// # Errors
    ///
    /// [`MemoryError::Invalid`] for a malformed rule, otherwise backend
    /// failures.
    async fn put_tool_rule(&self, rule: ToolMemoryRule) -> Result<(), MemoryError>;

    /// Delete one rule, reporting whether it existed.
    ///
    /// Idempotent, like [`crate::provider::MemoryCore::forget`].
    ///
    /// # Errors
    ///
    /// Backend failures only.
    async fn delete_tool_rule(&self, tool_name: &str, rule_id: &str) -> Result<bool, MemoryError>;
}

/// The write seam for host-driven source sync.
#[async_trait]
pub trait MemorySourceSink: Send + Sync {
    /// Accept a batch of items the host fetched from one logical source.
    ///
    /// `taint` applies to the whole batch and is stamped by the host. Sync
    /// paths ingesting third-party content pass
    /// [`MemoryTaint::ExternalSync`]; the driver persists what it is given and
    /// never assigns provenance itself.
    ///
    /// `source_kind` is a wire string (`folder`, `composio`, …) rather than an
    /// enum because the set of source kinds is owned by the host's sync
    /// machinery and grows without a contract change.
    ///
    /// # Errors
    ///
    /// [`MemoryError::Invalid`] for a rejected batch, otherwise backend
    /// failures. Per-item outcomes are counted in [`IngestOutcome`].
    async fn accept_source_items(
        &self,
        source_id: &str,
        source_kind: &str,
        items: Vec<SourceItem>,
        taint: MemoryTaint,
    ) -> Result<IngestOutcome, MemoryError>;

    /// Drop everything the driver holds for one logical source, returning how
    /// many units were removed.
    ///
    /// This is the disconnect path: when a user removes a source, its content
    /// must leave memory. Idempotent — an unknown `source_id` returns `Ok(0)`.
    ///
    /// # Errors
    ///
    /// Backend failures only.
    async fn forget_source(&self, source_id: &str) -> Result<u64, MemoryError>;

    /// Remove whatever [`ForgetSelector`] names, and report what went with it.
    ///
    /// # How this differs from [`Self::forget_source`]
    ///
    /// [`Self::forget_source`] is the whole-source disconnect: one logical id,
    /// every kind it appears under, one number back. This is the selective
    /// path, and each of its arms is something that call cannot express — a
    /// single chunk, a kind-qualified source, a family of derived source ids
    /// under one prefix, everything one owner brought in. Widening
    /// `forget_source` to cover them would mean four `Option` arguments where
    /// at most one may ever be set, on a call that deletes.
    ///
    /// A driver implementing both must keep them consistent: a
    /// [`ForgetSelector::Source`] naming the only kind a source has must
    /// remove exactly what `forget_source` would.
    ///
    /// # Why the outcome is not a count
    ///
    /// Deleting chunks can strand the summary trees derived from them, and
    /// cleaning a stranded tree is work that happens with no chunk removed at
    /// all. [`ForgetOutcome`] keeps the two counts apart so a caller can tell
    /// "nothing matched" from "nothing was left but the summaries".
    ///
    /// # Errors
    ///
    /// [`MemoryError::Unsupported`] from a driver that implements this family
    /// but not this member — deliberately not defaulted onto
    /// [`Self::forget_source`] for the [`ForgetSelector::Source`] arm, because
    /// a default that quietly ignored `source_kind` would delete across kinds
    /// a caller had narrowed away from.
    ///
    /// [`MemoryError::Invalid`] for a `source_kind` the driver does not
    /// recognise, never an outcome of zero: on a delete, a zero the caller
    /// reads as "already gone" is worse than a refusal.
    ///
    /// Otherwise backend failures. Idempotent — a selector that matches
    /// nothing removes nothing and returns an all-zero [`ForgetOutcome`].
    async fn forget_matching(
        &self,
        selector: &ForgetSelector,
    ) -> Result<ForgetOutcome, MemoryError> {
        let _ = selector;
        Err(MemoryError::unsupported(Capability::Sources))
    }
}

/// Periodic upkeep the host's scheduler drives.
///
/// Every operation here must be safe to call repeatedly and safe to interrupt:
/// the scheduler may invoke them on a timer, and a desktop process can exit at
/// any point. A driver that cannot bound the work should do a slice per call
/// and report progress in [`MaintenanceReport`].
///
/// [`Self::purge_all`] is the one member no scheduler may drive. It sits here
/// because this is where the optional operator-triggered mutations live, not
/// because it is upkeep; its own docs say why no other family could hold it.
#[async_trait]
pub trait MemoryMaintenance: Send + Sync {
    /// Recompute embeddings for content whose embedding is missing or stale.
    ///
    /// # Errors
    ///
    /// Backend failures, or [`MemoryError::BudgetExceeded`] when an embedding
    /// budget is exhausted mid-run.
    async fn reembed(&self) -> Result<MaintenanceReport, MemoryError>;

    /// Reclaim space: vacuum indexes, drop tombstones, prune dead references.
    ///
    /// # Errors
    ///
    /// Backend failures only.
    async fn compact(&self) -> Result<MaintenanceReport, MemoryError>;

    /// Merge and summarise accumulated memory — the "dream" pass.
    ///
    /// The embedded driver maps this onto its seal/cascade/reembed cycle; an
    /// external driver maps it onto whatever it calls the same idea. The
    /// contract deliberately does not specify the mechanism, only that it is
    /// the operation a scheduler runs when the system is idle.
    ///
    /// # Errors
    ///
    /// Backend failures only.
    async fn consolidate(&self) -> Result<MaintenanceReport, MemoryError>;

    /// Read-only integrity check.
    ///
    /// Reports findings in [`MaintenanceReport::findings`] and must change
    /// nothing — [`MaintenanceReport::changed`] is always `0`. A driver that
    /// repairs as it inspects should expose that as [`Self::compact`] instead,
    /// so an operator can diagnose without mutating.
    ///
    /// # Errors
    ///
    /// Backend failures only. A *finding* is not an error: a store with
    /// problems still returns `Ok` with the problems listed.
    async fn doctor(&self) -> Result<MaintenanceReport, MemoryError>;

    /// Aggregate counts over what this driver has stored.
    ///
    /// Defaulted to an empty [`StoreStats`] rather than `Unsupported`, and the
    /// difference matters: this is a diagnostic, and a caller asking "how much
    /// is stored" can do something sensible with "nothing reported" while
    /// having nothing to do with an error. A driver that can answer should.
    ///
    /// # Errors
    ///
    /// Backend failures only.
    async fn store_stats(&self) -> Result<StoreStats, MemoryError> {
        Ok(StoreStats::default())
    }

    /// Give terminally-failed queue work another attempt, and nudge whatever
    /// drains the queue.
    ///
    /// The nudge is part of the operation, not a separate call. A driver that
    /// requeues without waking has moved rows from `failed` back to `ready`
    /// and left them there until the next scheduled window, which looks
    /// identical to a retry that did not work — and the caller has no way to
    /// ask for the wake on its own.
    ///
    /// What counts as retryable is the driver's judgement. A failure it will
    /// never recover from is one it should leave parked; the caller is asking
    /// for another attempt, not asserting that one can succeed.
    ///
    /// Defaulted to an empty report — a driver with no queue has nothing to
    /// retry, which is true of it rather than a refusal.
    ///
    /// # Errors
    ///
    /// Backend failures only.
    async fn retry_failed(&self) -> Result<MaintenanceReport, MemoryError> {
        Ok(MaintenanceReport::default())
    }

    /// The ingest and re-embed queue's state.
    ///
    /// `kind` narrows to one job kind (the driver's own identifier); `None`
    /// counts every kind. A driver with no queue answers all-zero, which is
    /// true of it rather than a refusal — and so does a kind this driver does
    /// not have, since "no jobs of a kind I never enqueue" is the honest
    /// count. A caller that does not know the driver's vocabulary passes
    /// `None`; that is what the `Option` is for.
    ///
    /// # Errors
    ///
    /// Backend failures only.
    async fn queue_stats(&self, kind: Option<&str>) -> Result<QueueStats, MemoryError> {
        let _ = kind;
        Ok(QueueStats::default())
    }

    /// The most recent terminal queue failure, if the driver records one.
    ///
    /// `Ok(None)` means "nothing has failed", which is why this is not an
    /// error: a healthy queue and a driver that keeps no failure history give
    /// the same answer, and neither is a fault the caller can act on.
    ///
    /// # Errors
    ///
    /// Backend failures only.
    async fn latest_queue_failure(&self) -> Result<Option<QueueFailure>, MemoryError> {
        Ok(None)
    }

    /// Whether a re-embedding backfill is still working through its rows.
    ///
    /// **Driver-process-wide, and deliberately not store-scoped.** A driver
    /// serving several stores in one process answers the same for all of them:
    /// `true` means "a backfill is running somewhere in this driver", not "in
    /// the store you asked about". That is why this is a member of its own
    /// rather than a field on [`Self::queue_stats`] — a per-store snapshot is
    /// asked of one bound provider, so a global sitting inside it reads as
    /// per-store, and a caller has no way to find out otherwise. A global
    /// behind a signature that says so is coarse; a global behind one that
    /// does not is wrong.
    ///
    /// Not derivable from the queue counts. A backfill runs as a chain that
    /// re-enqueues itself, so between one link settling and the next being
    /// written there is an instant with nothing ready, nothing running, and
    /// the backfill nevertheless unfinished. The consumer is
    /// absence-reasoning — deciding whether an empty semantic recall means
    /// "nothing remembered" or "not embedded yet" — and it gets that wrong at
    /// exactly that instant without this.
    ///
    /// Defaulted to `false`: a driver that never backfills is not backfilling,
    /// which is true of it rather than a refusal.
    ///
    /// # Errors
    ///
    /// Backend failures only.
    async fn backfill_in_progress(&self) -> Result<bool, MemoryError> {
        Ok(false)
    }

    /// Flush buffered work that is old enough to be written out.
    ///
    /// The caller is a "flush now" control: a user who does not want to wait
    /// for the scheduled window. Whether a flush is *scheduled* is the
    /// driver's business — this asks it to consider the buffers now, and
    /// reports what it found and whether it acted.
    ///
    /// Deduplication is the driver's, not the caller's. Two flushes inside one
    /// window must not schedule the work twice, and the second reports
    /// `enqueued: false` with a truthful `stale_buffers` — which is why both
    /// numbers are on [`FlushOutcome`] rather than a bare bool.
    ///
    /// Defaulted to an empty outcome: a driver with nothing buffered has
    /// nothing to flush, which is true of it rather than a refusal.
    ///
    /// # Errors
    ///
    /// Backend failures only.
    async fn flush_pending(&self) -> Result<FlushOutcome, MemoryError> {
        Ok(FlushOutcome::default())
    }

    /// Re-file already-stored connector documents into the memory tree (#6012).
    ///
    /// openhuman#6007 fixed the routing for items synced *from now on*. It could
    /// not fix the records already stored: the per-item sync gate treats an
    /// ingested document as done, so re-syncing fetches nothing and creates no
    /// tree rows. Those memories stay fully embedded in the document store and
    /// invisible to every tree-backed surface until something re-files them.
    ///
    /// Idempotent, and by construction rather than by bookkeeping — the ingest
    /// gate answers `already_ingested` for a document the tree already holds, so
    /// a second pass writes nothing and an interrupted pass loses nothing. That
    /// is also why `limit` bounds cost rather than carrying a cursor: a pass asks
    /// that gate before it spends, so a document already filed is reported but
    /// never charged, and calling again advances past it (openhuman#6051).
    ///
    /// Expensive on purpose to call explicitly: a pass is one read and one set
    /// of chunk embeddings per document. A driver must not run this on its own
    /// initiative — spending a user's embedding budget unasked is its own bug
    /// (openhuman#5324).
    ///
    /// Defaulted to an empty outcome, matching [`Self::flush_pending`]: a driver
    /// with no connector documents has nothing to re-file, which is a fact about
    /// it rather than a refusal. A driver that stores nothing at all should
    /// override and refuse instead, so "backfilled nothing" cannot read as work
    /// done.
    ///
    /// # Errors
    ///
    /// Backend failures only.
    async fn backfill_connector_trees(
        &self,
        _request: BackfillTreesRequest,
    ) -> Result<BackfillTreesOutcome, MemoryError> {
        Ok(BackfillTreesOutcome::default())
    }

    /// Drop everything derived from stored content and schedule its
    /// re-derivation.
    ///
    /// Summaries, buffers, entity indexes and the trees over them are all
    /// *derived* — recomputable from the chunks they were built from. This
    /// discards them and queues the work to build them again. **Nothing a
    /// caller wrote is deleted**, which is the invariant that makes it safe to
    /// offer as an operator control at all; a driver that cannot promise that
    /// must refuse rather than implement this.
    ///
    /// Necessarily one operation. Deleting the derived rows without scheduling
    /// re-derivation leaves a store that answers structural queries with
    /// nothing and looks healthy doing it, and the two halves are not
    /// separately useful.
    ///
    /// Defaulted to an empty outcome, for a driver with nothing derived.
    ///
    /// # Errors
    ///
    /// Backend failures only. A driver that keeps derived state it cannot
    /// rebuild should answer [`MemoryError::Unsupported`] rather than delete
    /// it.
    async fn reset_derived_index(&self) -> Result<ResetOutcome, MemoryError> {
        Ok(ResetOutcome::default())
    }

    /// Delete everything this driver has stored.
    ///
    /// The operator's factory reset: every chunk, every derived row, every
    /// queue entry. The inverse of [`Self::reset_derived_index`], which is
    /// safe precisely because it deletes only what it can rebuild. Nothing
    /// here is rebuildable, and a caller reaching it has already asked a
    /// human.
    ///
    /// # Where the wipe stops
    ///
    /// At the storage the driver owns. A driver that keeps content outside its
    /// database in a place of its own choosing clears that too: leaving it
    /// behind orphans bytes nothing will ever reference again. A directory the
    /// *host* created, configured, and hands the driver a path into is the
    /// host's to remove, and a driver deleting host-owned directories is
    /// reaching past its own storage into somewhere it cannot reason about.
    /// The embedded driver's content vault is the second kind.
    ///
    /// # Why this is on maintenance and not on portability
    ///
    /// A wipe reads like the companion of import and export, and that is the
    /// wrong home for it: [`crate::provider::MemoryPortability`] is a
    /// **mandatory** supertrait, so putting it there would oblige every driver
    /// that compiles to implement a destructive whole-store delete — including
    /// the ones with nothing to wipe, and the ones fronting a store that must
    /// never be wiped through this contract at all. This family is optional
    /// and already holds the operator-triggered mutations
    /// ([`Self::reset_derived_index`], [`Self::flush_pending`]), so a driver
    /// declines by not advertising rather than by implementing a stub.
    ///
    /// # Why this defaults to a refusal
    ///
    /// The rest of this family defaults to an empty result, and that is honest
    /// for a *read* that under-claims: "nothing to report" is true of a driver
    /// with no queue. A `purge_all` defaulting to `rows_deleted: 0` would
    /// report a completed wipe from a driver that deleted nothing, to a caller
    /// whose next act is telling the user their memory is gone. It is the same
    /// distinction [`crate::null::NullMemoryProvider`] draws when it overrides
    /// the two mutating defaults above rather than inheriting them.
    ///
    /// # Errors
    ///
    /// [`MemoryError::Unsupported`] from a driver that cannot — or must not —
    /// destroy its store on request; that is the correct answer for one
    /// fronting a shared or externally-owned backend.
    ///
    /// Otherwise backend failures. A partial wipe is a failure and not a
    /// smaller success: a driver that cannot make this atomic reports what it
    /// managed in the error, rather than returning `Ok` over a store that is
    /// now half there.
    async fn purge_all(&self) -> Result<PurgeOutcome, MemoryError> {
        Err(MemoryError::unsupported(Capability::Maintenance))
    }

    /// The typed, per-stage diagnosis of the driver's ingest pipeline.
    ///
    /// Read-only in exactly the sense [`Self::doctor`] is, and driven by the
    /// same pass. What differs is who reads the answer.
    ///
    /// # Why this is not [`Self::doctor`] widened
    ///
    /// [`MaintenanceReport`] is deliberately one shape across `reembed`,
    /// `compact`, `consolidate` and `doctor`, so a scheduler running all four
    /// on a timer does not special-case one. That is the right shape for a
    /// scheduler and the wrong one for an operator: it flattens a classified
    /// cause into a line of prose, and a caller that wants to *act* on the
    /// cause — localise the remediation, decide whether a retry could help,
    /// tell "nothing ingested" apart from "ingested, not yet embedded" — has
    /// to parse that prose back into the structure it was flattened from.
    ///
    /// Adding those fields to [`MaintenanceReport`] was the alternative. Four
    /// of its five producers would leave every one of them empty, so the type
    /// would stop describing what any single call returns; and changing
    /// `doctor`'s return type instead is a breaking change to a member drivers
    /// already implement.
    ///
    /// So the two coexist and a driver derives both from one pass:
    /// [`Self::doctor`] is the lossy projection a scheduler reads,
    /// [`Self::diagnose`] the full one a human or an agent reads.
    ///
    /// # Why a caller cannot compute this itself
    ///
    /// Two of the four parts of a [`Diagnosis`] exist only inside the driver's
    /// process. [`crate::provider::diagnosis::DegradedCapabilities`] is set by
    /// the embed and extract stages as they run, and
    /// [`crate::provider::diagnosis::DiagnosisCounters`] is a read of the
    /// driver's own storage. A caller that hosts no engine has neither, and
    /// what it would produce is not a stale diagnosis but a confident
    /// all-clear over counters of zero.
    ///
    /// # Errors
    ///
    /// [`MemoryError::Unsupported`] from a driver that cannot diagnose itself
    /// — deliberately, and unlike the reads above, which default to an empty
    /// answer. An empty [`Diagnosis`] is not "nothing to report": its
    /// `healthy` flag would have to say something, and both answers are lies.
    /// `false` with no stages sends a user hunting a fault that was never
    /// found; `true` reports a clean bill of health from a driver that never
    /// looked.
    ///
    /// Backend failures otherwise. A *finding* is not an error, for the reason
    /// [`Self::doctor`] gives: a pipeline with problems still returns `Ok`
    /// with the problems in it.
    async fn diagnose(&self) -> Result<Diagnosis, MemoryError> {
        Err(MemoryError::unsupported(Capability::Maintenance))
    }

    /// Which capabilities are currently running in a reduced mode.
    ///
    /// The degradation flags on their own: semantic recall fallen back to
    /// recency, extraction producing no structure, the storage path unusable —
    /// and the cause of the most severe of those, when the driver knows it.
    ///
    /// # Why this is not [`Self::diagnose`] with the rest thrown away
    ///
    /// Cost, and the difference is not marginal. A [`Diagnosis`] is a full
    /// pass: it counts chunks, counts jobs in three states, measures extraction
    /// coverage over the whole store, and inspects the configuration of every
    /// pipeline stage. This is a read of flags the pipeline sets as it runs —
    /// no query, no configuration walk, nothing that touches storage.
    ///
    /// That matters because of who calls it. A diagnosis is asked for once,
    /// deliberately, by someone looking at a problem. Degradation is polled: it
    /// is what a status indicator shows continuously, and driving that from a
    /// full pass would put an aggregate query over the chunk table on a
    /// repeating timer. The two members exist so a caller can ask the cheap
    /// question without paying for the expensive one — and, just as important,
    /// so it is not tempted to poll the expensive one and cache the answer,
    /// which is how a status light ends up reporting a degradation that cleared
    /// minutes ago.
    ///
    /// [`Diagnosis::degraded`] carries the same shape, from the same source, so
    /// a caller that has just run a diagnosis has no reason to call this too.
    ///
    /// # Why a caller cannot compute it
    ///
    /// The flags are set inside the driver, by the embed and extract stages, as
    /// they fail. Nothing observable from outside distinguishes a recall that
    /// ranked semantically from one that fell back to recency — both return
    /// rows, in an order, with no marker on them. A caller with no engine would
    /// report an all-clear, which is not a stale answer but a confidently wrong
    /// one.
    ///
    /// # Errors
    ///
    /// [`MemoryError::Unsupported`] from a driver that tracks no degradation
    /// state. Deliberately not defaulted to
    /// [`DegradedCapabilities::default()`], which is all-clear: a driver that
    /// has never looked would report that everything is fine, and the whole
    /// purpose of this member is to be believed when it says that.
    ///
    /// Otherwise backend failures — though a driver reading in-process flags
    /// has no failure path and should not invent one.
    async fn degraded_state(&self) -> Result<DegradedCapabilities, MemoryError> {
        Err(MemoryError::unsupported(Capability::Maintenance))
    }
}
