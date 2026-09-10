//! `TinyBus` service boundary for the memory surface.
//!
//! One object, `/ai/tinyhumans/tinymemory/Memory`, exporting every capability
//! family plus the four driver-level methods.
//!
//! ```text
//! DriverId()                                        -> String
//! Capabilities()                                    -> Capabilities
//! Health()                                          -> MemoryHealth
//! Shutdown()                                        -> ()
//! OpenStore(memory_subdir)                          -> object_path
//!
//! Store(namespace, key, content, category, session_id, taint) -> ()
//! Get(namespace, key)                               -> Option<MemoryEntry>
//! Forget(namespace, key)                            -> bool
//! List(namespace, category, session_id)              -> [MemoryEntry]
//! Namespaces()                                      -> [NamespaceSummary]
//! Recall(query, limit, opts, scope)                 -> [MemoryEntry]
//! ExportPage(cursor, limit)                         -> ExportPage
//! ImportRecords(records)                            -> ImportOutcome
//!
//! ListPeople(limit)                                 -> [RankedPerson]
//! GetPerson(person_id)                              -> Option<PersonRecord>
//! ResolveHandle(handle, create_if_missing)          -> Option<ResolvedPerson>
//! AddHandleAlias(person_id, handle)                 -> ()
//! ScorePerson(person_id)                            -> Option<PersonScore>
//! RecordInteraction(interaction)                    -> ()
//! SeedFromAddressBook()                             -> AddressBookSeedOutcome
//!
//! ListChunks(query, scope)                          -> [Chunk]
//! CountChunks(query, scope)                         -> u64
//! GetChunk(chunk_id)                                -> Option<Chunk>
//! ChunkDetail(chunk_id)                             -> Option<ChunkDetail>
//! ChunkEmbeddings(chunk_ids, model_signature)       -> [ChunkEmbedding]
//! StorageKinds()                                    -> [String]
//! ListChunkDetails(query, scope)                    -> [ChunkListRow]
//! SourceTotals(limit, scope)                        -> [SourceTotal]
//! ChunkScore(chunk_id)                              -> Option<ChunkScore>
//! SourceIngestStatus(source_prefixes)               -> [SourceIngestStatus]
//!
//! ListActiveFacets() / ListAllFacets()               -> [ProfileFacet]
//! GetFacet(key) / FacetsByType(type)                 -> facet(s)
//! UpsertFacet(facet) / UpsertProviderFacet(…)        -> ()
//! SetFacetUserState(key, state) / DeleteFacet(key)   -> bool
//! DeleteFacetById(id) / DropFacetsBelow(threshold)   -> bool / usize
//! WorkflowIdentityMatches(pattern, value)            -> bool
//!
//! FastRetrieve(query, options, scope)               -> RetrievalResponse
//! CoverWindow(window, scope)                        -> RetrievalResponse
//! SearchEntities(query, kinds, limit)               -> [EntityMatch]
//! RecallNamespaceScored(ns, query, limit, exclude)  -> [NamespaceMemoryHit]
//! RetrieveSource(query, scope)                      -> RetrievalResponse
//! RetrieveChildren(node_id, max_depth, query, limit, scope) -> [RetrievalHit]
//! RetrieveLeaves(chunk_ids, scope)                   -> [RetrievalHit]
//!
//! SummaryForest(limit, scope)                       -> SummaryForest
//! RecentLeaves(limit, scope)                        -> [TreeLeaf]
//! Summarise(inputs, context)                        -> SummaryOutput
//! RootSummaries(per_namespace_cap, total_cap)       -> [RootSummary]
//! RuntimeBufferWrite(ns, content, ts, metadata)     -> String
//! RuntimeReadNode(ns, node_id)                      -> Option<TreeNode>
//! RuntimeReadChildren(ns, parent_id)                -> [TreeNode]
//! RuntimeTreeStatus(ns)                             -> TreeStatus
//! RuntimeSummarize(ns, ts)                          -> Option<TreeNode>
//! RuntimeRebuild(ns)                                -> TreeStatus
//! FlavourProfile(scope)                             -> Option<String>
//!
//! TopEntities(kind, limit)                          -> [EntityOccurrence]
//! ChunkEntities(chunk_ids, kinds)                   -> [ChunkEntityOccurrence]
//! EntityChunkIds(entity_id, limit)                  -> [String]
//!
//! ForgetMatching(selector)                          -> ForgetOutcome
//! PurgeAll()                                        -> PurgeOutcome
//!
//! FlushSourceTree(source_scope)                     -> u64
//! Diagnose()                                        -> Diagnosis
//! DegradedState()                                   -> DegradedCapabilities
//!
//! RunConnectionSync(toolkit, connection_id)         -> SyncRunOutcome
//! BootstrapConnection(toolkit, connection_id)       -> ()
//! IsToolkitSyncable(toolkit)                      -> bool
//! RunSourceSync(source_id)                          -> SyncRunOutcome
//! SourceSyncState(toolkit, connection_id)           -> Option<SourceSyncState>
//! SyncAuditLog(limit)                               -> [SyncAuditEntry]
//! EstimateSyncCostUsd(input_tokens, output_tokens)  -> f64
//! SyncStatuses()                                    -> [SourceSyncStatus]
//! RawArchiveCoverage(tree_scope, archive_source_id) -> RawArchiveCoverage
//! RebuildFromRawArchive(tree_scope, archive_source_id) -> RawRebuildOutcome
//!
//! CodingSessionStatus()                             -> [CodingSessionSource]
//! IngestCodingSessions(request)                     -> CodingSessionIngestReport
//!
//! ExtractEntities(query)                             -> [String]
//! EmbedText(text)                                    -> [f32]
//! EmbedderSlug()                                     -> String
//! ```
//!
//! # Source scope crosses as an argument, never as ambient state
//!
//! Every scoped method above takes `scope` explicitly. In-process the engine
//! resolves it from a task-local; that task-local belongs to the *host's* task
//! and does not exist on this side of a bus call. Inferring it here would read
//! as absent, and absent means unrestricted — a source gate failing open.
//!
//! # Why the method list mirrors a trait exactly
//!
//! These are `tinymemory_api`'s [`MemoryProvider`] and all of its capability
//! traits, with the borrows replaced by owned equivalents. That
//! is deliberate: the host binds an `Arc<dyn MemoryProvider>`, so a host-side
//! client that forwards each method one-for-one is a *complete* provider with no
//! translation layer in between. Anything cleverer — batching, a combined
//! "recall and store" call — would put engine semantics on the wire, where two
//! sides could disagree about them.
//!
//! # Everything travels inline
//!
//! A `TinyBus` frame is JSON capped at 16 MiB. That is a real constraint for a
//! generated document, where a byte array costs about 3.5 bytes per byte, and it
//! is not one here: memory entries are *text*, which costs about 1.1× as JSON.
//! So there is no blob store, no chunking and no held output — the apparatus the
//! `tinydocs` module needs does not appear in this one.
//!
//! Inline does not mean unbounded, though, and the three list-returning methods
//! are not all bounded the same way:
//!
//! - `ExportPage` is paged by contract, with the caller choosing the page size.
//!   Asking for a million records in one page gets an error, correctly.
//! - `Recall` takes a `limit`, so the caller bounds the count — but not the
//!   bytes, since fifty entries each holding a large document still overflow.
//! - `List` takes **neither**. It has no limit and no cursor, so entries can
//!   accumulate across individually valid `Store` calls until the response
//!   cannot cross a frame, and the caller has no way to ask for less.
//!
//! So `List` and `Recall` are checked against [`MAX_RESPONSE_BYTES`] and refuse
//! with a named `BudgetExceeded` rather than truncating. Truncating would be the
//! worse failure: with no cursor, a short list is indistinguishable from a
//! complete one, so a caller would conclude the missing entries do not exist.
//! `Namespaces` is left unchecked — it returns one small summary per namespace,
//! and a host with enough namespaces to fill 16 MiB of summaries has a different
//! problem.
//!
//! # Errors are named, and the names are the contract
//!
//! [`MemoryError`] is a rich enum, but a bus error is a name plus a string. The
//! table that maps between them lives in [`tinymemory_api::wire`] and is used by
//! **both** ends, so the module and the host cannot drift into disagreeing about
//! what a name means. See that module for why there is one name per variant
//! rather than one per outcome class.
//!
//! **No method here logs a namespace key, an entry's content, or a recall
//! query.** All three are user memory content, and a module error must not carry
//! payload values.

use std::collections::HashMap;
use std::sync::Arc;

// Deliberately the async mutex, not `std::sync::Mutex`: the open path holds
// this guard across an `.await` (see `open_store`), which a std guard cannot
// be held across.
use tokio::sync::Mutex;

use chrono::{DateTime, Utc};
use tinybus::{Connection, Error as BusError, Result as BusResult};
use tinymemory_api::capabilities::{Capabilities, Capability};
use tinymemory_api::chunks::Chunk;
use tinymemory_api::error::MemoryError;
use tinymemory_api::goals::GoalsDoc;
use tinymemory_api::health::MemoryHealth;
use tinymemory_api::learning::LearningCandidate;
use tinymemory_api::operations::{AnswerRequest, AnswerResponse, RawMemoryEvent};
use tinymemory_api::provider::types::{
    BackfillTreesOutcome, BackfillTreesRequest, ChunkEntityOccurrence, DiffReport, EntityHit,
    EntityOccurrence, ExportPage, ExportRecord, FlushOutcome, ForgetOutcome, ForgetSelector,
    ImportOutcome, IngestItem, IngestOutcome, MaintenanceReport, PurgeOutcome, QueueFailure,
    QueueStats, ResetOutcome, SnapshotRef, SourceItem, SourceScope, StoreStats,
};
// `MemoryCore`, `MemoryRecall` and `MemoryPortability` are deliberately not
// imported: they are supertraits of `MemoryProvider`, so their methods are
// already callable on the trait object.
use tinymemory_api::provider::chunks::{
    ChunkDetail, ChunkEmbedding, ChunkListRow, ChunkQuery, ChunkScore, SourceIngestQuery,
    SourceIngestStatus, SourceTotal,
};
use tinymemory_api::provider::diagnosis::{DegradedCapabilities, Diagnosis};
use tinymemory_api::provider::episodic::{ConversationSegment, EpisodicEvent, EpisodicTurn};
use tinymemory_api::provider::people::{
    AddressBookSeedOutcome, PersonHandle, PersonInteraction, PersonRecord, PersonScore,
    RankedPerson, ResolvedPerson,
};
use tinymemory_api::provider::profile::{FacetType, ProfileFacet, UserState};
use tinymemory_api::provider::retrieval::{
    CoverWindowQuery, EntityMatch, FastRetrieveQuery, RetrievalHit, RetrievalResponse,
    SourceRetrievalQuery,
};
use tinymemory_api::provider::sessions::{
    CodingSessionIngestReport, CodingSessionIngestRequest, CodingSessionSource,
};
use tinymemory_api::provider::sync::{
    RawArchiveCoverage, RawRebuildOutcome, SourceSyncState, SourceSyncStatus, SyncAuditEntry,
    SyncRunOutcome,
};
use tinymemory_api::provider::MemoryProvider;
use tinymemory_api::recall::OwnedRecallOpts;
use tinymemory_api::tool_memory::ToolMemoryRule;
use tinymemory_api::tree::{
    IngestRequest, QueryResult, RootSummary, SummaryContext, SummaryForest, SummaryInput,
    SummaryOutput, TreeLeaf, TreeNode, TreeStatus,
};
use tinymemory_api::types::{
    GraphRelationRecord, MemoryCategory, MemoryEntry, MemoryKvRecord, MemoryTaint,
    NamespaceDocumentInput, NamespaceMemoryHit, NamespaceRetrievalContext, NamespaceSummary,
    StoredMemoryDocument,
};
use tinymemory_api::wire;

#[cfg(not(test))]
#[path = "instrumentation.rs"]
mod instrumentation;
#[cfg(test)]
#[path = "instrumentation_test.rs"]
mod instrumentation;

/// Well-known name exported by the `TinyMemory` module.
pub const BUS_NAME: &str = "ai.tinyhumans.tinymemory.Memory";

/// Object path exported by the `TinyMemory` module.
pub const OBJECT_PATH: &str = "/ai/tinyhumans/tinymemory/Memory";

/// How many stores one module process will open, across every subtree.
///
/// Sized for "a host with per-profile memory", which is the case `OpenStore`
/// exists for — one store per profile, and a host with sixty-four live profiles
/// in one process is already outside what this was built for. It is a backstop
/// against a caller that opens stores in a loop, not a quota anyone should
/// meet.
pub(crate) const MAX_OPEN_STORES: usize = 64;

/// The served object: a bound driver, plus what it needs to open a sibling
/// store on request.
pub(crate) struct MemoryService {
    provider: Arc<dyn MemoryProvider>,
    /// Everything needed to build a second store under a different subtree.
    ///
    /// `None` on the objects that `OpenStore` itself creates: a store opened
    /// this way cannot open further stores. That is not a limitation worth
    /// lifting — the host asks the root object, which knows the workspace — and
    /// it keeps the recursion finite by construction.
    opener: Option<Arc<StoreOpener>>,
}

/// The root object's ability to bring up additional stores under the same
/// workspace.
pub(crate) struct StoreOpener {
    connection: Connection,
    config: crate::config::ModuleConfig,
    /// Subtrees already served, so a second `OpenStore` for the same one
    /// returns the existing object instead of opening the database twice.
    ///
    /// Two live handles to one SQLite file is not a hypothetical problem: the
    /// engine runs migrations on open, and concurrent migration attempts on the
    /// same file are exactly the kind of corruption that is invisible until it
    /// is not.
    ///
    /// The guard is therefore held across the whole open, not just the lookup —
    /// a lock released between the check and the insert would let two callers
    /// through and produce exactly the double-open it is here to prevent. That
    /// is why this is a `tokio::sync::Mutex`.
    served: Mutex<HashMap<String, String>>,
    instrumentation: instrumentation::OpenStoreInstrumentation,
}

impl MemoryService {
    /// Serve `provider` as a leaf object — one store, no opener.
    pub(crate) fn new(provider: Arc<dyn MemoryProvider>) -> Self {
        Self {
            provider,
            opener: None,
        }
    }

    /// Serve `provider` as the root object, able to open sibling stores.
    pub(crate) fn root(provider: Arc<dyn MemoryProvider>, opener: Arc<StoreOpener>) -> Self {
        Self {
            provider,
            opener: Some(opener),
        }
    }
}

impl StoreOpener {
    pub(crate) fn new(connection: Connection, config: crate::config::ModuleConfig) -> Self {
        Self {
            connection,
            config,
            served: Mutex::new(HashMap::new()),
            instrumentation: instrumentation::OpenStoreInstrumentation::default(),
        }
    }
}

/// Object path for a store rooted at `memory_subdir`.
///
/// Derived rather than free-form so a caller cannot name an arbitrary bus path,
/// and sanitised to the characters an object path allows — a subdir reaches
/// this from a profile id, and an id that fails validation must produce a
/// refusal, not a malformed path.
fn object_path_for_subdir(memory_subdir: &str) -> Option<String> {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    if memory_subdir.is_empty()
        || memory_subdir.len() > 128
        || !memory_subdir
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }

    // TinyBus object-path elements accept ASCII alphanumerics and `_`, but a
    // profile id commonly contains `-`. Escape both punctuation characters so
    // the mapping remains injective (`a-b` cannot collide with `a_2db`).
    let mut component = String::with_capacity(memory_subdir.len());
    for byte in memory_subdir.bytes() {
        if byte.is_ascii_alphanumeric() {
            component.push(char::from(byte));
        } else {
            component.push('_');
            component.push(char::from(HEX[usize::from(byte >> 4)]));
            component.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    Some(format!("{OBJECT_PATH}/stores/{component}"))
}

macro_rules! require_family {
    ($service:expr, $accessor:ident, $capability:expr) => {
        $service
            .provider
            .$accessor()
            .ok_or_else(|| into_bus_error(&MemoryError::unsupported($capability)))?
    };
}

#[tinybus::interface(name = "ai.tinyhumans.tinymemory.Memory")]
impl MemoryService {
    /// The bound driver's stable identifier.
    async fn driver_id(&self) -> BusResult<String> {
        std::future::ready(Ok(self.provider.driver_id().to_string())).await
    }

    /// The families this driver implements.
    ///
    /// The host caches this at bind time, exactly as it would for an in-process
    /// driver — the trait documents that the set is asked once and must not
    /// change afterwards.
    async fn capabilities(&self) -> BusResult<Capabilities> {
        std::future::ready(Ok(self.provider.capabilities())).await
    }

    /// Current liveness, as the driver reports it.
    async fn health(&self) -> BusResult<MemoryHealth> {
        Ok(self.provider.health().await)
    }

    /// Release backend resources.
    ///
    /// Idempotent, as the trait requires. Note that this does **not** unload the
    /// module: `TinyBus` never unloads a library, so a host that shuts the
    /// driver down and rebinds gets a fresh engine inside the same mapped image.
    async fn shutdown(&self) -> BusResult<()> {
        // Only the root object drains the hooks, and `opener` is what marks it:
        // `OpenStore` hands back objects with `None` there. The bank is
        // process-wide — the engine's hook releases the job locks for the whole
        // queue, not for one subtree — so draining it when a single profile's
        // store shuts down would run the release early and leave nothing banked
        // for the shutdown that actually ends the process.
        //
        // Hooks before `provider.shutdown()`: releasing a lock is a write to the
        // very store the provider is about to release, and the other order
        // leaves it nothing to write through.
        if self.opener.is_some() {
            crate::host::run_shutdown_hooks().await;
        }
        self.provider
            .shutdown()
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// Bring up a store rooted at `<workspace>/<memory_subdir>` and return the
    /// object path serving it.
    ///
    /// # Why the module opens stores rather than the host selecting one per call
    ///
    /// A host with per-profile memory needs more than one store in a process.
    /// The alternative was a store selector threaded through every method on
    /// every capability family — a change to the shape of the whole contract,
    /// to express something that is not a property of a memory operation at
    /// all. Which store you are talking to is settled when you are handed a
    /// driver, exactly like which workspace you are bound to.
    ///
    /// So the root object opens stores and hands back object paths. Each is an
    /// ordinary [`MemoryService`] exporting the identical interface, and the
    /// contract does not change at all: `MemoryProvider` still describes one
    /// store, and a proxy still talks to one store.
    ///
    /// Idempotent per subtree — see [`StoreOpener::served`] for why opening the
    /// same database twice is worth going out of the way to avoid.
    async fn open_store(&self, memory_subdir: String) -> BusResult<String> {
        let Some(opener) = self.opener.as_ref() else {
            return Err(BusError::MethodFailed {
                name: "ai.tinyhumans.tinymemory.Error.Invalid".to_string(),
                message: "only the root memory object can open stores".to_string(),
            });
        };
        let Some(path) = object_path_for_subdir(&memory_subdir) else {
            // The subdir is rejected by shape, and the message says so without
            // echoing it: it derives from a profile id, which is user data.
            return Err(BusError::MethodFailed {
                name: "ai.tinyhumans.tinymemory.Error.Invalid".to_string(),
                message: "memory subdirectory is empty, over-long, or contains \
                          characters outside [A-Za-z0-9_-]"
                    .to_string(),
            });
        };

        // The guard is taken here and held to the end of the method, so the
        // check and the insert cannot be split by the open in between. An
        // earlier version dropped it before opening the store, which read as
        // idempotent but was not: two concurrent calls for the same subtree
        // both missed the map, both opened the database, and both ran
        // migrations against one file — the corruption this map exists to
        // prevent, arrived at through the map.
        //
        // It serializes opens of *different* subtrees too. That is accepted
        // rather than worked around: an open happens once per profile, and a
        // per-key lock map costs more complexity than the contention it saves.
        let mut served = opener.served.lock().await;
        if let Some(existing) = served.get(&memory_subdir) {
            log::debug!("[tinymemory:module] open_store reusing already-served subtree");
            return Ok(existing.clone());
        }

        // Each store is a SQLite file, an object path and a set of file
        // descriptors that live until the process exits — nothing here ever
        // closes one, because tinybus does not unserve. A caller that opens a
        // fresh subdir in a loop would therefore exhaust descriptors with no
        // way back short of a restart. The cap is far above any real host (one
        // store per profile) and exists so that a bug is refused by name
        // instead of degrading the whole process.
        if served.len() >= MAX_OPEN_STORES {
            log::error!(
                "[tinymemory:module] open_store refused: already serving {MAX_OPEN_STORES} stores"
            );
            return Err(BusError::MethodFailed {
                name: "ai.tinyhumans.tinymemory.Error.Invalid".to_string(),
                message: format!(
                    "this module already serves the maximum of {MAX_OPEN_STORES} memory stores"
                ),
            });
        }

        opener.instrumentation.record_allocation();
        let client = tinymemory_core::store::factories::create_memory_client_in_subdir(
            &opener.config.memory,
            None,
            "",
            &opener.config.embedding_routes,
            opener.config.storage_provider.as_ref(),
            &opener.config.workspace_dir,
            &memory_subdir,
        )
        .map_err(|error| {
            // Same reasoning as `setup`: the factory error names this process's
            // filesystem layout, which the caller has no business learning.
            log::error!("[tinymemory:module] open_store create store failed: {error}");
            BusError::MethodFailed {
                name: "ai.tinyhumans.tinymemory.Error.Other".to_string(),
                message: "could not open the requested memory store".to_string(),
            }
        })?;

        // No queue worker pool is started for this store, and that is a finding
        // rather than an omission. The engine's queue is rooted at the
        // workspace, not at the store subtree: every `queue::store` entry point
        // resolves its database through `engine_config`, which is
        // `memory_config_from(config, config.workspace_dir())`, while
        // `memory_subdir` reaches only `UnifiedMemory::new_with_memory_dir`. One
        // module process serves one workspace — `claim_process_setup` refuses a
        // second `setup` — so every store opened here shares the one queue
        // `setup` already started a pool for, and a second `queue::start` would
        // be a silent no-op besides: its guard is a process-global `Once`.
        //
        // Calling `crate::start_queue_pool` here anyway would be correct and
        // would make that invariant enforced rather than argued. It is left out
        // because it would start a real four-worker pool inside the unit tests
        // that exercise this method, whose workspaces are temporary directories
        // deleted while the workers still poll them — the workers then mark the
        // store degraded process-wide, which later tests read. The invariant is
        // asserted instead by `crate::claim_queue_pool`, which is the same
        // decision without the tasks.
        let provider = crate::provider::provider(&opener.config, Arc::new(client));
        opener.instrumentation.before_registration()?;
        opener
            .connection
            .serve_at(
                path.as_str().try_into()?,
                MemoryService::new(Arc::new(provider)),
            )
            .await?;

        // Recorded only after `serve_at` succeeds, so a failed open is retried
        // rather than caching a path nothing answers on. Both early returns
        // above leave the map untouched for the same reason.
        served.insert(memory_subdir, path.clone());
        log::info!("[tinymemory:module] open_store now serving an additional memory subtree");
        Ok(path)
    }

    /// Upsert an entry keyed by `(namespace, key)`.
    ///
    /// `taint` is a required argument rather than a defaulted one, mirroring the
    /// contract: a driver that could default provenance would be able to launder
    /// externally-sourced content into internal-trust content, which is the one
    /// failure mode the host's policy guard exists to prevent.
    async fn store(
        &self,
        namespace: String,
        key: String,
        content: String,
        category: MemoryCategory,
        session_id: Option<String>,
        taint: MemoryTaint,
    ) -> BusResult<()> {
        self.provider
            .store(
                &namespace,
                &key,
                &content,
                category,
                session_id.as_deref(),
                taint,
            )
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// Fetch the entry at an exact `(namespace, key)`.
    async fn get(&self, namespace: String, key: String) -> BusResult<Option<MemoryEntry>> {
        self.provider
            .get(&namespace, &key)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// Delete the entry at `(namespace, key)`, reporting whether it existed.
    async fn forget(&self, namespace: String, key: String) -> BusResult<bool> {
        self.provider
            .forget(&namespace, &key)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// List entries, narrowing by namespace, category and session.
    ///
    /// Bounded by [`MAX_RESPONSE_BYTES`]: unlike `Recall` and `ExportPage`, this
    /// method takes no limit and no cursor, so the caller has no way to ask for
    /// less. See [`ensure_response_fits`] for why the answer is a named refusal
    /// rather than a truncation.
    async fn list(
        &self,
        namespace: Option<String>,
        category: Option<MemoryCategory>,
        session_id: Option<String>,
    ) -> BusResult<Vec<MemoryEntry>> {
        let entries = self
            .provider
            .list(
                namespace.as_deref(),
                category.as_ref(),
                session_id.as_deref(),
            )
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&entries, "List")?;
        Ok(entries)
    }

    /// Enumerate namespaces with their aggregate counts.
    async fn namespaces(&self) -> BusResult<Vec<NamespaceSummary>> {
        self.provider
            .namespaces()
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// Ranked retrieval.
    ///
    /// `scope` is a query predicate the driver applies internally, not a filter
    /// the host may apply to the result: narrowing afterwards would let the
    /// driver spend its `limit` on entries the caller is not allowed to see and
    /// then return fewer than it could have.
    async fn recall(
        &self,
        query: String,
        limit: usize,
        opts: OwnedRecallOpts,
        scope: Option<SourceScope>,
    ) -> BusResult<Vec<MemoryEntry>> {
        let entries = self
            .provider
            .recall(&query, limit, &opts, scope.as_ref())
            .await
            .map_err(|error| into_bus_error(&error))?;
        // `limit` bounds the count but not the bytes: a caller asking for 50
        // entries that each hold a large document still overflows a frame.
        ensure_response_fits(&entries, "Recall")?;
        Ok(entries)
    }

    /// Read one page of the export, continuing from `cursor`.
    async fn export_page(&self, cursor: Option<String>, limit: usize) -> BusResult<ExportPage> {
        self.provider
            .export_page(cursor.as_deref(), limit)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// Write a batch of previously-exported records.
    ///
    /// Partial success is reported inside [`ImportOutcome`] rather than as an
    /// error, so a million-record restore is not aborted by one bad record.
    async fn import_records(&self, records: Vec<ExportRecord>) -> BusResult<ImportOutcome> {
        self.provider
            .import_records(records)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn ingest_document(&self, item: IngestItem) -> BusResult<IngestOutcome> {
        require_family!(self, as_document_ingest, Capability::DocumentIngest)
            .ingest_document(item)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn ingest_chat(&self, messages: Vec<IngestItem>) -> BusResult<IngestOutcome> {
        require_family!(self, as_conversation_ingest, Capability::ConversationIngest)
            .ingest_conversation(messages)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// Ingest one email thread, ordered by the items' timestamps.
    ///
    /// A driver that advertises `Ingest` may still refuse this one — the
    /// method has a default that answers `Unsupported`, since it postdates the
    /// family — so the capability check here admits the call and the driver has
    /// the last word.
    async fn ingest_email(&self, messages: Vec<IngestItem>) -> BusResult<IngestOutcome> {
        require_family!(self, as_ingest, Capability::Ingest)
            .ingest_email(messages)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn put_document(&self, input: NamespaceDocumentInput) -> BusResult<String> {
        require_family!(self, as_documents, Capability::Documents)
            .put_document(input)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn get_document(
        &self,
        namespace: String,
        key: String,
    ) -> BusResult<Option<StoredMemoryDocument>> {
        require_family!(self, as_documents, Capability::Documents)
            .get_document(&namespace, &key)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn list_documents(&self, namespace: Option<String>) -> BusResult<serde_json::Value> {
        require_family!(self, as_documents, Capability::Documents)
            .list_documents(namespace.as_deref())
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn list_namespaces(&self) -> BusResult<Vec<String>> {
        require_family!(self, as_documents, Capability::Documents)
            .list_namespaces()
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn delete_document(
        &self,
        namespace: String,
        document_id: String,
    ) -> BusResult<serde_json::Value> {
        require_family!(self, as_documents, Capability::Documents)
            .delete_document(&namespace, &document_id)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn clear_namespace(&self, namespace: String) -> BusResult<()> {
        require_family!(self, as_documents, Capability::Documents)
            .clear_namespace(&namespace)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn query_documents(
        &self,
        namespace: String,
        query: String,
        limit: usize,
    ) -> BusResult<NamespaceRetrievalContext> {
        let response = require_family!(self, as_documents, Capability::Documents)
            .query_documents(&namespace, &query, limit)
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&response, "QueryDocuments")?;
        Ok(response)
    }

    async fn recall_documents(
        &self,
        namespace: String,
        limit: usize,
    ) -> BusResult<NamespaceRetrievalContext> {
        let response = require_family!(self, as_documents, Capability::Documents)
            .recall_documents(&namespace, limit)
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&response, "RecallDocuments")?;
        Ok(response)
    }

    async fn append(&self, request: IngestRequest) -> BusResult<()> {
        require_family!(self, as_tree, Capability::Tree)
            .append(request)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn query_source(
        &self,
        namespace: String,
        source_id: String,
        limit: usize,
        scope: Option<SourceScope>,
    ) -> BusResult<Vec<Chunk>> {
        let response = require_family!(self, as_tree, Capability::Tree)
            .query_source(&namespace, &source_id, limit, scope.as_ref())
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&response, "QuerySource")?;
        Ok(response)
    }

    async fn drill_down(&self, namespace: String, node_id: String) -> BusResult<QueryResult> {
        require_family!(self, as_tree, Capability::Tree)
            .drill_down(&namespace, &node_id)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn seal(&self, namespace: String) -> BusResult<TreeStatus> {
        require_family!(self, as_tree, Capability::Tree)
            .seal(&namespace)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn cascade(&self, namespace: String) -> BusResult<TreeStatus> {
        require_family!(self, as_tree, Capability::Tree)
            .cascade(&namespace)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn entities(
        &self,
        namespace: String,
        query: Option<String>,
        limit: usize,
    ) -> BusResult<Vec<EntityHit>> {
        let response = require_family!(self, as_entities, Capability::Entities)
            .entities(&namespace, query.as_deref(), limit)
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&response, "Entities")?;
        Ok(response)
    }

    async fn entity_edges(
        &self,
        namespace: String,
        entity_id: String,
        limit: usize,
    ) -> BusResult<Vec<GraphRelationRecord>> {
        require_family!(self, as_entities, Capability::Entities)
            .entity_edges(&namespace, &entity_id, limit)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn touch_entities(&self, namespace: String, entity_ids: Vec<String>) -> BusResult<()> {
        require_family!(self, as_entities, Capability::Entities)
            .touch_entities(&namespace, &entity_ids)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn kv_get(
        &self,
        namespace: Option<String>,
        key: String,
    ) -> BusResult<Option<MemoryKvRecord>> {
        require_family!(self, as_graph, Capability::Graph)
            .kv_get(namespace.as_deref(), &key)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn kv_put(
        &self,
        namespace: Option<String>,
        key: String,
        value: serde_json::Value,
    ) -> BusResult<()> {
        require_family!(self, as_graph, Capability::Graph)
            .kv_put(namespace.as_deref(), &key, value)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn kv_delete(&self, namespace: Option<String>, key: String) -> BusResult<bool> {
        require_family!(self, as_graph, Capability::Graph)
            .kv_delete(namespace.as_deref(), &key)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn kv_list(
        &self,
        namespace: Option<String>,
        prefix: Option<String>,
        limit: usize,
    ) -> BusResult<Vec<MemoryKvRecord>> {
        let response = require_family!(self, as_graph, Capability::Graph)
            .kv_list(namespace.as_deref(), prefix.as_deref(), limit)
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&response, "KvList")?;
        Ok(response)
    }

    async fn relations(
        &self,
        namespace: Option<String>,
        subject: Option<String>,
        predicate: Option<String>,
        limit: usize,
    ) -> BusResult<Vec<GraphRelationRecord>> {
        let response = require_family!(self, as_graph, Capability::Graph)
            .relations(
                namespace.as_deref(),
                subject.as_deref(),
                predicate.as_deref(),
                limit,
            )
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&response, "Relations")?;
        Ok(response)
    }

    async fn put_relation(&self, relation: GraphRelationRecord) -> BusResult<()> {
        require_family!(self, as_graph, Capability::Graph)
            .put_relation(relation)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn capture_snapshot(&self, source_id: String) -> BusResult<SnapshotRef> {
        require_family!(self, as_diff, Capability::Diff)
            .capture_snapshot(&source_id)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn snapshots(&self, source_id: String, limit: usize) -> BusResult<Vec<SnapshotRef>> {
        require_family!(self, as_diff, Capability::Diff)
            .snapshots(&source_id, limit)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn diff(
        &self,
        source_id: String,
        from: Option<String>,
        to: String,
    ) -> BusResult<DiffReport> {
        require_family!(self, as_diff, Capability::Diff)
            .diff(&source_id, from.as_deref(), &to)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn goals(&self) -> BusResult<GoalsDoc> {
        require_family!(self, as_goals, Capability::Goals)
            .goals()
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn set_goals(&self, goals: GoalsDoc) -> BusResult<()> {
        require_family!(self, as_goals, Capability::Goals)
            .set_goals(goals)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn tool_rules(&self, tool_name: String) -> BusResult<Vec<ToolMemoryRule>> {
        require_family!(self, as_tool_memory, Capability::ToolMemory)
            .tool_rules(&tool_name)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn put_tool_rule(&self, rule: ToolMemoryRule) -> BusResult<()> {
        require_family!(self, as_tool_memory, Capability::ToolMemory)
            .put_tool_rule(rule)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn delete_tool_rule(&self, tool_name: String, rule_id: String) -> BusResult<bool> {
        require_family!(self, as_tool_memory, Capability::ToolMemory)
            .delete_tool_rule(&tool_name, &rule_id)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn accept_source_items(
        &self,
        source_id: String,
        source_kind: String,
        items: Vec<SourceItem>,
        taint: MemoryTaint,
    ) -> BusResult<IngestOutcome> {
        require_family!(self, as_sources, Capability::Sources)
            .accept_source_items(&source_id, &source_kind, items, taint)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn forget_source(&self, source_id: String) -> BusResult<u64> {
        require_family!(self, as_sources, Capability::Sources)
            .forget_source(&source_id)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn reembed(&self) -> BusResult<MaintenanceReport> {
        require_family!(self, as_maintenance, Capability::Maintenance)
            .reembed()
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn compact(&self) -> BusResult<MaintenanceReport> {
        require_family!(self, as_maintenance, Capability::Maintenance)
            .compact()
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn consolidate(&self) -> BusResult<MaintenanceReport> {
        require_family!(self, as_maintenance, Capability::Maintenance)
            .consolidate()
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn doctor(&self) -> BusResult<MaintenanceReport> {
        require_family!(self, as_maintenance, Capability::Maintenance)
            .doctor()
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn retry_failed(&self) -> BusResult<MaintenanceReport> {
        require_family!(self, as_maintenance, Capability::Maintenance)
            .retry_failed()
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn store_stats(&self) -> BusResult<StoreStats> {
        require_family!(self, as_maintenance, Capability::Maintenance)
            .store_stats()
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn queue_stats(&self, kind: Option<String>) -> BusResult<QueueStats> {
        require_family!(self, as_maintenance, Capability::Maintenance)
            .queue_stats(kind.as_deref())
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn latest_queue_failure(&self) -> BusResult<Option<QueueFailure>> {
        require_family!(self, as_maintenance, Capability::Maintenance)
            .latest_queue_failure()
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn backfill_in_progress(&self) -> BusResult<bool> {
        require_family!(self, as_maintenance, Capability::Maintenance)
            .backfill_in_progress()
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn flush_pending(&self) -> BusResult<FlushOutcome> {
        require_family!(self, as_maintenance, Capability::Maintenance)
            .flush_pending()
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn reset_derived_index(&self) -> BusResult<ResetOutcome> {
        require_family!(self, as_maintenance, Capability::Maintenance)
            .reset_derived_index()
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn recall_namespace_recent(
        &self,
        namespace: String,
        limit: usize,
    ) -> BusResult<Vec<NamespaceMemoryHit>> {
        let hits = require_family!(self, as_retrieval, Capability::Retrieval)
            .recall_namespace_recent(&namespace, limit)
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&hits, "RecallNamespaceRecent")?;
        Ok(hits)
    }

    // ── People ──────────────────────────────────────────────────────────────

    /// Known people, ranked by closeness.
    ///
    /// Size-checked like the other list-returning methods. `limit` bounds the
    /// *count* but not the bytes — a store of people each carrying many handles
    /// can still overflow a frame — so the ceiling is enforced on the encoded
    /// response rather than trusted to the caller's limit.
    async fn list_people(&self, limit: Option<usize>) -> BusResult<Vec<RankedPerson>> {
        let people = require_family!(self, as_people, Capability::People)
            .list_people(limit)
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&people, "ListPeople")?;
        Ok(people)
    }

    async fn get_person(&self, person_id: String) -> BusResult<Option<PersonRecord>> {
        require_family!(self, as_people, Capability::People)
            .get_person(&person_id)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn resolve_handle(
        &self,
        handle: PersonHandle,
        create_if_missing: bool,
    ) -> BusResult<Option<ResolvedPerson>> {
        require_family!(self, as_people, Capability::People)
            .resolve_handle(&handle, create_if_missing)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn add_handle_alias(&self, person_id: String, handle: PersonHandle) -> BusResult<()> {
        require_family!(self, as_people, Capability::People)
            .add_handle_alias(&person_id, &handle)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn score_person(&self, person_id: String) -> BusResult<Option<PersonScore>> {
        require_family!(self, as_people, Capability::People)
            .score_person(&person_id)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn record_interaction(&self, interaction: PersonInteraction) -> BusResult<()> {
        require_family!(self, as_people, Capability::People)
            .record_interaction(&interaction)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn seed_from_address_book(&self) -> BusResult<AddressBookSeedOutcome> {
        require_family!(self, as_people, Capability::People)
            .seed_from_address_book()
            .await
            .map_err(|error| into_bus_error(&error))
    }

    // ── Chunks ──────────────────────────────────────────────────────────────

    /// Chunks matching the query, size-checked.
    ///
    /// `ChunkQuery::limit` bounds rows, not bytes, and a chunk carries full
    /// content — so this is one of the methods where the ceiling matters most.
    async fn list_chunks(
        &self,
        query: ChunkQuery,
        scope: Option<SourceScope>,
    ) -> BusResult<Vec<Chunk>> {
        let chunks = require_family!(self, as_chunks, Capability::Chunks)
            .list_chunks(&query, scope.as_ref())
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&chunks, "ListChunks")?;
        Ok(chunks)
    }

    /// One chunk, size-checked.
    ///
    /// A single object is checked for the same reason a list is: the ceiling is
    /// a property of the frame, not of the row count, and one chunk carries
    /// full content with no bound of its own. A list of one that is refused
    /// while the singular read of the same chunk succeeds would be an odd
    /// contract to explain.
    async fn get_chunk(&self, chunk_id: String) -> BusResult<Option<Chunk>> {
        let chunk = require_family!(self, as_chunks, Capability::Chunks)
            .get_chunk(&chunk_id)
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&chunk, "GetChunk")?;
        Ok(chunk)
    }

    /// One chunk plus its metadata, size-checked.
    async fn chunk_detail(&self, chunk_id: String) -> BusResult<Option<ChunkDetail>> {
        let detail = require_family!(self, as_chunks, Capability::Chunks)
            .chunk_detail(&chunk_id)
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&detail, "ChunkDetail")?;
        Ok(detail)
    }

    async fn storage_kinds(&self) -> BusResult<Vec<String>> {
        require_family!(self, as_chunks, Capability::Chunks)
            .storage_kinds()
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// Embedding vectors are the largest thing this interface returns.
    ///
    /// A 1536-dimension vector encodes to roughly 10 KiB of JSON, so a few
    /// hundred chunks reach the frame ceiling on their own. Checked for the same
    /// reason `List` is, and refused by name rather than truncated — a short
    /// batch is indistinguishable from "those chunks have no vector".
    async fn chunk_embeddings(
        &self,
        chunk_ids: Vec<String>,
        model_signature: String,
    ) -> BusResult<Vec<ChunkEmbedding>> {
        let embeddings = require_family!(self, as_chunks, Capability::Chunks)
            .chunk_embeddings(&chunk_ids, &model_signature)
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&embeddings, "ChunkEmbeddings")?;
        Ok(embeddings)
    }

    // ── Retrieval ───────────────────────────────────────────────────────────

    async fn fast_retrieve(
        &self,
        query: String,
        options: FastRetrieveQuery,
        scope: Option<SourceScope>,
    ) -> BusResult<RetrievalResponse> {
        let response = require_family!(self, as_retrieval, Capability::Retrieval)
            .fast_retrieve(&query, options, scope.as_ref())
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&response, "FastRetrieve")?;
        Ok(response)
    }

    async fn cover_window(
        &self,
        window: CoverWindowQuery,
        scope: Option<SourceScope>,
    ) -> BusResult<RetrievalResponse> {
        let response = require_family!(self, as_retrieval, Capability::Retrieval)
            .cover_window(&window, scope.as_ref())
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&response, "CoverWindow")?;
        Ok(response)
    }

    // ── Profile ─────────────────────────────────────────────────────────────

    async fn list_active_facets(&self) -> BusResult<Vec<ProfileFacet>> {
        let facets = require_family!(self, as_profile, Capability::Profile)
            .list_active_facets()
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&facets, "ListActiveFacets")?;
        Ok(facets)
    }

    async fn list_all_facets(&self) -> BusResult<Vec<ProfileFacet>> {
        let facets = require_family!(self, as_profile, Capability::Profile)
            .list_all_facets()
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&facets, "ListAllFacets")?;
        Ok(facets)
    }

    async fn get_facet(&self, key: String) -> BusResult<Option<ProfileFacet>> {
        require_family!(self, as_profile, Capability::Profile)
            .get_facet(&key)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn facets_by_type(&self, facet_type: FacetType) -> BusResult<Vec<ProfileFacet>> {
        let facets = require_family!(self, as_profile, Capability::Profile)
            .facets_by_type(facet_type)
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&facets, "FacetsByType")?;
        Ok(facets)
    }

    // ── Episodic ────────────────────────────────────────────────────────────

    /// Record one turn, answering with the row id the engine assigned it.
    async fn insert_turn(&self, turn: EpisodicTurn) -> BusResult<i64> {
        require_family!(self, as_episodic, Capability::Episodic)
            .insert_turn(&turn)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// Every recorded turn for one session, oldest first.
    async fn session_turns(&self, session_id: String) -> BusResult<Vec<EpisodicTurn>> {
        let turns = require_family!(self, as_episodic, Capability::Episodic)
            .session_turns(&session_id)
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&turns, "SessionTurns")?;
        Ok(turns)
    }

    /// The open segment for a session, if there is one.
    async fn open_segment(&self, session_id: String) -> BusResult<Option<ConversationSegment>> {
        require_family!(self, as_episodic, Capability::Episodic)
            .open_segment(&session_id)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// Start a new segment.
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors `MemoryEpisodic::create_segment`; the service layer must \
                  not reshape a contract signature"
    )]
    async fn create_segment(
        &self,
        segment_id: String,
        session_id: String,
        namespace: String,
        start_episodic_id: i64,
        start_seq: Option<u32>,
        start_timestamp: f64,
        now: f64,
    ) -> BusResult<()> {
        require_family!(self, as_episodic, Capability::Episodic)
            .create_segment(
                &segment_id,
                &session_id,
                &namespace,
                start_episodic_id,
                start_seq,
                start_timestamp,
                now,
            )
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// Extend a segment to include one more turn.
    async fn append_turn(
        &self,
        segment_id: String,
        episodic_id: i64,
        seq: Option<u32>,
        timestamp: f64,
        now: f64,
    ) -> BusResult<()> {
        require_family!(self, as_episodic, Capability::Episodic)
            .append_turn(&segment_id, episodic_id, seq, timestamp, now)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// Mark a segment closed.
    async fn close_segment(&self, segment_id: String, now: f64) -> BusResult<()> {
        require_family!(self, as_episodic, Capability::Episodic)
            .close_segment(&segment_id, now)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// Attach a summary to a closed segment.
    async fn set_segment_summary(
        &self,
        segment_id: String,
        summary: String,
        now: f64,
    ) -> BusResult<()> {
        require_family!(self, as_episodic, Capability::Episodic)
            .set_segment_summary(&segment_id, &summary, now)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// Store a segment's embedding under `model_signature`.
    async fn upsert_segment_embedding(
        &self,
        segment_id: String,
        model_signature: String,
        embedding: Vec<f32>,
        created_at: f64,
    ) -> BusResult<()> {
        require_family!(self, as_episodic, Capability::Episodic)
            .upsert_segment_embedding(&segment_id, &model_signature, &embedding, created_at)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn insert_event(&self, event: EpisodicEvent) -> BusResult<()> {
        require_family!(self, as_episodic, Capability::Episodic)
            .insert_event(&event)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn upsert_facet(&self, facet: ProfileFacet) -> BusResult<()> {
        require_family!(self, as_profile, Capability::Profile)
            .upsert_facet(&facet)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors `MemoryProfile::upsert_provider_facet`; the service layer \
                  must not reshape a contract signature"
    )]
    async fn upsert_provider_facet(
        &self,
        facet_id: String,
        facet_type: FacetType,
        key: String,
        value: String,
        confidence: f64,
        segment_id: Option<String>,
        observed_at: f64,
    ) -> BusResult<()> {
        require_family!(self, as_profile, Capability::Profile)
            .upsert_provider_facet(
                &facet_id,
                facet_type,
                &key,
                &value,
                confidence,
                segment_id.as_deref(),
                observed_at,
            )
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn set_facet_user_state(&self, key: String, user_state: UserState) -> BusResult<bool> {
        require_family!(self, as_profile, Capability::Profile)
            .set_facet_user_state(&key, user_state)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn delete_facet(&self, key: String) -> BusResult<bool> {
        require_family!(self, as_profile, Capability::Profile)
            .delete_facet(&key)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn delete_facet_by_id(&self, facet_id: String) -> BusResult<bool> {
        require_family!(self, as_profile, Capability::Profile)
            .delete_facet_by_id(&facet_id)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn drop_facets_below(&self, threshold: f64) -> BusResult<usize> {
        require_family!(self, as_profile, Capability::Profile)
            .drop_facets_below(threshold)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// Returns `bool`, not `BusResult<bool>` on the trait — but the wire needs a
    /// result, so an absent family answers `false` rather than erroring, which
    /// is the trait's documented reading of "cannot tell" for this predicate.
    async fn workflow_identity_matches(
        &self,
        key_pattern: String,
        canonical_value: String,
    ) -> BusResult<bool> {
        let Some(profile) = self.provider.as_profile() else {
            return Ok(false);
        };
        Ok(profile
            .workflow_identity_matches(&key_pattern, &canonical_value)
            .await)
    }

    async fn retrieve_source(
        &self,
        query: SourceRetrievalQuery,
        scope: Option<SourceScope>,
    ) -> BusResult<RetrievalResponse> {
        let response = require_family!(self, as_retrieval, Capability::Retrieval)
            .retrieve_source(&query, scope.as_ref())
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&response, "RetrieveSource")?;
        Ok(response)
    }

    async fn retrieve_children(
        &self,
        node_id: String,
        max_depth: u32,
        query: Option<String>,
        limit: Option<usize>,
        scope: Option<SourceScope>,
    ) -> BusResult<Vec<RetrievalHit>> {
        let hits = require_family!(self, as_retrieval, Capability::Retrieval)
            .retrieve_children(&node_id, max_depth, query.as_deref(), limit, scope.as_ref())
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&hits, "RetrieveChildren")?;
        Ok(hits)
    }

    async fn retrieve_leaves(
        &self,
        chunk_ids: Vec<String>,
        scope: Option<SourceScope>,
    ) -> BusResult<Vec<RetrievalHit>> {
        let hits = require_family!(self, as_retrieval, Capability::Retrieval)
            .retrieve_leaves(&chunk_ids, scope.as_ref())
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&hits, "RetrieveLeaves")?;
        Ok(hits)
    }

    async fn recall_namespace_scored(
        &self,
        namespace: String,
        query: String,
        limit: usize,
        exclude_session_id: Option<String>,
    ) -> BusResult<Vec<NamespaceMemoryHit>> {
        let hits = require_family!(self, as_retrieval, Capability::Retrieval)
            .recall_namespace_scored(&namespace, &query, limit, exclude_session_id.as_deref())
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&hits, "RecallNamespaceScored")?;
        Ok(hits)
    }

    async fn search_entities(
        &self,
        query: String,
        kinds: Option<Vec<String>>,
        limit: usize,
    ) -> BusResult<Vec<EntityMatch>> {
        let matches = require_family!(self, as_retrieval, Capability::Retrieval)
            .search_entities(&query, kinds.as_deref(), limit)
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&matches, "SearchEntities")?;
        Ok(matches)
    }

    /// How many chunks `ListChunks` matches, with its page bounds ignored.
    ///
    /// Declared here, at the end, rather than beside `ListChunks`: member order
    /// is the wire order this module serves, and `tinymemory_bus::METHODS` is
    /// compared against it as a sequence, so a new member is appended rather
    /// than filed with its family.
    ///
    /// Not size-checked. The ceiling exists for responses that carry content;
    /// this one is a number, and no query can make it bigger.
    async fn count_chunks(&self, query: ChunkQuery, scope: Option<SourceScope>) -> BusResult<u64> {
        require_family!(self, as_chunks, Capability::Chunks)
            .count_chunks(&query, scope.as_ref())
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// The store-wide entity index, most-observed first — see `Entities` for
    /// the namespace-scoped, hotness-ranked read these three do not replace.
    ///
    /// Appended here rather than filed beside `Entities` for the reason
    /// `count_chunks` gives above: member order is wire order.
    async fn top_entities(
        &self,
        kind: Option<String>,
        limit: usize,
    ) -> BusResult<Vec<EntityOccurrence>> {
        let rows = require_family!(self, as_entities, Capability::Entities)
            .top_entities(kind.as_deref(), limit)
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&rows, "TopEntities")?;
        Ok(rows)
    }

    /// Every entity indexed against a batch of chunks.
    ///
    /// The batch is the point. The caller this exists for is drawing a graph
    /// over a page of chunks, and one id per call is one round trip per chunk —
    /// a page of a thousand becomes a thousand calls for a single view.
    /// `kinds` narrows to the kinds that caller will actually render, so the
    /// frame carries the rows it asked for instead of the whole index of every
    /// chunk in the page.
    ///
    /// Rows come back as [`ChunkEntityOccurrence`] rather than
    /// [`EntityOccurrence`] because over a batch a flat list has no other way
    /// back to the chunk each row describes — see the contract, which says to
    /// group by `chunk_id` and never index by position.
    ///
    /// Widening the arguments is only legitimate because this member has never
    /// shipped: it was added on this branch, so no released host calls the
    /// single-id form. Its position in the member sequence is unchanged, which
    /// is what the drift assertion pins.
    ///
    /// Size-checked even though the contract gives it no `limit`: the bound is
    /// the extraction of the chunks named, which is the driver's number rather
    /// than the caller's, and an over-large frame the host cannot decode is a
    /// worse answer than a named refusal naming the method.
    async fn chunk_entities(
        &self,
        chunk_ids: Vec<String>,
        kinds: Option<Vec<String>>,
    ) -> BusResult<Vec<ChunkEntityOccurrence>> {
        let rows = require_family!(self, as_entities, Capability::Entities)
            .chunk_entities(&chunk_ids, kinds.as_deref())
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&rows, "ChunkEntities")?;
        Ok(rows)
    }

    /// The chunks one entity was observed in, as ids.
    async fn entity_chunk_ids(&self, entity_id: String, limit: usize) -> BusResult<Vec<String>> {
        let ids = require_family!(self, as_entities, Capability::Entities)
            .entity_chunk_ids(&entity_id, limit)
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&ids, "EntityChunkIds")?;
        Ok(ids)
    }

    /// Every sealed summary in the store, with the tree each belongs to.
    ///
    /// Appended here rather than filed beside `DrillDown` for the reason
    /// `count_chunks` gives above: member order is wire order.
    ///
    /// Size-checked, and it is the method most likely to hit the ceiling: the
    /// caller's `limit` bounds *nodes*, not bytes, and a store of long-scoped
    /// trees can put a forest-sized walk over a frame. A named refusal telling
    /// the caller to lower the bound beats a frame the host cannot decode.
    async fn summary_forest(
        &self,
        limit: usize,
        scope: Option<SourceScope>,
    ) -> BusResult<SummaryForest> {
        let forest = require_family!(self, as_tree, Capability::Tree)
            .summary_forest(limit, scope.as_ref())
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&forest, "SummaryForest")?;
        Ok(forest)
    }

    /// The newest leaves and the summaries that sealed them.
    async fn recent_leaves(
        &self,
        limit: usize,
        scope: Option<SourceScope>,
    ) -> BusResult<Vec<TreeLeaf>> {
        let leaves = require_family!(self, as_tree, Capability::Tree)
            .recent_leaves(limit, scope.as_ref())
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&leaves, "RecentLeaves")?;
        Ok(leaves)
    }

    /// What `ChunkDetail` returns, for a whole page in one read.
    ///
    /// Appended here rather than filed beside `ListChunks` for the reason
    /// `count_chunks` gives above: member order is wire order.
    ///
    /// It is not `ChunkDetail` in a loop, and the difference is not stylistic.
    /// One detail is several engine reads, so a thousand-row page done that way
    /// is several thousand queries behind a thousand round trips. Sharing
    /// `ListChunks`' own filter is the other half: a page and the details
    /// describing it cannot disagree about which chunks are in it.
    ///
    /// Size-checked, and it is the chunk method most likely to trip the
    /// ceiling: a row carries chunk text, so the limit that bounds rows does
    /// not bound bytes.
    async fn list_chunk_details(
        &self,
        query: ChunkQuery,
        scope: Option<SourceScope>,
    ) -> BusResult<Vec<ChunkListRow>> {
        let rows = require_family!(self, as_chunks, Capability::Chunks)
            .list_chunk_details(&query, scope.as_ref())
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&rows, "ListChunkDetails")?;
        Ok(rows)
    }

    /// One row per source, with what that source put in the store.
    ///
    /// Aggregated by the driver because the alternative is listing every chunk
    /// and grouping caller-side, which crosses the whole store to compute a
    /// handful of counts — and crosses it as content, which is what the
    /// response ceiling is there to stop.
    ///
    /// Size-checked for the same reason `TopEntities` is: `limit` bounds rows,
    /// and the ceiling is a property of the frame rather than of the row count.
    async fn source_totals(
        &self,
        limit: usize,
        scope: Option<SourceScope>,
    ) -> BusResult<Vec<SourceTotal>> {
        let totals = require_family!(self, as_chunks, Capability::Chunks)
            .source_totals(limit, scope.as_ref())
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&totals, "SourceTotals")?;
        Ok(totals)
    }

    /// Forget everything one selector names.
    ///
    /// One door rather than one member per shape — a chunk, a source, a source
    /// prefix, an owner. The four deletions differ only in which rows they
    /// match, and four members would be four chances for one of them to leave
    /// behind a side table the others clear.
    ///
    /// Not size-checked: the response counts what went, and no selector can
    /// make a count bigger.
    async fn forget_matching(&self, selector: ForgetSelector) -> BusResult<ForgetOutcome> {
        require_family!(self, as_sources, Capability::Sources)
            .forget_matching(&selector)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// Erase every row this driver holds.
    ///
    /// Filed under maintenance rather than sources because it is scoped to no
    /// source: it is the "wipe this store" a host offers behind a confirmation,
    /// and the driver's half of that is every table at once. What it does not
    /// touch is the filesystem — the content directory belongs to the host, and
    /// a driver deleting host directories would be reaching past its own
    /// storage into somewhere it cannot reason about.
    ///
    /// Not size-checked, for the reason `forget_matching` gives above.
    async fn purge_all(&self) -> BusResult<PurgeOutcome> {
        require_family!(self, as_maintenance, Capability::Maintenance)
            .purge_all()
            .await
            .map_err(|error| into_bus_error(&error))
    }

    // ── Tree, by source scope ───────────────────────────────────────────────

    /// Seal and cascade one source's tree now.
    ///
    /// Addressed by source scope rather than by namespace, unlike every other
    /// member of this family: the caller is looking at one connected source and
    /// that is the identity it holds. The scope is **not** logged — it carries
    /// a platform and a connection id, and the second is user data.
    async fn flush_source_tree(&self, source_scope: String) -> BusResult<u64> {
        require_family!(self, as_tree, Capability::Tree)
            .flush_source_tree(&source_scope)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    // ── Maintenance, typed ──────────────────────────────────────────────────

    /// The typed, per-stage pipeline diagnosis.
    ///
    /// Beside `Doctor` rather than replacing it. `Doctor` returns the uniform
    /// `MaintenanceReport` a scheduler reads across all four upkeep calls; this
    /// returns the classified causes, degradation flags and counters an
    /// operator or an agent acts on. Both come from one pass driver-side.
    ///
    /// Not size-checked: the report is bounded by the driver's stage list.
    async fn diagnose(&self) -> BusResult<Diagnosis> {
        require_family!(self, as_maintenance, Capability::Maintenance)
            .diagnose()
            .await
            .map_err(|error| into_bus_error(&error))
    }

    // ── Source sync the driver runs itself ──────────────────────────────────

    /// Sync one connection now.
    ///
    /// The manual "sync now" a user presses. The periodic loops already run in
    /// this process; this is the on-demand half, which a schedule cannot
    /// express.
    ///
    /// Neither argument is logged. A toolkit is harmless, a connection id is
    /// not, and logging one without the other says nothing useful.
    async fn run_connection_sync(
        &self,
        toolkit: String,
        connection_id: String,
    ) -> BusResult<SyncRunOutcome> {
        require_family!(self, as_source_sync, Capability::SourceSync)
            .run_connection_sync(&toolkit, &connection_id)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// Run one configured memory source through its pipeline, whatever kind.
    ///
    /// Beside `RunConnectionSync` rather than replacing it: that member is
    /// Composio-shaped and this one covers every kind, including the folder,
    /// repository, feed and web-page sources that have no toolkit or connection
    /// id to name.
    async fn run_source_sync(&self, source_id: String) -> BusResult<SyncRunOutcome> {
        require_family!(self, as_source_sync, Capability::SourceSync)
            .run_source_sync(&source_id)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// Run one connection's first-time bootstrap.
    ///
    /// Beside `RunConnectionSync` rather than inside it: a sync moves items and
    /// runs many times, a bootstrap establishes what a sync then assumes and
    /// runs once. They also fail differently, and a caller can only decline to
    /// stop syncing over a failed bootstrap if it can tell the two apart.
    async fn bootstrap_connection(&self, toolkit: String, connection_id: String) -> BusResult<()> {
        require_family!(self, as_source_sync, Capability::SourceSync)
            .bootstrap_connection(&toolkit, &connection_id)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// Whether this driver has a sync pipeline for one toolkit.
    ///
    /// Asked rather than answered from a list the caller holds, so the
    /// normalisation the driver applies never has to be reimplemented on the
    /// far side of the bus.
    async fn is_toolkit_syncable(&self, toolkit: String) -> BusResult<bool> {
        require_family!(self, as_source_sync, Capability::SourceSync)
            .is_toolkit_syncable(&toolkit)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// The persisted cursor, dedup and budget state for one connection.
    ///
    /// `None` is "never synced", which is a state and not an error — a status
    /// list covering every connection would otherwise be all errors on a fresh
    /// install.
    async fn source_sync_state(
        &self,
        toolkit: String,
        connection_id: String,
    ) -> BusResult<Option<SourceSyncState>> {
        require_family!(self, as_source_sync, Capability::SourceSync)
            .source_sync_state(&toolkit, &connection_id)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// Past sync runs, newest first.
    ///
    /// Size-checked, and the only member of this family that needs to be: the
    /// audit log is append-only for the life of a workspace, so it is the one
    /// response here that grows without a bound the caller controls. `limit`
    /// bounds the *count*; the bytes are bounded here, and a refusal names
    /// `BudgetExceeded` so the caller knows to ask for fewer rows rather than
    /// reading a silently short log as a complete one.
    async fn sync_audit_log(&self, limit: Option<usize>) -> BusResult<Vec<SyncAuditEntry>> {
        let entries = require_family!(self, as_source_sync, Capability::SourceSync)
            .sync_audit_log(limit)
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&entries, "SyncAuditLog")?;
        Ok(entries)
    }

    /// Price a token count at the driver's own rate.
    ///
    /// A bus round trip for two multiplications, and deliberately so: the same
    /// constants stamped `estimated_cost_usd` onto every audit row above, and a
    /// caller holding its own copy would show a projection and a historical
    /// total computed at two different prices.
    async fn estimate_sync_cost_usd(
        &self,
        input_tokens: u64,
        output_tokens: u64,
    ) -> BusResult<f64> {
        require_family!(self, as_source_sync, Capability::SourceSync)
            .estimate_sync_cost_usd(input_tokens, output_tokens)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// Per-provider sync progress, derived from stored content.
    ///
    /// Not size-checked: one row per provider, and a store with enough distinct
    /// providers to fill a frame has a different problem — the same reasoning
    /// `Namespaces` is left unchecked under.
    async fn sync_statuses(&self) -> BusResult<Vec<SourceSyncStatus>> {
        require_family!(self, as_source_sync, Capability::SourceSync)
            .sync_statuses()
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// How much of one raw archive its summary tree covers.
    async fn raw_archive_coverage(
        &self,
        tree_scope: String,
        archive_source_id: String,
    ) -> BusResult<RawArchiveCoverage> {
        require_family!(self, as_source_sync, Capability::SourceSync)
            .raw_archive_coverage(&tree_scope, &archive_source_id)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// Re-derive a summary tree from its raw archive.
    ///
    /// Costs inference and can run long. It is a call rather than a background
    /// job on purpose: the module holds no notion of a caller's request, so a
    /// fire-and-forget rebuild would have nowhere to report to and no way to be
    /// cancelled. A caller that does not want to wait runs it off its own task.
    async fn rebuild_from_raw_archive(
        &self,
        tree_scope: String,
        archive_source_id: String,
    ) -> BusResult<RawRebuildOutcome> {
        require_family!(self, as_source_sync, Capability::SourceSync)
            .rebuild_from_raw_archive(&tree_scope, &archive_source_id)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    // ── Local coding-agent transcripts ──────────────────────────────────────

    /// What each supported coding agent's session store holds.
    ///
    /// Not size-checked: one row per agent the driver supports.
    async fn coding_session_status(&self) -> BusResult<Vec<CodingSessionSource>> {
        require_family!(self, as_coding_sessions, Capability::CodingSessions)
            .coding_session_status()
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// Distil coding sessions into observations.
    ///
    /// The longest-running member on this object: one or more sequential model
    /// calls per session, bounded by the request's session count and by the
    /// driver's own clamp on it. A caller enforcing a deadline does so on its
    /// own side — abandoning a run here would leave the driver's per-file state
    /// disagreeing with what it wrote.
    async fn ingest_coding_sessions(
        &self,
        request: CodingSessionIngestRequest,
    ) -> BusResult<CodingSessionIngestReport> {
        require_family!(self, as_coding_sessions, Capability::CodingSessions)
            .ingest_coding_sessions(request)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn extract_entities(&self, query: String) -> BusResult<Vec<String>> {
        require_family!(self, as_scoring, Capability::Scoring)
            .extract_entities(&query)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn embed_text(&self, text: String) -> BusResult<Vec<f32>> {
        require_family!(self, as_scoring, Capability::Scoring)
            .embed_text(&text)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    async fn embedder_slug(&self) -> BusResult<String> {
        require_family!(self, as_scoring, Capability::Scoring)
            .embedder_slug()
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// Fold summary inputs into one parent summary, through the driver's chat
    /// provider.
    ///
    /// Appended here rather than filed beside `Seal` for the reason
    /// `count_chunks` gives above: member order is wire order.
    ///
    /// The longest-running member of the tree family: one provider call, over
    /// the network, priced at the driver's rate. It is a call rather than a job
    /// for the same reason `RebuildFromRawArchive` is — the module holds no
    /// notion of a caller's request, so a fire-and-forget fold would have
    /// nowhere to report the summary it produced.
    ///
    /// Not size-checked. The response is one summary, clamped driver-side to
    /// the `token_budget` the caller itself supplied, so no input can make it
    /// exceed a frame. The *request* can be large — it carries every input's
    /// body — and that bound is the caller's: it chose how many inputs to fold.
    async fn summarise(
        &self,
        inputs: Vec<SummaryInput>,
        context: SummaryContext,
    ) -> BusResult<SummaryOutput> {
        require_family!(self, as_tree, Capability::Tree)
            .summarise(&inputs, &context)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// Every namespace's root summary, capped per namespace and in total.
    ///
    /// Appended here for the reason above. The member name is deliberately
    /// shorter than the trait method it forwards to
    /// (`MemoryTree::root_summaries_with_caps`): the caps are visible in the
    /// signature on both sides, and a wire name is a string a host spells by
    /// hand, so it carries only what distinguishes the call.
    ///
    /// Size-checked even though `total_cap` already bounds the payload in
    /// characters, because that bound is the *caller's* number and nothing
    /// stops it being larger than a frame. A named refusal telling the caller
    /// to lower it beats a response the host cannot decode.
    async fn root_summaries(
        &self,
        per_namespace_cap: usize,
        total_cap: usize,
    ) -> BusResult<Vec<RootSummary>> {
        let summaries = require_family!(self, as_tree, Capability::Tree)
            .root_summaries_with_caps(per_namespace_cap, total_cap)
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&summaries, "RootSummaries")?;
        Ok(summaries)
    }

    /// Which capabilities are currently running in a reduced mode.
    ///
    /// Appended here for the reason `count_chunks` gives above: member order is
    /// wire order.
    ///
    /// Beside `Diagnose` rather than inside it, and the difference is the price.
    /// `Diagnose` runs the driver's whole diagnostic pass — an aggregate scan of
    /// the chunk table, three job counts, an extraction-coverage measurement and
    /// a walk of the pipeline configuration. This reads the flags the pipeline
    /// set as it ran. A status indicator polls the second; only a human asks for
    /// the first.
    ///
    /// Not size-checked: three booleans and at most one classified cause.
    async fn degraded_state(&self) -> BusResult<DegradedCapabilities> {
        require_family!(self, as_maintenance, Capability::Maintenance)
            .degraded_state()
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// One chunk's admission decision and the signals behind it.
    ///
    /// Appended here for the reason above.
    ///
    /// A diagnostic read — "why is this in memory, and why is that not" — and
    /// not an input to ranking, which the retrieval family owns. `None` is a
    /// chunk that was never scored, which is a different fact from one that
    /// scored zero; the driver must not collapse them and neither may a caller.
    ///
    /// Not size-checked. The response is one row of numbers plus, at most, the
    /// driver's own short rationale for the verdict.
    async fn chunk_score(&self, chunk_id: String) -> BusResult<Option<ChunkScore>> {
        require_family!(self, as_chunks, Capability::Chunks)
            .chunk_score(&chunk_id)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// How far ingest has got for each configured source the caller names.
    ///
    /// Appended here for the reason above.
    ///
    /// The caller supplies the chunk-id prefix per source because deriving it
    /// needs the host's source registry — the source's kind, its toolkit, its
    /// connection id — which is state the driver does not have and this contract
    /// exists to stop it reaching for. The driver answers only what it can read
    /// from its own tables: how many rows sit under that key, and how many of
    /// them are still in flight.
    ///
    /// A row comes back for every query, zero-filled when the prefix matches
    /// nothing. That is the whole reason this is not `SourceTotals`, which
    /// returns the groups that exist and therefore drops a source that has never
    /// synced — off a dashboard, where an absent row reads as a source that was
    /// never configured.
    ///
    /// Neither the prefixes nor the ids are logged: a connector prefix carries a
    /// connection id, which is user data.
    ///
    /// Size-checked, because the caller chooses how many sources to ask about
    /// and the rows are small but unbounded in number.
    async fn source_ingest_status(
        &self,
        source_prefixes: Vec<SourceIngestQuery>,
    ) -> BusResult<Vec<SourceIngestStatus>> {
        let rows = require_family!(self, as_chunks, Capability::Chunks)
            .source_ingest_status(&source_prefixes)
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&rows, "SourceIngestStatus")?;
        Ok(rows)
    }

    /// Buffer raw content for the markdown time tree, answering with where it
    /// landed.
    ///
    /// Appended here rather than filed beside `Append` for the reason
    /// `count_chunks` gives above: member order is wire order.
    ///
    /// Not size-checked. The response is one path string; the *request*
    /// carries the content, and that bound is the caller's, exactly as it is
    /// for `Summarise`.
    async fn runtime_buffer_write(
        &self,
        namespace: String,
        content: String,
        timestamp: DateTime<Utc>,
        metadata: Option<serde_json::Value>,
    ) -> BusResult<String> {
        require_family!(self, as_tree, Capability::Tree)
            .runtime_buffer_write(&namespace, &content, timestamp, metadata)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// One time-tree node, or none — appended here for the reason above.
    ///
    /// Size-checked, unlike `DrillDown`. The level budget bounds a node's
    /// *summary* and nothing else: `token_count` is documented as the count of
    /// `summary`, and the fold applies `NodeLevel::max_tokens` when it
    /// summarises the body. `TreeNode::metadata` is outside it — an
    /// `Option<String>` the engine fills with a serialized pending-fold
    /// receipt whose `buffer_filenames` holds one name per buffered entry in
    /// the hour, so it grows with how much was buffered rather than with any
    /// level's budget. Without the check an oversized node fails during frame
    /// encoding; with it the caller gets `BUDGET_EXCEEDED` and a reason.
    async fn runtime_read_node(
        &self,
        namespace: String,
        node_id: String,
    ) -> BusResult<Option<TreeNode>> {
        let node = require_family!(self, as_tree, Capability::Tree)
            .runtime_read_node(&namespace, &node_id)
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&node, "RuntimeReadNode")?;
        Ok(node)
    }

    /// A time-tree node's direct children — appended here for the reason
    /// above.
    ///
    /// Size-checked on `RuntimeReadNode`'s reasoning, which applies harder
    /// here: the calendar bounds the fanout to at most 31 children, but 31
    /// unbounded metadata blobs is still unbounded.
    async fn runtime_read_children(
        &self,
        namespace: String,
        parent_id: String,
    ) -> BusResult<Vec<TreeNode>> {
        let children = require_family!(self, as_tree, Capability::Tree)
            .runtime_read_children(&namespace, &parent_id)
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&children, "RuntimeReadChildren")?;
        Ok(children)
    }

    /// One namespace's time-tree shape and coverage — appended here for the
    /// reason above. Not size-checked: counts and timestamps.
    async fn runtime_tree_status(&self, namespace: String) -> BusResult<TreeStatus> {
        require_family!(self, as_tree, Capability::Tree)
            .runtime_tree_status(&namespace)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// Drain the buffer into the tree on the driver's provider — appended here
    /// for the reason above.
    ///
    /// Long-running on `Summarise`'s terms: provider calls, over the network,
    /// priced at the driver's rate — one per hour group drained plus the
    /// propagation above them.
    ///
    /// Size-checked on `RuntimeReadNode`'s reasoning. The node this answers
    /// with is the one the pass just wrote, so its receipt names every buffer
    /// file the pass drained — the largest metadata blob in the tree is the
    /// one returned here.
    async fn runtime_summarize(
        &self,
        namespace: String,
        timestamp: DateTime<Utc>,
    ) -> BusResult<Option<TreeNode>> {
        let node = require_family!(self, as_tree, Capability::Tree)
            .runtime_summarize(&namespace, timestamp)
            .await
            .map_err(|error| into_bus_error(&error))?;
        ensure_response_fits(&node, "RuntimeSummarize")?;
        Ok(node)
    }

    /// Rebuild the whole time tree from its hour leaves — appended here for
    /// the reason above. Long-running on `RuntimeSummarize`'s terms; the
    /// answer is one status row.
    async fn runtime_rebuild(&self, namespace: String) -> BusResult<TreeStatus> {
        require_family!(self, as_tree, Capability::Tree)
            .runtime_rebuild(&namespace)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// The compiled flavoured-root profile for one scope — appended here for
    /// the reason above.
    ///
    /// Not size-checked: the body is clamped driver-side to the flavoured
    /// root's own token budget at compile time, so no scope can make the
    /// artifact outgrow a frame.
    ///
    /// The scope is not logged — today's scopes are facet names, but the
    /// vocabulary is the caller's and nothing here may assume it stays free of
    /// user data.
    async fn flavour_profile(&self, scope: String) -> BusResult<Option<String>> {
        require_family!(self, as_tree, Capability::Tree)
            .flavour_profile(&scope)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// Ingest one learning candidate through the granular capability added
    /// after the runtime-tree doors. Kept at the interface tail to preserve
    /// every previously released wire slot.
    async fn ingest_learning(&self, learning: LearningCandidate) -> BusResult<IngestOutcome> {
        require_family!(self, as_learning_ingest, Capability::LearningIngest)
            .ingest_learning(learning)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// Ingest one raw event through the granular event capability. Appended
    /// here so older member indices remain stable.
    async fn ingest_event(&self, event: RawMemoryEvent) -> BusResult<IngestOutcome> {
        require_family!(self, as_event_ingest, Capability::EventIngest)
            .ingest_event(event)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// Produce a grounded answer through the granular answer capability.
    /// Appended here so older member indices remain stable.
    async fn answer(&self, request: AnswerRequest) -> BusResult<AnswerResponse> {
        require_family!(self, as_answer, Capability::Answer)
            .answer(request)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// Open a bounded manual-override window on the scheduler gate.
    ///
    /// The host calls this when the user explicitly asks for maintenance
    /// while the gate is paused (`mode = off`, signed-out, battery): for
    /// `seconds`, background claims read `Policy::Normal` and paused sleepers
    /// are woken, so a user's "process now" runs without turning the gate's
    /// protection off for anything they did not ask for (openhuman#5935).
    // async only for the interface macro's member contract — the body is one
    // synchronous global write, and that is the point: a claim's step-0 read
    // must never wait on this.
    #[allow(clippy::unused_async, clippy::unused_async_trait_impl)]
    async fn override_scheduler_gate(&self, seconds: u64) -> BusResult<()> {
        // Clamp: a window longer than an hour is the gate turned off with
        // extra steps, which is the config's job, not this member's.
        let seconds = seconds.min(3600);
        tinymemory_core::scheduler_gate::set_manual_override(seconds);
        log::info!(
            "[tinymemory:module] scheduler gate manually overridden for {seconds}s (host request)"
        );
        Ok(())
    }

    // Appended at the tail, not filed beside `flush_pending` where its family
    // sits. `#[tinybus::interface]` derives member order from this block, and
    // `the_served_members_are_exactly_the_published_contract` compares that
    // order positionally against `tinymemory_bus::METHODS` — which is
    // append-only for the same reason: a member inserted mid-list renumbers
    // every member after it (openhuman#6012).
    async fn backfill_connector_trees(
        &self,
        request: BackfillTreesRequest,
    ) -> BusResult<BackfillTreesOutcome> {
        require_family!(self, as_maintenance, Capability::Maintenance)
            .backfill_connector_trees(request)
            .await
            .map_err(|error| into_bus_error(&error))
    }

    /// Closed segments with no summary yet, for a host re-running its own
    /// summariser over a recap that failed (openhuman#6186).
    ///
    /// Appended at the tail for the same positional reason as the member above
    /// it; its family sits ~900 lines up, around `set_segment_summary`.
    async fn segments_pending_summary(&self, limit: u32) -> BusResult<Vec<ConversationSegment>> {
        require_family!(self, as_episodic, Capability::Episodic)
            .segments_pending_summary(limit)
            .await
            .map_err(|error| into_bus_error(&error))
    }
}

/// The response-size ceiling for a method that returns a list of entries.
///
/// A `TinyBus` frame is JSON capped at 16 MiB. 8 MiB of raw entry content leaves
/// room for the JSON structure around it and for escaping, which can double a
/// pathological string, so a response that passes this check fits with margin.
pub(crate) const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// Refuse a response that would not fit in a frame.
///
/// # Why a refusal and not a truncation
///
/// Truncating would be worse than failing. `List` has no cursor, so a caller
/// receiving a short list has no way to tell it apart from a complete one and no
/// way to ask for the rest — it would conclude those entries do not exist. A
/// named error tells the caller to narrow by namespace, category or session,
/// which is a query it can actually issue.
///
/// # Why `BudgetExceeded` and not a new name
///
/// The name has to be one both ends already agree on, and
/// [`tinymemory_api::wire`] is the table that makes that true. `BudgetExceeded`
/// is what it means — the result exceeded a size budget — and it round-trips to
/// the host as `MemoryError::BudgetExceeded` with no client change. A new name
/// would decode to `Other` on any host older than the module, turning an
/// actionable "narrow your query" into an opaque backend failure.
///
/// # Errors
///
/// [`wire::BUDGET_EXCEEDED`], when the estimate exceeds [`MAX_RESPONSE_BYTES`].
/// The message names the method and the sizes, never entry content.
fn ensure_response_fits<T: serde::Serialize>(response: &T, method: &str) -> BusResult<()> {
    let estimate = serde_json::to_vec(response)
        .map_err(|error| BusError::Protocol(error.to_string()))?
        .len();

    if estimate > MAX_RESPONSE_BYTES {
        log::warn!(
            "[tinymemory:module] {method} refused: response estimated at {estimate} bytes \
             exceeds the {MAX_RESPONSE_BYTES} byte response ceiling"
        );
        return Err(BusError::MethodFailed {
            name: wire::BUDGET_EXCEEDED.to_string(),
            message: format!(
                "{method} would return ~{estimate} bytes, over the \
                 {MAX_RESPONSE_BYTES} byte response ceiling; narrow the query by \
                 namespace, category or session"
            ),
        });
    }
    Ok(())
}

/// Map a [`MemoryError`] onto a named bus error.
///
/// Both the name and the message come from [`tinymemory_api::wire`], which the
/// host's client also uses to map them back. Deriving them here instead would
/// give the contract two definitions free to drift — and the drift that matters
/// is silent: a `PathEscape` arriving as an `Invalid` reclassifies a sandbox
/// escape as a caller mistake.
fn into_bus_error(error: &MemoryError) -> BusError {
    BusError::MethodFailed {
        name: wire::wire_name(error).to_string(),
        message: wire::wire_message(error),
    }
}

/// Serve the memory object and claim the well-known name.
pub(crate) async fn serve(
    connection: &Connection,
    provider: Arc<dyn MemoryProvider>,
    config: crate::config::ModuleConfig,
) -> BusResult<()> {
    let opener = Arc::new(StoreOpener::new(connection.clone(), config));
    connection
        .serve_at(
            OBJECT_PATH.try_into()?,
            MemoryService::root(provider, opener),
        )
        .await?;
    connection.request_name(BUS_NAME).await?;
    Ok(())
}

#[cfg(test)]
#[path = "test.rs"]
mod test;
