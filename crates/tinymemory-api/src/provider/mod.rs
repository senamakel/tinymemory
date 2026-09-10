//! The memory driver contract: [`MemoryProvider`] plus its capability
//! family traits a driver may implement.
//!
//! ## Shape
//!
//! ```text
//! MemoryProvider  ── identity, capabilities, health, shutdown
//!   : MemoryCore          (mandatory — supertrait, always callable)
//!   : MemoryRecall        (mandatory — supertrait, always callable)
//!   : MemoryPortability   (mandatory — supertrait, always callable)
//!   ├─ as_ingest()       -> Option<&dyn MemoryIngest>
//!   ├─ as_documents()    -> Option<&dyn MemoryDocuments>
//!   ├─ as_tree()         -> Option<&dyn MemoryTree>
//!   ├─ as_entities()     -> Option<&dyn MemoryEntities>
//!   ├─ as_graph()        -> Option<&dyn MemoryGraph>
//!   ├─ as_diff()         -> Option<&dyn MemoryDiff>
//!   ├─ as_goals()        -> Option<&dyn MemoryGoals>
//!   ├─ as_tool_memory()  -> Option<&dyn MemoryToolMemory>
//!   ├─ as_sources()      -> Option<&dyn MemorySourceSink>
//!   ├─ as_maintenance()  -> Option<&dyn MemoryMaintenance>
//!   ├─ as_people()       -> Option<&dyn MemoryPeople>
//!   ├─ as_chunks()       -> Option<&dyn MemoryChunks>
//!   ├─ as_retrieval()    -> Option<&dyn MemoryRetrieval>
//!   ├─ as_profile()      -> Option<&dyn MemoryProfile>
//!   ├─ as_episodic()     -> Option<&dyn MemoryEpisodic>
//!   ├─ as_source_sync()  -> Option<&dyn MemorySourceSync>
//!   └─ as_coding_sessions()
//!                        -> Option<&dyn MemoryCodingSessions>
//! ```
//!
//! The mandatory three are supertraits, so "mandatory" is enforced by the type
//! system rather than by a runtime check. The optional seventeen are accessors
//! that default to `None`, so absence is the default and presence is opt-in.
//!
//! ## Rules that bind every family
//!
//! 1. **Typed errors, always.** Every method returns
//!    `Result<_, MemoryError>`. The transport adapter maps an out-of-process
//!    `501` onto [`crate::error::MemoryError::Unsupported`], and the kernel
//!    distinguishes "cannot" from "failed". `anyhow::Error` would erase that.
//! 2. **No configuration crosses the boundary.** Not one signature names a
//!    config type. A driver holds its own configuration; the contract passes
//!    domain arguments only.
//! 3. **No host types.** Nothing here names an OpenHuman type, so a
//!    third-party driver depends on this crate alone.
//! 4. **The driver never assigns provenance.** [`crate::types::MemoryTaint`] is
//!    an argument on every write path and a preserved field on every import.
//! 5. **The host owns the loop** — with one recorded exception. Sealing,
//!    cascading and maintenance are all "run one step when asked", and no
//!    driver hooks the agent turn. *Source sync* is the exception, and it
//!    moved deliberately: a host that stops compiling an engine has no
//!    periodic loop left to run, so the loops went into the module beside the
//!    queue pool. What the caller kept is the manual trigger — see
//!    [`MemorySourceSync`], which exists because a user's "sync now" is not a
//!    schedule and no member of [`MemorySourceSink`] can express it.
//! 6. **Object safety throughout.** No generics, no `Self` returns, no
//!    associated constants — every family is usable as `&dyn`.
//!
//! ## Reference implementation
//!
//! [`crate::null::NullMemoryProvider`] implements every family directly for
//! conformance testing:
//! `/dev/null` semantics for the mandatory three, and
//! [`crate::error::MemoryError::Unsupported`] for the other seventeen, which it
//! does not advertise. It is what a compiled-out or unconfigured memory subsystem
//! binds to, and it doubles as the proof that the mandatory set is
//! implementable without a storage engine.

pub mod audit;
pub mod chunks;
pub mod content;
pub mod driver;
pub mod episodic;
pub mod knowledge;
pub mod mandatory;
pub mod operations;
pub mod people;
pub mod profile;
pub mod records;
pub mod retrieval;
pub mod scoring;
pub mod sessions;
pub mod sync;
// The value types every family exchanges, defined in `tinymemory-bus` and
// re-exported at their historical path. See this crate's `lib.rs` for why the
// vocabulary sits a layer below the traits.
//
// `diagnosis` is re-exported rather than wrapped in a module of its own here
// because it carries no trait: it is the return shape of one maintenance
// member, so there is nothing for a file on this side to hold.
pub use tinymemory_bus::provider::{diagnosis, types};

pub use audit::{audit_provider, CapabilityAudit};
pub use chunks::{
    ChunkDetail, ChunkEmbedding, ChunkListRow, ChunkQuery, ChunkScore, ChunkScoreSignals,
    MemoryChunks, SourceIngestQuery, SourceIngestStatus, SourceTotal, DEFAULT_DROP_THRESHOLD,
};
pub use content::{
    MemoryDocuments, MemoryIngest, MemoryTree, RootSummary, SummaryContext, SummaryInput,
    SummaryOutput,
};
pub use diagnosis::{
    DegradedCapabilities, Diagnosis, DiagnosisCounters, DiagnosisFailure, DiagnosisStage,
};
pub use driver::MemoryProvider;
pub use episodic::{
    ConversationSegment, EpisodicEvent, EpisodicTurn, EventKind, MemoryEpisodic, SegmentStatus,
};
pub use knowledge::{MemoryDiff, MemoryEntities, MemoryGraph, INBOUND_SCAN_LIMIT};
pub use mandatory::{MemoryCore, MemoryPortability, MemoryRecall};
pub use operations::{
    AnswerCitation, AnswerRequest, AnswerResponse, AnswerStep, MemoryAnswer,
    MemoryConversationIngest, MemoryDocumentIngest, MemoryEventIngest, MemoryLearningIngest,
    RawMemoryEvent,
};
pub use people::{
    AddressBookSeedOutcome, MemoryPeople, PersonHandle, PersonInteraction, PersonRecord, PersonRef,
    PersonScore, RankedPerson, ResolvedPerson,
};
pub use profile::{FacetState, FacetType, MemoryProfile, ProfileFacet, UserState};
pub use records::{MemoryGoals, MemoryMaintenance, MemorySourceSink, MemoryToolMemory};
pub use retrieval::{
    CoverWindowQuery, EntityMatch, FastRetrieveQuery, MemoryRetrieval, RetrievalHit,
    RetrievalNodeKind, RetrievalResponse, SourceRetrievalQuery,
};
pub use scoring::MemoryScoring;
pub use sessions::{
    CodingSessionIngestReport, CodingSessionIngestRequest, CodingSessionSource,
    MemoryCodingSessions,
};
pub use sync::{
    MemorySourceSync, RawArchiveCoverage, RawRebuildOutcome, SourceSyncState, SourceSyncStatus,
    SyncAuditEntry, SyncFreshness, SyncRunOutcome,
};
pub use types::{
    BackfillTreesOutcome, BackfillTreesRequest, ChangeKind, ChunkEntityOccurrence, DiffReport,
    EntityHit, EntityOccurrence, EntityRef, ExportPage, ExportRecord, FlushOutcome, ForgetOutcome,
    ForgetSelector, ImportOutcome, IngestItem, IngestOutcome, MaintenanceReport, PurgeOutcome,
    ResetOutcome, SnapshotRef, SourceChange, SourceItem, SourceScope,
};
