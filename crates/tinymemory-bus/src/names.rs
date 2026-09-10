//! The object this contract addresses, and every member name on it.
//!
//! A member name is what actually travels in a frame, so it is the part of
//! the contract a typo breaks at runtime rather than at compile time. The
//! constants here exist so neither end spells one by hand.
//!
//! The names are the `PascalCase` of the module's method identifiers, which is
//! what `#[tinybus::interface]` derives them from. [`METHODS`] lists all of
//! them; the module asserts its served members against it, so a method added
//! there without a constant here fails that crate's tests.

/// Well-known bus name exported by the `TinyMemory` module.
pub const BUS_NAME: &str = "ai.tinyhumans.tinymemory.Memory";

/// Object path the interface is served at.
///
/// `OpenStore` returns a *different* path — a sibling store under the same
/// workspace, exporting this identical interface. Treat this constant as the
/// root object, not as the only one.
pub const OBJECT_PATH: &str = "/ai/tinyhumans/tinymemory/Memory";

/// One constant per member name on [`BUS_NAME`].
pub mod methods {
    // Driver identity, capability negotiation, health and store opening.
    /// `DriverId` — driver id.
    pub const DRIVER_ID: &str = "DriverId";
    /// `Capabilities` — capabilities.
    pub const CAPABILITIES: &str = "Capabilities";
    /// `Health` — health.
    pub const HEALTH: &str = "Health";
    /// `Shutdown` — shutdown.
    pub const SHUTDOWN: &str = "Shutdown";
    /// `OpenStore` — open store.
    pub const OPEN_STORE: &str = "OpenStore";

    // The mandatory key/value surface every driver implements.
    /// `Store` — store.
    pub const STORE: &str = "Store";
    /// `Get` — get.
    pub const GET: &str = "Get";
    /// `Forget` — forget.
    pub const FORGET: &str = "Forget";
    /// `List` — list.
    pub const LIST: &str = "List";
    /// `Namespaces` — namespaces.
    pub const NAMESPACES: &str = "Namespaces";

    // Semantic recall over stored entries.
    /// `Recall` — recall.
    pub const RECALL: &str = "Recall";
    /// `RecallNamespaceScored` — recall namespace scored.
    pub const RECALL_NAMESPACE_SCORED: &str = "RecallNamespaceScored";

    // Paged export and bulk import of raw records.
    /// `ExportPage` — export page.
    pub const EXPORT_PAGE: &str = "ExportPage";
    /// `ImportRecords` — import records.
    pub const IMPORT_RECORDS: &str = "ImportRecords";

    // Document, chat and mail ingestion through the summary pipeline.
    /// `IngestDocument` — ingest document.
    pub const INGEST_DOCUMENT: &str = "IngestDocument";
    /// `IngestChat` — ingest chat.
    pub const INGEST_CHAT: &str = "IngestChat";
    /// `IngestEmail` — ingest email.
    pub const INGEST_EMAIL: &str = "IngestEmail";
    /// `IngestLearning` — ingest one learning candidate.
    pub const INGEST_LEARNING: &str = "IngestLearning";
    /// `IngestEvent` — ingest one raw event.
    pub const INGEST_EVENT: &str = "IngestEvent";
    /// `Answer` — synthesize a grounded answer.
    pub const ANSWER: &str = "Answer";

    // Namespace-scoped document storage and retrieval.
    /// `PutDocument` — put document.
    pub const PUT_DOCUMENT: &str = "PutDocument";
    /// `GetDocument` — get document.
    pub const GET_DOCUMENT: &str = "GetDocument";
    /// `ListDocuments` — list documents.
    pub const LIST_DOCUMENTS: &str = "ListDocuments";
    /// `ListNamespaces` — list namespaces.
    pub const LIST_NAMESPACES: &str = "ListNamespaces";
    /// `DeleteDocument` — delete document.
    pub const DELETE_DOCUMENT: &str = "DeleteDocument";
    /// `ClearNamespace` — clear namespace.
    pub const CLEAR_NAMESPACE: &str = "ClearNamespace";
    /// `QueryDocuments` — query documents.
    pub const QUERY_DOCUMENTS: &str = "QueryDocuments";
    /// `RecallDocuments` — recall documents.
    pub const RECALL_DOCUMENTS: &str = "RecallDocuments";

    // The markdown summary tree: append, query, drill down, seal, cascade.
    /// `Append` — append.
    pub const APPEND: &str = "Append";
    /// `QuerySource` — query source.
    pub const QUERY_SOURCE: &str = "QuerySource";
    /// `DrillDown` — drill down.
    pub const DRILL_DOWN: &str = "DrillDown";
    /// `Seal` — seal.
    pub const SEAL: &str = "Seal";
    /// `Cascade` — cascade.
    pub const CASCADE: &str = "Cascade";
    /// `SummaryForest` — every sealed summary in the store, with its tree.
    pub const SUMMARY_FOREST: &str = "SummaryForest";
    /// `RecentLeaves` — the newest leaves and the summaries that sealed them.
    pub const RECENT_LEAVES: &str = "RecentLeaves";
    /// `Summarise` — fold summary inputs into one parent summary.
    pub const SUMMARISE: &str = "Summarise";
    /// `RootSummaries` — every namespace's root summary, capped.
    pub const ROOT_SUMMARIES: &str = "RootSummaries";

    // The runtime (markdown time) tree, node by node. `Seal` and `Cascade`
    // above run whole passes and answer with tree state; these are the doors
    // under the host's tree-summarizer RPC surface — the buffered write that
    // reports where it landed, the two structural reads and the status read
    // unbundled from `DrillDown`, and the two provider-backed passes with the
    // shapes the RPC replies actually carry.
    /// `RuntimeBufferWrite` — buffer raw content for the time tree, answering
    /// with the path it landed at.
    pub const RUNTIME_BUFFER_WRITE: &str = "RuntimeBufferWrite";
    /// `RuntimeReadNode` — one time-tree node, or none.
    pub const RUNTIME_READ_NODE: &str = "RuntimeReadNode";
    /// `RuntimeReadChildren` — a time-tree node's direct children.
    pub const RUNTIME_READ_CHILDREN: &str = "RuntimeReadChildren";
    /// `RuntimeTreeStatus` — one namespace's time-tree shape and coverage.
    pub const RUNTIME_TREE_STATUS: &str = "RuntimeTreeStatus";
    /// `RuntimeSummarize` — drain the buffer into the tree on the driver's
    /// provider, answering with the last hour node it wrote.
    pub const RUNTIME_SUMMARIZE: &str = "RuntimeSummarize";
    /// `RuntimeRebuild` — rebuild the whole time tree from its hour leaves.
    pub const RUNTIME_REBUILD: &str = "RuntimeRebuild";
    /// `FlavourProfile` — the compiled flavoured-root profile for one scope.
    pub const FLAVOUR_PROFILE: &str = "FlavourProfile";

    // Entities, relations and the namespaced key/value store.
    /// `Entities` — entities.
    pub const ENTITIES: &str = "Entities";
    /// `EntityEdges` — entity edges.
    pub const ENTITY_EDGES: &str = "EntityEdges";
    /// `TouchEntities` — touch entities.
    pub const TOUCH_ENTITIES: &str = "TouchEntities";
    /// `SearchEntities` — search entities.
    pub const SEARCH_ENTITIES: &str = "SearchEntities";
    /// `TopEntities` — the store-wide entity index, most-observed first.
    pub const TOP_ENTITIES: &str = "TopEntities";
    /// `ChunkEntities` — every entity indexed against one chunk.
    pub const CHUNK_ENTITIES: &str = "ChunkEntities";
    /// `EntityChunkIds` — the chunks one entity was observed in.
    pub const ENTITY_CHUNK_IDS: &str = "EntityChunkIds";
    /// `Relations` — relations.
    pub const RELATIONS: &str = "Relations";
    /// `PutRelation` — put relation.
    pub const PUT_RELATION: &str = "PutRelation";
    /// `KvGet` — kv get.
    pub const KV_GET: &str = "KvGet";
    /// `KvPut` — kv put.
    pub const KV_PUT: &str = "KvPut";
    /// `KvDelete` — kv delete.
    pub const KV_DELETE: &str = "KvDelete";
    /// `KvList` — kv list.
    pub const KV_LIST: &str = "KvList";

    // Source snapshots, diffs, item acceptance and forgetting.
    /// `CaptureSnapshot` — capture snapshot.
    pub const CAPTURE_SNAPSHOT: &str = "CaptureSnapshot";
    /// `Snapshots` — snapshots.
    pub const SNAPSHOTS: &str = "Snapshots";
    /// `Diff` — diff.
    pub const DIFF: &str = "Diff";
    /// `AcceptSourceItems` — accept source items.
    pub const ACCEPT_SOURCE_ITEMS: &str = "AcceptSourceItems";
    /// `ForgetSource` — forget source.
    pub const FORGET_SOURCE: &str = "ForgetSource";
    /// `ForgetMatching` — forget everything one selector names.
    pub const FORGET_MATCHING: &str = "ForgetMatching";

    // The long-term goals document.
    /// `Goals` — goals.
    pub const GOALS: &str = "Goals";
    /// `SetGoals` — set goals.
    pub const SET_GOALS: &str = "SetGoals";

    // Tool-scoped memory rules.
    /// `ToolRules` — tool rules.
    pub const TOOL_RULES: &str = "ToolRules";
    /// `PutToolRule` — put tool rule.
    pub const PUT_TOOL_RULE: &str = "PutToolRule";
    /// `DeleteToolRule` — delete tool rule.
    pub const DELETE_TOOL_RULE: &str = "DeleteToolRule";

    // Re-embedding, compaction, consolidation and diagnosis.
    /// `Reembed` — reembed.
    pub const REEMBED: &str = "Reembed";
    /// `Compact` — compact.
    pub const COMPACT: &str = "Compact";
    /// `Consolidate` — consolidate.
    pub const CONSOLIDATE: &str = "Consolidate";
    /// `Doctor` — doctor.
    pub const DOCTOR: &str = "Doctor";
    /// `RetryFailed` — give terminally-failed queue work another attempt.
    pub const RETRY_FAILED: &str = "RetryFailed";
    /// `StoreStats` — aggregate counts over what the driver has stored.
    pub const STORE_STATS: &str = "StoreStats";
    /// `QueueStats` — the ingest and re-embed queue's state.
    pub const QUEUE_STATS: &str = "QueueStats";
    /// `LatestQueueFailure` — the most recent terminal queue failure.
    pub const LATEST_QUEUE_FAILURE: &str = "LatestQueueFailure";
    /// `BackfillInProgress` — whether a re-embedding backfill is still running
    /// anywhere in the driver's process.
    pub const BACKFILL_IN_PROGRESS: &str = "BackfillInProgress";
    /// `RecallNamespaceRecent` — namespace recall ordered by recency, no query.
    pub const RECALL_NAMESPACE_RECENT: &str = "RecallNamespaceRecent";
    /// `FlushPending` — flush buffered work old enough to be written out.
    pub const FLUSH_PENDING: &str = "FlushPending";
    /// `BackfillConnectorTrees` — file already-stored connector documents into
    /// the memory tree, for records synced before the routing fix
    /// (openhuman#6007) reached them.
    pub const BACKFILL_CONNECTOR_TREES: &str = "BackfillConnectorTrees";
    /// `ResetDerivedIndex` — drop derived state and schedule its rebuild.
    pub const RESET_DERIVED_INDEX: &str = "ResetDerivedIndex";
    /// `PurgeAll` — erase every row the driver holds.
    pub const PURGE_ALL: &str = "PurgeAll";

    // The people store: ranking, handles, scores and interactions.
    /// `ListPeople` — list people.
    pub const LIST_PEOPLE: &str = "ListPeople";
    /// `GetPerson` — get person.
    pub const GET_PERSON: &str = "GetPerson";
    /// `ResolveHandle` — resolve handle.
    pub const RESOLVE_HANDLE: &str = "ResolveHandle";
    /// `AddHandleAlias` — add handle alias.
    pub const ADD_HANDLE_ALIAS: &str = "AddHandleAlias";
    /// `ScorePerson` — score person.
    pub const SCORE_PERSON: &str = "ScorePerson";
    /// `RecordInteraction` — record interaction.
    pub const RECORD_INTERACTION: &str = "RecordInteraction";
    /// `SeedFromAddressBook` — seed from address book.
    pub const SEED_FROM_ADDRESS_BOOK: &str = "SeedFromAddressBook";

    // The persisted chunk model and its embeddings.
    /// `ListChunks` — list chunks.
    pub const LIST_CHUNKS: &str = "ListChunks";
    /// `GetChunk` — get chunk.
    pub const GET_CHUNK: &str = "GetChunk";
    /// `ChunkDetail` — chunk detail.
    pub const CHUNK_DETAIL: &str = "ChunkDetail";
    /// `StorageKinds` — storage kinds.
    pub const STORAGE_KINDS: &str = "StorageKinds";
    /// `ChunkEmbeddings` — chunk embeddings.
    pub const CHUNK_EMBEDDINGS: &str = "ChunkEmbeddings";
    /// `CountChunks` — how many chunks `ListChunks` matches, page bounds
    /// ignored.
    pub const COUNT_CHUNKS: &str = "CountChunks";
    /// `ListChunkDetails` — the metadata `ChunkDetail` returns, for a whole
    /// page at once.
    pub const LIST_CHUNK_DETAILS: &str = "ListChunkDetails";
    /// `SourceTotals` — one row per source, with what it contributed.
    pub const SOURCE_TOTALS: &str = "SourceTotals";
    /// `ChunkScore` — one chunk's admission decision and the signals behind it.
    pub const CHUNK_SCORE: &str = "ChunkScore";
    /// `SourceIngestStatus` — per configured source, how far ingest has got.
    pub const SOURCE_INGEST_STATUS: &str = "SourceIngestStatus";

    // The scored retrieval surface.
    /// `FastRetrieve` — fast retrieve.
    pub const FAST_RETRIEVE: &str = "FastRetrieve";
    /// `CoverWindow` — cover window.
    pub const COVER_WINDOW: &str = "CoverWindow";
    /// `RetrieveSource` — retrieve source.
    pub const RETRIEVE_SOURCE: &str = "RetrieveSource";
    /// `RetrieveChildren` — retrieve children.
    pub const RETRIEVE_CHILDREN: &str = "RetrieveChildren";
    /// `RetrieveLeaves` — retrieve leaves.
    pub const RETRIEVE_LEAVES: &str = "RetrieveLeaves";

    // Profile facets and their provenance.
    /// `ListActiveFacets` — list active facets.
    pub const LIST_ACTIVE_FACETS: &str = "ListActiveFacets";
    /// `ListAllFacets` — list all facets.
    pub const LIST_ALL_FACETS: &str = "ListAllFacets";
    /// `GetFacet` — get facet.
    pub const GET_FACET: &str = "GetFacet";
    /// `FacetsByType` — facets by type.
    pub const FACETS_BY_TYPE: &str = "FacetsByType";
    /// `UpsertFacet` — upsert facet.
    pub const UPSERT_FACET: &str = "UpsertFacet";
    /// `UpsertProviderFacet` — upsert provider facet.
    pub const UPSERT_PROVIDER_FACET: &str = "UpsertProviderFacet";
    /// `SetFacetUserState` — set facet user state.
    pub const SET_FACET_USER_STATE: &str = "SetFacetUserState";
    /// `DeleteFacet` — delete facet.
    pub const DELETE_FACET: &str = "DeleteFacet";
    /// `DeleteFacetById` — delete facet by id.
    pub const DELETE_FACET_BY_ID: &str = "DeleteFacetById";
    /// `DropFacetsBelow` — drop facets below.
    pub const DROP_FACETS_BELOW: &str = "DropFacetsBelow";
    /// `WorkflowIdentityMatches` — workflow identity matches.
    pub const WORKFLOW_IDENTITY_MATCHES: &str = "WorkflowIdentityMatches";

    // Episodic turns and conversation segments.
    /// `InsertTurn` — insert turn.
    pub const INSERT_TURN: &str = "InsertTurn";
    /// `SessionTurns` — session turns.
    pub const SESSION_TURNS: &str = "SessionTurns";
    /// `OpenSegment` — open segment.
    pub const OPEN_SEGMENT: &str = "OpenSegment";
    /// `CreateSegment` — create segment.
    pub const CREATE_SEGMENT: &str = "CreateSegment";
    /// `AppendTurn` — append turn.
    pub const APPEND_TURN: &str = "AppendTurn";
    /// `CloseSegment` — close segment.
    pub const CLOSE_SEGMENT: &str = "CloseSegment";
    /// `SetSegmentSummary` — set segment summary.
    pub const SET_SEGMENT_SUMMARY: &str = "SetSegmentSummary";
    /// `SegmentsPendingSummary` — closed segments with no summary yet.
    pub const SEGMENTS_PENDING_SUMMARY: &str = "SegmentsPendingSummary";
    /// `UpsertSegmentEmbedding` — upsert segment embedding.
    pub const UPSERT_SEGMENT_EMBEDDING: &str = "UpsertSegmentEmbedding";
    /// `InsertEvent` — record one extracted event against its segment.
    pub const INSERT_EVENT: &str = "InsertEvent";

    // The summary tree's flush door, addressed by source scope rather than by
    // a tree handle.
    /// `FlushSourceTree` — seal and cascade one source's tree now.
    pub const FLUSH_SOURCE_TREE: &str = "FlushSourceTree";

    // The typed pipeline diagnosis, beside the maintenance family's uniform
    // report.
    /// `Diagnose` — the typed, per-stage pipeline diagnosis.
    pub const DIAGNOSE: &str = "Diagnose";
    /// `OverrideSchedulerGate` — open a bounded manual-override window on the
    /// scheduler gate, for user-initiated maintenance while paused.
    pub const OVERRIDE_SCHEDULER_GATE: &str = "OverrideSchedulerGate";
    /// `DegradedState` — the degradation flags alone, without a diagnosis.
    pub const DEGRADED_STATE: &str = "DegradedState";

    // Syncs the driver runs itself: the manual trigger, the persisted state,
    // and what past runs cost.
    /// `RunConnectionSync` — run one connection's sync now.
    pub const RUN_CONNECTION_SYNC: &str = "RunConnectionSync";

    /// `RunSourceSync` — run one configured memory source's sync now,
    /// whatever kind it is.
    pub const RUN_SOURCE_SYNC: &str = "RunSourceSync";
    /// `BootstrapConnection` — run one connection's first-time bootstrap.
    pub const BOOTSTRAP_CONNECTION: &str = "BootstrapConnection";
    /// `IsToolkitSyncable` — whether this driver has a pipeline for a toolkit.
    pub const IS_TOOLKIT_SYNCABLE: &str = "IsToolkitSyncable";
    /// `SourceSyncState` — the persisted cursor and budget for one connection.
    pub const SOURCE_SYNC_STATE: &str = "SourceSyncState";
    /// `SyncAuditLog` — past sync runs, newest first.
    pub const SYNC_AUDIT_LOG: &str = "SyncAuditLog";
    /// `EstimateSyncCostUsd` — price a token count at the driver's own rate.
    pub const ESTIMATE_SYNC_COST_USD: &str = "EstimateSyncCostUsd";
    /// `SyncStatuses` — per-provider progress, derived from stored content.
    pub const SYNC_STATUSES: &str = "SyncStatuses";
    /// `RawArchiveCoverage` — how much of a raw archive its tree covers.
    pub const RAW_ARCHIVE_COVERAGE: &str = "RawArchiveCoverage";
    /// `RebuildFromRawArchive` — re-derive a tree from its raw archive.
    pub const REBUILD_FROM_RAW_ARCHIVE: &str = "RebuildFromRawArchive";

    // The local coding-agent transcripts the driver distils.
    /// `CodingSessionStatus` — what each agent's session store holds.
    pub const CODING_SESSION_STATUS: &str = "CodingSessionStatus";
    /// `IngestCodingSessions` — distil coding sessions into observations.
    pub const INGEST_CODING_SESSIONS: &str = "IngestCodingSessions";

    // Scoring family — entity extraction and text embedding through the bus.
    /// `ExtractEntities` — extract canonical entity ids from a query string.
    pub const EXTRACT_ENTITIES: &str = "ExtractEntities";
    /// `EmbedText` — produce a dense embedding vector for an arbitrary string.
    pub const EMBED_TEXT: &str = "EmbedText";
    /// `EmbedderSlug` — the stable identifier of the active embedder.
    pub const EMBEDDER_SLUG: &str = "EmbedderSlug";
}

/// Every member name, in the order the module declares them.
///
/// The order matters: `tinybus`'s `Interface::members()` returns declaration
/// order, and the module compares the two sequences directly rather than as
/// sets, so a reordering is caught alongside an addition or a removal.
pub const METHODS: [&str; 144] = [
    methods::DRIVER_ID,
    methods::CAPABILITIES,
    methods::HEALTH,
    methods::SHUTDOWN,
    methods::OPEN_STORE,
    methods::STORE,
    methods::GET,
    methods::FORGET,
    methods::LIST,
    methods::NAMESPACES,
    methods::RECALL,
    methods::EXPORT_PAGE,
    methods::IMPORT_RECORDS,
    methods::INGEST_DOCUMENT,
    methods::INGEST_CHAT,
    methods::INGEST_EMAIL,
    methods::PUT_DOCUMENT,
    methods::GET_DOCUMENT,
    methods::LIST_DOCUMENTS,
    methods::LIST_NAMESPACES,
    methods::DELETE_DOCUMENT,
    methods::CLEAR_NAMESPACE,
    methods::QUERY_DOCUMENTS,
    methods::RECALL_DOCUMENTS,
    methods::APPEND,
    methods::QUERY_SOURCE,
    methods::DRILL_DOWN,
    methods::SEAL,
    methods::CASCADE,
    methods::ENTITIES,
    methods::ENTITY_EDGES,
    methods::TOUCH_ENTITIES,
    methods::KV_GET,
    methods::KV_PUT,
    methods::KV_DELETE,
    methods::KV_LIST,
    methods::RELATIONS,
    methods::PUT_RELATION,
    methods::CAPTURE_SNAPSHOT,
    methods::SNAPSHOTS,
    methods::DIFF,
    methods::GOALS,
    methods::SET_GOALS,
    methods::TOOL_RULES,
    methods::PUT_TOOL_RULE,
    methods::DELETE_TOOL_RULE,
    methods::ACCEPT_SOURCE_ITEMS,
    methods::FORGET_SOURCE,
    methods::REEMBED,
    methods::COMPACT,
    methods::CONSOLIDATE,
    methods::DOCTOR,
    methods::RETRY_FAILED,
    methods::STORE_STATS,
    methods::QUEUE_STATS,
    methods::LATEST_QUEUE_FAILURE,
    methods::BACKFILL_IN_PROGRESS,
    methods::FLUSH_PENDING,
    methods::RESET_DERIVED_INDEX,
    methods::RECALL_NAMESPACE_RECENT,
    methods::LIST_PEOPLE,
    methods::GET_PERSON,
    methods::RESOLVE_HANDLE,
    methods::ADD_HANDLE_ALIAS,
    methods::SCORE_PERSON,
    methods::RECORD_INTERACTION,
    methods::SEED_FROM_ADDRESS_BOOK,
    methods::LIST_CHUNKS,
    methods::GET_CHUNK,
    methods::CHUNK_DETAIL,
    methods::STORAGE_KINDS,
    methods::CHUNK_EMBEDDINGS,
    methods::FAST_RETRIEVE,
    methods::COVER_WINDOW,
    methods::LIST_ACTIVE_FACETS,
    methods::LIST_ALL_FACETS,
    methods::GET_FACET,
    methods::FACETS_BY_TYPE,
    methods::INSERT_TURN,
    methods::SESSION_TURNS,
    methods::OPEN_SEGMENT,
    methods::CREATE_SEGMENT,
    methods::APPEND_TURN,
    methods::CLOSE_SEGMENT,
    methods::SET_SEGMENT_SUMMARY,
    methods::UPSERT_SEGMENT_EMBEDDING,
    methods::INSERT_EVENT,
    methods::UPSERT_FACET,
    methods::UPSERT_PROVIDER_FACET,
    methods::SET_FACET_USER_STATE,
    methods::DELETE_FACET,
    methods::DELETE_FACET_BY_ID,
    methods::DROP_FACETS_BELOW,
    methods::WORKFLOW_IDENTITY_MATCHES,
    methods::RETRIEVE_SOURCE,
    methods::RETRIEVE_CHILDREN,
    methods::RETRIEVE_LEAVES,
    methods::RECALL_NAMESPACE_SCORED,
    methods::SEARCH_ENTITIES,
    methods::COUNT_CHUNKS,
    methods::TOP_ENTITIES,
    methods::CHUNK_ENTITIES,
    methods::ENTITY_CHUNK_IDS,
    methods::SUMMARY_FOREST,
    methods::RECENT_LEAVES,
    methods::LIST_CHUNK_DETAILS,
    methods::SOURCE_TOTALS,
    methods::FORGET_MATCHING,
    methods::PURGE_ALL,
    methods::FLUSH_SOURCE_TREE,
    methods::DIAGNOSE,
    methods::RUN_CONNECTION_SYNC,
    methods::RUN_SOURCE_SYNC,
    methods::BOOTSTRAP_CONNECTION,
    methods::IS_TOOLKIT_SYNCABLE,
    methods::SOURCE_SYNC_STATE,
    methods::SYNC_AUDIT_LOG,
    methods::ESTIMATE_SYNC_COST_USD,
    methods::SYNC_STATUSES,
    methods::RAW_ARCHIVE_COVERAGE,
    methods::REBUILD_FROM_RAW_ARCHIVE,
    methods::CODING_SESSION_STATUS,
    methods::INGEST_CODING_SESSIONS,
    methods::EXTRACT_ENTITIES,
    methods::EMBED_TEXT,
    methods::EMBEDDER_SLUG,
    methods::SUMMARISE,
    methods::ROOT_SUMMARIES,
    methods::DEGRADED_STATE,
    methods::CHUNK_SCORE,
    methods::SOURCE_INGEST_STATUS,
    methods::RUNTIME_BUFFER_WRITE,
    methods::RUNTIME_READ_NODE,
    methods::RUNTIME_READ_CHILDREN,
    methods::RUNTIME_TREE_STATUS,
    methods::RUNTIME_SUMMARIZE,
    methods::RUNTIME_REBUILD,
    methods::FLAVOUR_PROFILE,
    methods::INGEST_LEARNING,
    methods::INGEST_EVENT,
    methods::ANSWER,
    methods::OVERRIDE_SCHEDULER_GATE,
    // Connector-backfill round (openhuman#6012): appended at the tail, per
    // this table's append-only rule — filing it beside `FlushPending`, where
    // its family lives, would renumber every member after it and invoke the
    // wrong method on a host built against an earlier release.
    methods::BACKFILL_CONNECTOR_TREES,
    // Re-summarisation round (openhuman#6186): appended at the tail for the
    // same reason as the round above it. Its family sits at slots 55-63; filed
    // there it would have renumbered all eighty members after it.
    methods::SEGMENTS_PENDING_SUMMARY,
];

#[cfg(test)]
#[path = "names_tests.rs"]
mod tests;
