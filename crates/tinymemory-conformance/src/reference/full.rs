// ported from openhuman src/openhuman/memory/guard/test_support_part_0{1,2,3}.rs
use std::sync::Mutex;
use tinymemory_api::provider::operations::{
    MemoryAnswer, MemoryConversationIngest, MemoryDocumentIngest, MemoryEventIngest,
    MemoryLearningIngest,
};

use async_trait::async_trait;
use tinymemory_api::capabilities::Capabilities;
use tinymemory_api::chunks::Chunk;
use tinymemory_api::error::MemoryError;
use tinymemory_api::goals::GoalsDoc;
use tinymemory_api::health::MemoryHealth;
use tinymemory_api::provider::sessions::{
    CodingSessionIngestReport, CodingSessionIngestRequest, CodingSessionSource,
};
use tinymemory_api::provider::sync::{
    RawArchiveCoverage, RawRebuildOutcome, SourceSyncState, SourceSyncStatus, SyncAuditEntry,
    SyncRunOutcome,
};
use tinymemory_api::provider::types::{
    DiffReport, EntityHit, ExportPage, ExportRecord, ImportOutcome, IngestItem, IngestOutcome,
    MaintenanceReport, SnapshotRef, SourceItem, SourceScope,
};
use tinymemory_api::provider::{
    AddressBookSeedOutcome, ChunkDetail, ChunkEmbedding, ChunkQuery, CoverWindowQuery, EntityMatch,
    EpisodicEvent, FacetType, FastRetrieveQuery, MemoryChunks, MemoryCodingSessions, MemoryCore,
    MemoryDiff, MemoryDocuments, MemoryEntities, MemoryEpisodic, MemoryGoals, MemoryGraph,
    MemoryIngest, MemoryMaintenance, MemoryPeople, MemoryPortability, MemoryProfile,
    MemoryProvider, MemoryRecall, MemoryRetrieval, MemoryScoring, MemorySourceSink,
    MemorySourceSync, MemoryToolMemory, MemoryTree, PersonHandle, PersonInteraction, PersonRecord,
    PersonScore, ProfileFacet, RankedPerson, ResolvedPerson, RetrievalHit, RetrievalResponse,
    SourceRetrievalQuery, UserState,
};
use tinymemory_api::recall::OwnedRecallOpts;
use tinymemory_api::tool_memory::ToolMemoryRule;
use tinymemory_api::tree::{IngestRequest, QueryResult, TreeStatus};
use tinymemory_api::types::{
    GraphRelationRecord, MemoryCategory, MemoryEntry, MemoryItemKind, MemoryKvRecord, MemoryTaint,
    NamespaceDocumentInput, NamespaceMemoryHit, NamespaceRetrievalContext, NamespaceSummary,
    RetrievalScoreBreakdown, StoredMemoryDocument,
};

/// A relation's upsert key: its namespace and the triple it asserts.
type RelationKey = (Option<String>, String, String, String);

/// Relations held by [`RecordingProvider`], keyed by [`RelationKey`].
type RelationRows = std::collections::HashMap<RelationKey, GraphRelationRecord>;

/// The driver id [`RecordingProvider`] binds under.
pub const FULL_DRIVER_ID: &str = "recording";

/// Locks a fake's state, recovering from a poisoned mutex rather than failing.
///
/// A poisoned lock means an earlier caller panicked while holding it. In a
/// storage engine that is a reason to refuse the call, and the reference driver
/// does exactly that. Here it is not: this driver's state is a call log and a
/// couple of maps, a panicking test has already failed, and turning its
/// neighbour's lock into a second, unrelated failure only obscures which test
/// broke. `into_inner` keeps the first failure the only one.
fn lock<T>(cell: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    cell.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// One call that reached the driver.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    /// The family-qualified method name, e.g. `chunks.list_chunks`.
    pub method: String,
    /// Content the driver was handed, when the method carries any.
    pub content: Option<String>,
    /// Provenance the driver was handed, when the method carries any.
    pub taint: Option<MemoryTaint>,
    /// Whether the method received a `Some(scope)`.
    pub scoped: Option<bool>,
}

/// The scope's allow list rendered for assertions, sorted for determinism.
fn rendered_scope(scope: Option<&SourceScope>) -> Option<String> {
    scope.map(|s| {
        let mut allow = s.allow.clone();
        allow.sort();
        allow.join(",")
    })
}

impl Call {
    fn plain(method: &str) -> Self {
        Self {
            method: method.into(),
            content: None,
            taint: None,
            scoped: None,
        }
    }
}

/// The entity kinds [`MemoryRetrieval::search_entities`] accepts in its filter,
/// as enumerated by the contract's own module docs.
///
/// Request-side only. `EntityMatch::kind` in a *response* is an open
/// vocabulary and must never be checked against this list.
const KNOWN_ENTITY_KINDS: &[&str] = &[
    "email",
    "url",
    "handle",
    "hashtag",
    "person",
    "organization",
    "location",
    "event",
    "product",
    "datetime",
    "technology",
    "artifact",
    "quantity",
    "misc",
    "topic",
];

/// A provider that records and answers with empties.
pub struct RecordingProvider {
    calls: Mutex<Vec<Call>>,
    /// What `recall` returns, so budget tests can drive a known result set.
    recall_result: Mutex<Vec<MemoryEntry>>,
    /// What `fast_retrieve` returns, so the auto-recall lane can be driven
    /// through a real guard with known hits.
    fast_retrieve_result: Mutex<RetrievalResponse>,
    /// What `recall_namespace_scored` returns, so the vector-floored recall
    /// paths (Lane B, the contradiction check) can be driven with known scores.
    namespace_hits: Mutex<Vec<NamespaceMemoryHit>>,
    /// What `namespaces` returns, so a namespace can look populated (Lane B
    /// asks for the count before it pays for an embed) without a real store.
    namespace_summaries: Mutex<Vec<NamespaceSummary>>,
    /// Documents written through [`MemoryDocuments::put_document`], keyed the
    /// way the contract upserts them.
    documents: Mutex<std::collections::HashMap<(String, String), StoredMemoryDocument>>,
    /// Rows written through [`MemoryGraph::kv_put`].
    kv: Mutex<std::collections::HashMap<(Option<String>, String), MemoryKvRecord>>,
    /// Relations written through [`MemoryGraph::put_relation`], keyed on the
    /// triple the contract upserts on.
    relations: Mutex<RelationRows>,
    /// Rules written through [`MemoryToolMemory::put_tool_rule`], keyed by id.
    tool_rules: Mutex<std::collections::HashMap<String, ToolMemoryRule>>,
    /// The document written through [`MemoryGoals::set_goals`].
    goals: Mutex<Option<GoalsDoc>>,
    /// Entries written through [`MemoryCore::store`], keyed the way the
    /// contract upserts them.
    ///
    /// Without this the driver accepted writes and discarded them, which the
    /// suite treats as a legitimate `/dev/null` binding — so `retains_writes`
    /// probed false and `assert_provider` skipped every storage assertion. It
    /// passed, vacuously. See `the_full_driver_retains_writes`.
    entries: Mutex<std::collections::HashMap<(String, String), MemoryEntry>>,
}

impl Default for RecordingProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl RecordingProvider {
    /// Builds a driver with an empty call log and empty canned answers.
    #[must_use]
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            recall_result: Mutex::new(Vec::new()),
            fast_retrieve_result: Mutex::new(RetrievalResponse::default()),
            namespace_hits: Mutex::new(Vec::new()),
            namespace_summaries: Mutex::new(Vec::new()),
            documents: Mutex::new(std::collections::HashMap::new()),
            kv: Mutex::new(std::collections::HashMap::new()),
            relations: Mutex::new(std::collections::HashMap::new()),
            tool_rules: Mutex::new(std::collections::HashMap::new()),
            goals: Mutex::new(None),
            entries: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Sets what [`MemoryRecall::recall`] returns.
    #[must_use]
    pub fn with_recall_result(self, entries: Vec<MemoryEntry>) -> Self {
        *lock(&self.recall_result) = entries;
        self
    }

    /// Sets what [`MemoryRetrieval::fast_retrieve`] returns.
    #[must_use]
    pub fn with_fast_retrieve_result(self, response: RetrievalResponse) -> Self {
        *lock(&self.fast_retrieve_result) = response;
        self
    }

    /// Sets what [`MemoryRetrieval::recall_namespace_scored`] returns.
    #[must_use]
    pub fn with_namespace_hits(self, hits: Vec<NamespaceMemoryHit>) -> Self {
        *lock(&self.namespace_hits) = hits;
        self
    }

    /// Sets what [`MemoryCore::namespaces`] returns.
    #[must_use]
    pub fn with_namespace_summaries(self, summaries: Vec<NamespaceSummary>) -> Self {
        *lock(&self.namespace_summaries) = summaries;
        self
    }

    fn record(&self, call: Call) {
        lock(&self.calls).push(call);
    }

    /// Every call this driver has been handed, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<Call> {
        lock(&self.calls).clone()
    }

    /// How many calls this driver has been handed.
    #[must_use]
    pub fn call_count(&self) -> usize {
        lock(&self.calls).len()
    }

    /// The single recorded call, panicking when there is not exactly one.
    pub fn only_call(&self) -> Call {
        let mut calls = self.calls();
        assert_eq!(
            calls.len(),
            1,
            "expected exactly one driver call: {calls:?}"
        );
        calls.remove(0)
    }
}

/// An [`ExportRecord`] fixture.
pub fn export_record(taint: MemoryTaint) -> ExportRecord {
    ExportRecord {
        kind: "entry".into(),
        id: "r1".into(),
        namespace: Some("ns".into()),
        taint,
        payload: serde_json::Value::Null,
    }
}

/// A [`MemoryEntry`] fixture.
pub fn entry(content: &str) -> MemoryEntry {
    MemoryEntry {
        id: "id".into(),
        key: "key".into(),
        content: content.into(),
        namespace: Some("ns".into()),
        category: MemoryCategory::Core,
        timestamp: "2026-01-01T00:00:00Z".into(),
        session_id: None,
        score: None,
        taint: MemoryTaint::Internal,
    }
}

/// A [`TreeStatus`] fixture.
fn tree_status(namespace: &str) -> TreeStatus {
    TreeStatus {
        namespace: namespace.to_string(),
        total_nodes: 0,
        depth: 0,
        oldest_entry: None,
        newest_entry: None,
        last_run_at: None,
    }
}

/// A [`NamespaceDocumentInput`] fixture.
pub fn document(content: &str, taint: MemoryTaint) -> NamespaceDocumentInput {
    NamespaceDocumentInput {
        namespace: "ns".into(),
        key: "k".into(),
        title: "t".into(),
        content: content.into(),
        source_type: "chat".into(),
        priority: "normal".into(),
        tags: vec![],
        metadata: serde_json::Value::Null,
        category: "core".into(),
        session_id: None,
        document_id: None,
        taint,
    }
}

#[async_trait]
impl MemoryCore for RecordingProvider {
    async fn store(
        &self,
        namespace: &str,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        taint: MemoryTaint,
    ) -> Result<(), MemoryError> {
        self.record(Call {
            method: "core.store".into(),
            content: Some(content.to_string()),
            taint: Some(taint),
            scoped: None,
        });
        lock(&self.entries).insert(
            (namespace.to_string(), key.to_string()),
            MemoryEntry {
                id: format!("{namespace}::{key}"),
                key: key.to_string(),
                content: content.to_string(),
                namespace: Some(namespace.to_string()),
                category,
                timestamp: "1970-01-01T00:00:00Z".to_string(),
                session_id: session_id.map(str::to_owned),
                score: None,
                // Persisted as given. A driver that re-stamped this would
                // launder external content into internal-trust content, which
                // is the failure the parameter exists to prevent.
                taint,
            },
        );
        Ok(())
    }

    async fn get(&self, namespace: &str, key: &str) -> Result<Option<MemoryEntry>, MemoryError> {
        self.record(Call::plain("core.get"));
        Ok(lock(&self.entries)
            .get(&(namespace.to_string(), key.to_string()))
            .cloned())
    }

    async fn forget(&self, namespace: &str, key: &str) -> Result<bool, MemoryError> {
        self.record(Call::plain("core.forget"));
        Ok(lock(&self.entries)
            .remove(&(namespace.to_string(), key.to_string()))
            .is_some())
    }

    // Namespace, category and session are the contract's own isolation rules
    // rather than query semantics, so they are applied. Nothing else is: this
    // driver does not rank, score or search.
    async fn list(
        &self,
        namespace: Option<&str>,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>, MemoryError> {
        self.record(Call::plain("core.list"));
        let mut rows: Vec<MemoryEntry> = lock(&self.entries)
            .values()
            .filter(|e| namespace.is_none_or(|ns| e.namespace.as_deref() == Some(ns)))
            .filter(|e| category.is_none_or(|c| &e.category == c))
            .filter(|e| session_id.is_none_or(|s| e.session_id.as_deref() == Some(s)))
            .cloned()
            .collect();
        rows.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(rows)
    }

    // The canned answer wins when a caller set one — several host tests drive a
    // known namespace count without writing rows. Otherwise it is derived, so a
    // driver that stored something never reports an empty workspace.
    async fn namespaces(&self) -> Result<Vec<NamespaceSummary>, MemoryError> {
        self.record(Call::plain("core.namespaces"));
        let canned = lock(&self.namespace_summaries).clone();
        if !canned.is_empty() {
            return Ok(canned);
        }
        let mut counts: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for entry in lock(&self.entries).values() {
            if let Some(ns) = entry.namespace.as_deref() {
                *counts.entry(ns.to_string()).or_default() += 1;
            }
        }
        Ok(counts
            .into_iter()
            .map(|(namespace, count)| NamespaceSummary {
                namespace,
                count,
                last_updated: None,
            })
            .collect())
    }
}

#[async_trait]
impl MemoryRecall for RecordingProvider {
    async fn recall(
        &self,
        query: &str,
        _limit: usize,
        _opts: &OwnedRecallOpts,
        scope: Option<&SourceScope>,
    ) -> Result<Vec<MemoryEntry>, MemoryError> {
        self.record(Call {
            method: "recall.recall".into(),
            content: Some(query.to_string()),
            taint: None,
            scoped: Some(scope.is_some()),
        });
        // The canned answer wins when a caller set one — budget and auto-recall
        // tests drive a known result set without writing rows.
        let canned = lock(&self.recall_result).clone();
        if !canned.is_empty() {
            return Ok(canned);
        }
        // Otherwise recall what was stored. This is a case-insensitive
        // substring match over content, exactly as `InMemoryProvider` does and
        // for the same stated reason: it is enough for "did the write land and
        // come back", and deliberately **not** enough to test ranking. A test
        // about ordering wants a real engine.
        if scope.is_some_and(SourceScope::is_empty) {
            return Ok(Vec::new());
        }
        let needle = query.to_lowercase();
        // Namespace, category and session are the same isolation rules `list`
        // applies, and recall has to apply them too: a caller that narrowed a
        // recall by category and got rows from another one has been told
        // something false about its own store. `min_score` is deliberately not
        // honoured — that is ranking, and this driver does not rank.
        let mut hits: Vec<MemoryEntry> = lock(&self.entries)
            .values()
            .filter(|e| {
                _opts
                    .namespace
                    .as_deref()
                    .is_none_or(|ns| e.namespace.as_deref() == Some(ns))
            })
            .filter(|e| _opts.category.as_ref().is_none_or(|c| &e.category == c))
            .filter(|e| {
                _opts
                    .session_id
                    .as_deref()
                    .is_none_or(|s| e.session_id.as_deref() == Some(s))
            })
            .filter(|e| e.content.to_lowercase().contains(&needle))
            .cloned()
            .collect();
        hits.sort_by(|a, b| a.id.cmp(&b.id));
        hits.truncate(_limit);
        Ok(hits)
    }
}

#[async_trait]
impl MemoryPortability for RecordingProvider {
    // A cursor this driver never issued is refused rather than silently
    // restarting the export, which would duplicate rows for a caller paging
    // through. The fake issues no cursors at all, so *every* cursor is
    // unrecognised — which is exactly the state the contract's rule is about,
    // and the port arrived here answering an empty page instead.
    async fn export_page(
        &self,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<ExportPage, MemoryError> {
        self.record(Call::plain("portability.export_page"));
        let mut rows: Vec<MemoryEntry> = lock(&self.entries).values().cloned().collect();
        rows.sort_by(|a, b| a.id.cmp(&b.id));
        super::export_entries_page(&rows, cursor, limit)
    }

    async fn import_records(
        &self,
        records: Vec<ExportRecord>,
    ) -> Result<ImportOutcome, MemoryError> {
        self.record(Call {
            method: "portability.import_records".into(),
            content: None,
            taint: records.first().map(|r| r.taint),
            scoped: None,
        });
        let mut outcome = ImportOutcome::default();
        for record in &records {
            let Some((namespace, key, content, category, session_id)) =
                super::decode_export_record(record)
            else {
                // Per-record rejection is reported, not returned as an error: a
                // migration must not abort a whole restore over one bad row.
                outcome.failed += 1;
                outcome
                    .errors
                    .push(format!("record {} lacks a namespace or key", record.id));
                continue;
            };
            lock(&self.entries).insert(
                (namespace.clone(), key.clone()),
                MemoryEntry {
                    id: format!("{namespace}::{key}"),
                    key,
                    content,
                    namespace: Some(namespace),
                    category,
                    timestamp: "1970-01-01T00:00:00Z".to_string(),
                    session_id,
                    score: None,
                    // Carried from the record. Re-stamping on import is how a
                    // restore launders external content into internal trust.
                    taint: record.taint,
                },
            );
            outcome.imported += 1;
        }
        Ok(outcome)
    }
}

#[async_trait]
impl MemoryIngest for RecordingProvider {
    async fn ingest_document(&self, item: IngestItem) -> Result<IngestOutcome, MemoryError> {
        self.record(Call {
            method: "ingest.ingest_document".into(),
            content: Some(item.content),
            taint: Some(item.taint),
            scoped: None,
        });
        Ok(IngestOutcome::default())
    }

    async fn ingest_chat(&self, messages: Vec<IngestItem>) -> Result<IngestOutcome, MemoryError> {
        self.record(Call {
            method: "ingest.ingest_chat".into(),
            content: messages.first().map(|m| m.content.clone()),
            taint: messages.first().map(|m| m.taint),
            scoped: None,
        });
        Ok(IngestOutcome::default())
    }
}

#[async_trait]
impl MemoryDocuments for RecordingProvider {
    async fn put_document(&self, input: NamespaceDocumentInput) -> Result<String, MemoryError> {
        self.record(Call {
            method: "documents.put_document".into(),
            content: Some(input.content.clone()),
            taint: Some(input.taint),
            scoped: None,
        });
        let document_id = input.document_id.clone().unwrap_or_else(|| "doc".into());
        let now = tinymemory_api::chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
        let stored = StoredMemoryDocument {
            document_id: document_id.clone(),
            namespace: input.namespace.clone(),
            key: input.key.clone(),
            title: input.title,
            content: input.content,
            source_type: input.source_type,
            priority: input.priority,
            tags: input.tags,
            metadata: input.metadata,
            category: input.category,
            session_id: input.session_id,
            created_at: now,
            updated_at: now,
            markdown_rel_path: String::new(),
            taint: input.taint,
        };
        lock(&self.documents).insert((input.namespace, input.key), stored);
        Ok(document_id)
    }

    async fn get_document(
        &self,
        namespace: &str,
        key: &str,
    ) -> Result<Option<StoredMemoryDocument>, MemoryError> {
        self.record(Call::plain("documents.get_document"));
        Ok(lock(&self.documents)
            .get(&(namespace.to_string(), key.to_string()))
            .cloned())
    }

    async fn list_documents(
        &self,
        namespace: Option<&str>,
    ) -> Result<serde_json::Value, MemoryError> {
        self.record(Call::plain("documents.list_documents"));
        let docs = lock(&self.documents);
        let mut rows: Vec<&StoredMemoryDocument> = docs
            .values()
            .filter(|doc| namespace.is_none_or(|want| doc.namespace == want))
            .collect();
        // Newest first, as the engine's `ORDER BY updated_at DESC` gives. Ties
        // break on the key so the order is total rather than merely stable,
        // because two documents written in the same millisecond otherwise come
        // back in `HashMap` order — reproducible for a run and not between them.
        rows.sort_by(|a, b| {
            b.updated_at
                .total_cmp(&a.updated_at)
                .then_with(|| a.key.cmp(&b.key))
        });
        let documents: Vec<serde_json::Value> = rows
            .into_iter()
            .map(|d| {
                serde_json::json!({
                    "documentId": d.document_id,
                    "namespace": d.namespace,
                    "key": d.key,
                    "title": d.title,
                    "sourceType": d.source_type,
                    "priority": d.priority,
                    "createdAt": d.created_at,
                    "updatedAt": d.updated_at,
                    "taint": d.taint,
                })
            })
            .collect();
        Ok(serde_json::json!({ "count": documents.len(), "documents": documents }))
    }

    async fn list_namespaces(&self) -> Result<Vec<String>, MemoryError> {
        self.record(Call::plain("documents.list_namespaces"));
        let mut seen: Vec<String> = lock(&self.documents)
            .keys()
            .map(|(ns, _)| ns.clone())
            .collect();
        seen.sort_unstable();
        seen.dedup();
        Ok(seen)
    }

    async fn delete_document(
        &self,
        namespace: &str,
        document_id: &str,
    ) -> Result<serde_json::Value, MemoryError> {
        self.record(Call::plain("documents.delete_document"));
        let mut docs = lock(&self.documents);
        let victim = docs
            .iter()
            .find(|((ns, _), doc)| ns == namespace && doc.document_id == document_id)
            .map(|(k, _)| k.clone());
        let deleted = victim.is_some_and(|k| docs.remove(&k).is_some());
        // The namespace and the id are echoed back because the contract's
        // documented envelope carries them — this driver does not sanitise, so
        // the namespace it reports is the one it was handed.
        Ok(serde_json::json!({
            "deleted": deleted,
            "namespace": namespace,
            "documentId": document_id,
        }))
    }

    async fn clear_namespace(&self, namespace: &str) -> Result<(), MemoryError> {
        self.record(Call::plain("documents.clear_namespace"));
        lock(&self.documents).retain(|(ns, _), _| ns != namespace);
        Ok(())
    }

    async fn query_documents(
        &self,
        namespace: &str,
        query: &str,
        _limit: usize,
    ) -> Result<NamespaceRetrievalContext, MemoryError> {
        self.record(Call {
            method: "documents.query_documents".into(),
            content: Some(query.to_string()),
            taint: None,
            scoped: None,
        });
        Ok(NamespaceRetrievalContext {
            namespace: namespace.to_string(),
            query: Some(query.to_string()),
            context_text: String::new(),
            hits: vec![],
        })
    }

    async fn recall_documents(
        &self,
        namespace: &str,
        limit: usize,
    ) -> Result<NamespaceRetrievalContext, MemoryError> {
        self.record(Call::plain("documents.recall_documents"));
        // Query-less recall over the documents this driver holds.
        //
        // It used to answer an empty context unconditionally, which for a
        // namespace holding documents is the write-only shape again, reached
        // through a different reader: `put_document` accepted the write and
        // this said the namespace was empty. The contract's "an empty
        // namespace returns empty context" carries the converse.
        //
        // Freshness is the whole of the ranking here, and deliberately so. The
        // contract calls this "the namespace's freshness and priority
        // ranking"; freshness is `updated_at`, which any driver storing
        // documents has, whereas how priority *weighs against* it is a scoring
        // model this driver has no business inventing. So `priority` breaks
        // ties and nothing more, and `score` stays 0.0 rather than a number
        // that would look like a ranking signal a caller could sort on.
        let docs = lock(&self.documents);
        let mut rows: Vec<&StoredMemoryDocument> = docs
            .values()
            .filter(|doc| doc.namespace == namespace)
            .collect();
        rows.sort_by(|a, b| {
            b.updated_at
                .total_cmp(&a.updated_at)
                .then_with(|| a.priority.cmp(&b.priority))
                .then_with(|| a.key.cmp(&b.key))
        });
        rows.truncate(limit);
        let hits: Vec<NamespaceMemoryHit> = rows
            .iter()
            .map(|d| NamespaceMemoryHit {
                id: d.document_id.clone(),
                kind: MemoryItemKind::Document,
                namespace: d.namespace.clone(),
                key: d.key.clone(),
                title: Some(d.title.clone()),
                content: d.content.clone(),
                category: d.category.clone(),
                source_type: Some(d.source_type.clone()),
                updated_at: d.updated_at,
                score: 0.0,
                score_breakdown: RetrievalScoreBreakdown::default(),
                document_id: Some(d.document_id.clone()),
                chunk_id: None,
                supporting_relations: Vec::new(),
                taint: d.taint,
            })
            .collect();
        // `context_text` is documented as "assembled from `hits`", so it is
        // assembled from them rather than rendered independently — the two
        // disagreeing is the defect the field's own doc comment warns about.
        let context_text = hits
            .iter()
            .map(|hit| {
                format!(
                    "{}\n{}",
                    hit.title.as_deref().unwrap_or(&hit.key),
                    hit.content
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        Ok(NamespaceRetrievalContext {
            namespace: namespace.to_string(),
            query: None,
            context_text,
            hits,
        })
    }
}

#[async_trait]
impl MemoryTree for RecordingProvider {
    async fn append(&self, request: IngestRequest) -> Result<(), MemoryError> {
        self.record(Call {
            method: "tree.append".into(),
            content: Some(request.content),
            taint: None,
            scoped: None,
        });
        Ok(())
    }

    async fn query_source(
        &self,
        _namespace: &str,
        _source_id: &str,
        _limit: usize,
        scope: Option<&SourceScope>,
    ) -> Result<Vec<Chunk>, MemoryError> {
        self.record(Call {
            method: "tree.query_source".into(),
            // The scope's allow list, rendered so a test can assert which one
            // arrived. Sorted because it comes from a `HashSet`.
            content: scope.map(|s| {
                let mut allow = s.allow.clone();
                allow.sort();
                allow.join(",")
            }),
            taint: None,
            scoped: Some(scope.is_some()),
        });
        Ok(vec![])
    }

    async fn drill_down(
        &self,
        _namespace: &str,
        _node_id: &str,
    ) -> Result<QueryResult, MemoryError> {
        self.record(Call::plain("tree.drill_down"));
        Err(MemoryError::NotFound("node".into()))
    }

    async fn seal(&self, namespace: &str) -> Result<TreeStatus, MemoryError> {
        self.record(Call::plain("tree.seal"));
        Ok(tree_status(namespace))
    }

    async fn cascade(&self, namespace: &str) -> Result<TreeStatus, MemoryError> {
        self.record(Call::plain("tree.cascade"));
        Ok(tree_status(namespace))
    }

    /// Records the folded bodies as one blob, so a redaction test can assert on
    /// what the driver's summariser would have been handed.
    async fn summarise(
        &self,
        inputs: &[tinymemory_api::provider::content::SummaryInput],
        _context: &tinymemory_api::provider::content::SummaryContext,
    ) -> Result<tinymemory_api::provider::content::SummaryOutput, MemoryError> {
        self.record(Call {
            method: "tree.summarise".into(),
            content: Some(
                inputs
                    .iter()
                    .map(|input| input.content.clone())
                    .collect::<Vec<_>>()
                    .join("|"),
            ),
            taint: None,
            scoped: None,
        });
        Ok(Default::default())
    }

    async fn root_summaries_with_caps(
        &self,
        _per_namespace_cap: usize,
        _total_cap: usize,
    ) -> Result<Vec<tinymemory_api::provider::content::RootSummary>, MemoryError> {
        self.record(Call::plain("tree.root_summaries_with_caps"));
        Ok(Vec::new())
    }

    // ── The runtime-tree and flavour doors ──────────────────────────────────
    //
    // Overridden for the same reason `summarise` and `root_summaries_with_caps`
    // are: each is defaulted on the trait, so a `GuardedTree` that forgot to
    // forward one still compiles and answers `Unsupported`. A driver that
    // *succeeds* here is what makes `the_defaulted_doors_are_forwarded_rather_than_refused`
    // able to tell the two apart.

    /// Records the buffered body, so a redaction test can assert what the
    /// driver's buffer would have been handed — [`Self::append`]'s twin.
    async fn runtime_buffer_write(
        &self,
        _namespace: &str,
        content: &str,
        _timestamp: tinymemory_api::chrono::DateTime<tinymemory_api::chrono::Utc>,
        _metadata: Option<serde_json::Value>,
    ) -> Result<String, MemoryError> {
        self.record(Call {
            method: "tree.runtime_buffer_write".into(),
            content: Some(content.to_string()),
            taint: None,
            scoped: None,
        });
        Ok("/buffer/2026/01/01/00.md".to_string())
    }

    async fn runtime_read_node(
        &self,
        _namespace: &str,
        _node_id: &str,
    ) -> Result<Option<tinymemory_api::tree::TreeNode>, MemoryError> {
        self.record(Call::plain("tree.runtime_read_node"));
        Ok(None)
    }

    async fn runtime_read_children(
        &self,
        _namespace: &str,
        _parent_id: &str,
    ) -> Result<Vec<tinymemory_api::tree::TreeNode>, MemoryError> {
        self.record(Call::plain("tree.runtime_read_children"));
        Ok(Vec::new())
    }

    async fn runtime_tree_status(&self, namespace: &str) -> Result<TreeStatus, MemoryError> {
        self.record(Call::plain("tree.runtime_tree_status"));
        Ok(tree_status(namespace))
    }

    async fn runtime_summarize(
        &self,
        _namespace: &str,
        _timestamp: tinymemory_api::chrono::DateTime<tinymemory_api::chrono::Utc>,
    ) -> Result<Option<tinymemory_api::tree::TreeNode>, MemoryError> {
        self.record(Call::plain("tree.runtime_summarize"));
        Ok(None)
    }

    async fn runtime_rebuild(&self, namespace: &str) -> Result<TreeStatus, MemoryError> {
        self.record(Call::plain("tree.runtime_rebuild"));
        Ok(tree_status(namespace))
    }

    async fn flavour_profile(&self, _scope: &str) -> Result<Option<String>, MemoryError> {
        self.record(Call::plain("tree.flavour_profile"));
        Ok(None)
    }
}

#[async_trait]
impl MemoryEntities for RecordingProvider {
    async fn entities(
        &self,
        _namespace: &str,
        _query: Option<&str>,
        _limit: usize,
    ) -> Result<Vec<EntityHit>, MemoryError> {
        self.record(Call::plain("entities.entities"));
        Ok(vec![])
    }

    async fn entity_edges(
        &self,
        _namespace: &str,
        _entity_id: &str,
        _limit: usize,
    ) -> Result<Vec<GraphRelationRecord>, MemoryError> {
        self.record(Call::plain("entities.entity_edges"));
        Ok(vec![])
    }

    async fn touch_entities(
        &self,
        _namespace: &str,
        _entity_ids: &[String],
    ) -> Result<(), MemoryError> {
        self.record(Call::plain("entities.touch_entities"));
        Ok(())
    }
}

#[async_trait]
impl MemoryGraph for RecordingProvider {
    async fn kv_get(
        &self,
        _namespace: Option<&str>,
        _key: &str,
    ) -> Result<Option<MemoryKvRecord>, MemoryError> {
        self.record(Call::plain("graph.kv_get"));
        Ok(lock(&self.kv)
            .get(&(_namespace.map(str::to_string), _key.to_string()))
            .cloned())
    }

    async fn kv_put(
        &self,
        namespace: Option<&str>,
        key: &str,
        value: serde_json::Value,
    ) -> Result<(), MemoryError> {
        self.record(Call {
            method: "graph.kv_put".into(),
            content: Some(value.to_string()),
            taint: None,
            scoped: None,
        });
        let owned_ns = namespace.map(str::to_string);
        lock(&self.kv).insert(
            (owned_ns.clone(), key.to_string()),
            MemoryKvRecord {
                namespace: owned_ns,
                key: key.to_string(),
                value,
                updated_at: 0.0,
            },
        );
        Ok(())
    }

    async fn kv_delete(&self, namespace: Option<&str>, key: &str) -> Result<bool, MemoryError> {
        self.record(Call::plain("graph.kv_delete"));
        Ok(lock(&self.kv)
            .remove(&(namespace.map(str::to_string), key.to_string()))
            .is_some())
    }

    async fn kv_list(
        &self,
        _namespace: Option<&str>,
        _prefix: Option<&str>,
        _limit: usize,
    ) -> Result<Vec<MemoryKvRecord>, MemoryError> {
        self.record(Call::plain("graph.kv_list"));
        let want_ns = _namespace.map(str::to_string);
        let mut rows: Vec<MemoryKvRecord> = lock(&self.kv)
            .iter()
            .filter(|((ns, key), _)| *ns == want_ns && _prefix.is_none_or(|p| key.starts_with(p)))
            .map(|(_, record)| record.clone())
            .collect();
        rows.sort_by(|a, b| a.key.cmp(&b.key));
        rows.truncate(_limit);
        Ok(rows)
    }

    async fn relations(
        &self,
        _namespace: Option<&str>,
        _subject: Option<&str>,
        _predicate: Option<&str>,
        _limit: usize,
    ) -> Result<Vec<GraphRelationRecord>, MemoryError> {
        self.record(Call::plain("graph.relations"));
        let want_ns = _namespace.map(str::to_string);
        let mut rows: Vec<GraphRelationRecord> = lock(&self.relations)
            .values()
            .filter(|r| _namespace.is_none() || r.namespace == want_ns)
            .filter(|r| _subject.is_none_or(|s| r.subject == s))
            .filter(|r| _predicate.is_none_or(|p| r.predicate == p))
            .cloned()
            .collect();
        rows.sort_by(|a, b| {
            (&a.subject, &a.predicate, &a.object).cmp(&(&b.subject, &b.predicate, &b.object))
        });
        rows.truncate(_limit);
        Ok(rows)
    }

    async fn put_relation(&self, relation: GraphRelationRecord) -> Result<(), MemoryError> {
        self.record(Call::plain("graph.put_relation"));
        lock(&self.relations).insert(
            (
                relation.namespace.clone(),
                relation.subject.clone(),
                relation.predicate.clone(),
                relation.object.clone(),
            ),
            relation,
        );
        Ok(())
    }
}

#[async_trait]
impl MemoryDiff for RecordingProvider {
    async fn capture_snapshot(&self, _source_id: &str) -> Result<SnapshotRef, MemoryError> {
        self.record(Call::plain("diff.capture_snapshot"));
        Err(MemoryError::NotFound("source".into()))
    }

    async fn snapshots(
        &self,
        _source_id: &str,
        _limit: usize,
    ) -> Result<Vec<SnapshotRef>, MemoryError> {
        self.record(Call::plain("diff.snapshots"));
        Ok(vec![])
    }

    async fn diff(
        &self,
        _source_id: &str,
        _from: Option<&str>,
        _to: &str,
    ) -> Result<DiffReport, MemoryError> {
        self.record(Call::plain("diff.diff"));
        Err(MemoryError::NotFound("snapshot".into()))
    }
}

#[async_trait]
impl MemoryGoals for RecordingProvider {
    async fn goals(&self) -> Result<GoalsDoc, MemoryError> {
        self.record(Call::plain("goals.goals"));
        Ok(lock(&self.goals).clone().unwrap_or_default())
    }

    async fn set_goals(&self, goals: GoalsDoc) -> Result<(), MemoryError> {
        self.record(Call::plain("goals.set_goals"));
        *lock(&self.goals) = Some(goals);
        Ok(())
    }
}

#[async_trait]
impl MemoryToolMemory for RecordingProvider {
    async fn tool_rules(&self, tool_name: &str) -> Result<Vec<ToolMemoryRule>, MemoryError> {
        self.record(Call::plain("tool_memory.tool_rules"));
        let mut rows: Vec<ToolMemoryRule> = lock(&self.tool_rules)
            .values()
            .filter(|r| r.tool_name == tool_name)
            .cloned()
            .collect();
        rows.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(rows)
    }

    async fn put_tool_rule(&self, rule: ToolMemoryRule) -> Result<(), MemoryError> {
        self.record(Call::plain("tool_memory.put_tool_rule"));
        lock(&self.tool_rules).insert(rule.id.clone(), rule);
        Ok(())
    }

    async fn delete_tool_rule(&self, tool_name: &str, rule_id: &str) -> Result<bool, MemoryError> {
        self.record(Call::plain("tool_memory.delete_tool_rule"));
        let mut rules = lock(&self.tool_rules);
        match rules.get(rule_id) {
            Some(rule) if rule.tool_name == tool_name => {
                rules.remove(rule_id);
                Ok(true)
            }
            // Deleting by the wrong tool name is a miss, not a silent success:
            // the id is unique but the pair is what the caller asserted.
            _ => Ok(false),
        }
    }
}
#[async_trait]
impl MemorySourceSink for RecordingProvider {
    async fn accept_source_items(
        &self,
        _source_id: &str,
        _source_kind: &str,
        items: Vec<SourceItem>,
        taint: MemoryTaint,
    ) -> Result<IngestOutcome, MemoryError> {
        self.record(Call {
            method: "sources.accept_source_items".into(),
            content: items.first().map(|i| i.content.clone()),
            taint: Some(taint),
            scoped: None,
        });
        Ok(IngestOutcome::default())
    }

    async fn forget_source(&self, _source_id: &str) -> Result<u64, MemoryError> {
        self.record(Call::plain("sources.forget_source"));
        Ok(0)
    }
}

#[async_trait]
impl MemoryMaintenance for RecordingProvider {
    async fn reembed(&self) -> Result<MaintenanceReport, MemoryError> {
        self.record(Call::plain("maintenance.reembed"));
        Ok(MaintenanceReport::default())
    }

    async fn compact(&self) -> Result<MaintenanceReport, MemoryError> {
        self.record(Call::plain("maintenance.compact"));
        Ok(MaintenanceReport::default())
    }

    async fn consolidate(&self) -> Result<MaintenanceReport, MemoryError> {
        self.record(Call::plain("maintenance.consolidate"));
        Ok(MaintenanceReport::default())
    }

    async fn doctor(&self) -> Result<MaintenanceReport, MemoryError> {
        self.record(Call::plain("maintenance.doctor"));
        Ok(MaintenanceReport::default())
    }

    async fn diagnose(
        &self,
    ) -> Result<tinymemory_api::provider::diagnosis::Diagnosis, MemoryError> {
        self.record(Call::plain("maintenance.diagnose"));
        Ok(Default::default())
    }

    async fn degraded_state(
        &self,
    ) -> Result<tinymemory_api::provider::diagnosis::DegradedCapabilities, MemoryError> {
        self.record(Call::plain("maintenance.degraded_state"));
        Ok(Default::default())
    }
}

#[async_trait]
impl MemoryProvider for RecordingProvider {
    fn driver_id(&self) -> &str {
        FULL_DRIVER_ID
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::all()
    }

    async fn health(&self) -> MemoryHealth {
        MemoryHealth::Ready
    }

    fn as_ingest(&self) -> Option<&dyn MemoryIngest> {
        Some(self)
    }
    fn as_documents(&self) -> Option<&dyn MemoryDocuments> {
        Some(self)
    }
    fn as_tree(&self) -> Option<&dyn MemoryTree> {
        Some(self)
    }
    fn as_entities(&self) -> Option<&dyn MemoryEntities> {
        Some(self)
    }
    fn as_graph(&self) -> Option<&dyn MemoryGraph> {
        Some(self)
    }
    fn as_diff(&self) -> Option<&dyn MemoryDiff> {
        Some(self)
    }
    fn as_goals(&self) -> Option<&dyn MemoryGoals> {
        Some(self)
    }
    fn as_tool_memory(&self) -> Option<&dyn MemoryToolMemory> {
        Some(self)
    }
    fn as_sources(&self) -> Option<&dyn MemorySourceSink> {
        Some(self)
    }
    fn as_maintenance(&self) -> Option<&dyn MemoryMaintenance> {
        Some(self)
    }
    fn as_people(&self) -> Option<&dyn MemoryPeople> {
        Some(self)
    }
    fn as_chunks(&self) -> Option<&dyn MemoryChunks> {
        Some(self)
    }
    fn as_retrieval(&self) -> Option<&dyn MemoryRetrieval> {
        Some(self)
    }
    fn as_profile(&self) -> Option<&dyn MemoryProfile> {
        Some(self)
    }
    fn as_episodic(&self) -> Option<&dyn MemoryEpisodic> {
        Some(self)
    }
    fn as_source_sync(&self) -> Option<&dyn MemorySourceSync> {
        Some(self)
    }
    fn as_coding_sessions(&self) -> Option<&dyn MemoryCodingSessions> {
        Some(self)
    }
    fn as_scoring(&self) -> Option<&dyn MemoryScoring> {
        Some(self)
    }
    fn as_document_ingest(&self) -> Option<&dyn MemoryDocumentIngest> {
        Some(self)
    }
    fn as_conversation_ingest(&self) -> Option<&dyn MemoryConversationIngest> {
        Some(self)
    }
    fn as_learning_ingest(&self) -> Option<&dyn MemoryLearningIngest> {
        Some(self)
    }
    fn as_event_ingest(&self) -> Option<&dyn MemoryEventIngest> {
        Some(self)
    }
    fn as_answer(&self) -> Option<&dyn MemoryAnswer> {
        Some(self)
    }
}

// The two families tinymemory v1.7.0 added. `capabilities()` above answers
// `Capabilities::all()`, so a driver that advertises them and then hands back
// `None` from the accessor is exactly the inconsistency `audit_provider`
// exists to catch — the recorder has to serve them to stay honest.

#[async_trait]
impl MemorySourceSync for RecordingProvider {
    async fn run_connection_sync(
        &self,
        toolkit: &str,
        connection_id: &str,
    ) -> Result<SyncRunOutcome, MemoryError> {
        self.record(Call::plain("source_sync.run_connection_sync"));
        let _ = (toolkit, connection_id);
        Ok(SyncRunOutcome::default())
    }
    async fn source_sync_state(
        &self,
        toolkit: &str,
        connection_id: &str,
    ) -> Result<Option<SourceSyncState>, MemoryError> {
        self.record(Call::plain("source_sync.source_sync_state"));
        let _ = (toolkit, connection_id);
        Ok(None)
    }
    async fn sync_audit_log(
        &self,
        _limit: Option<usize>,
    ) -> Result<Vec<SyncAuditEntry>, MemoryError> {
        self.record(Call::plain("source_sync.sync_audit_log"));
        Ok(Vec::new())
    }
    async fn estimate_sync_cost_usd(
        &self,
        _input_tokens: u64,
        _output_tokens: u64,
    ) -> Result<f64, MemoryError> {
        self.record(Call::plain("source_sync.estimate_sync_cost_usd"));
        Ok(0.0)
    }
    async fn sync_statuses(&self) -> Result<Vec<SourceSyncStatus>, MemoryError> {
        self.record(Call::plain("source_sync.sync_statuses"));
        Ok(Vec::new())
    }
    async fn raw_archive_coverage(
        &self,
        tree_scope: &str,
        archive_source_id: &str,
    ) -> Result<RawArchiveCoverage, MemoryError> {
        self.record(Call::plain("source_sync.raw_archive_coverage"));
        let _ = (tree_scope, archive_source_id);
        Ok(RawArchiveCoverage::default())
    }
    async fn rebuild_from_raw_archive(
        &self,
        tree_scope: &str,
        archive_source_id: &str,
    ) -> Result<RawRebuildOutcome, MemoryError> {
        self.record(Call::plain("source_sync.rebuild_from_raw_archive"));
        let _ = (tree_scope, archive_source_id);
        Ok(RawRebuildOutcome::default())
    }
}

#[async_trait]
impl MemoryCodingSessions for RecordingProvider {
    async fn coding_session_status(&self) -> Result<Vec<CodingSessionSource>, MemoryError> {
        self.record(Call::plain("coding_sessions.coding_session_status"));
        Ok(Vec::new())
    }
    async fn ingest_coding_sessions(
        &self,
        _request: CodingSessionIngestRequest,
    ) -> Result<CodingSessionIngestReport, MemoryError> {
        self.record(Call::plain("coding_sessions.ingest_coding_sessions"));
        Ok(CodingSessionIngestReport::default())
    }
}

#[async_trait]
impl MemoryEpisodic for RecordingProvider {
    async fn insert_turn(
        &self,
        turn: &tinymemory_api::provider::episodic::EpisodicTurn,
    ) -> Result<i64, MemoryError> {
        // Records the turn text, so a guard that failed to redact one would be
        // visible here rather than only in a live store.
        self.record(Call {
            method: "episodic.insert_turn".into(),
            content: Some(turn.content.clone()),
            taint: None,
            scoped: None,
        });
        Ok(1)
    }

    async fn session_turns(
        &self,
        _session_id: &str,
    ) -> Result<Vec<tinymemory_api::provider::episodic::EpisodicTurn>, MemoryError> {
        self.record(Call::plain("episodic.session_turns"));
        Ok(vec![])
    }

    async fn open_segment(
        &self,
        _session_id: &str,
    ) -> Result<Option<tinymemory_api::provider::episodic::ConversationSegment>, MemoryError> {
        self.record(Call::plain("episodic.open_segment"));
        Ok(None)
    }

    /// Recorded rather than left to the trait default: the default answers
    /// `Ok(vec![])` too, but silently, and a recorder that does not see the
    /// call cannot hold a caller to making it.
    async fn segments_pending_summary(
        &self,
        _limit: u32,
    ) -> Result<Vec<tinymemory_api::provider::episodic::ConversationSegment>, MemoryError> {
        self.record(Call::plain("episodic.segments_pending_summary"));
        Ok(vec![])
    }

    async fn create_segment(
        &self,
        _segment_id: &str,
        _session_id: &str,
        _namespace: &str,
        _start_episodic_id: i64,
        _start_seq: Option<u32>,
        _start_timestamp: f64,
        _now: f64,
    ) -> Result<(), MemoryError> {
        self.record(Call::plain("episodic.create_segment"));
        Ok(())
    }

    async fn append_turn(
        &self,
        _segment_id: &str,
        _episodic_id: i64,
        _seq: Option<u32>,
        _timestamp: f64,
        _now: f64,
    ) -> Result<(), MemoryError> {
        self.record(Call::plain("episodic.append_turn"));
        Ok(())
    }

    async fn close_segment(&self, _segment_id: &str, _now: f64) -> Result<(), MemoryError> {
        self.record(Call::plain("episodic.close_segment"));
        Ok(())
    }

    async fn insert_event(&self, event: &EpisodicEvent) -> Result<(), MemoryError> {
        // Records the event text for the same reason `insert_turn` does: a guard
        // that stopped redacting one would otherwise be invisible to every test,
        // and the redaction on this path has already been missing once.
        self.record(Call {
            method: "episodic.insert_event".into(),
            content: Some(event.content.clone()),
            taint: None,
            scoped: None,
        });
        Ok(())
    }

    async fn set_segment_summary(
        &self,
        _segment_id: &str,
        summary: &str,
        _now: f64,
    ) -> Result<(), MemoryError> {
        self.record(Call {
            method: "episodic.set_segment_summary".into(),
            content: Some(summary.to_string()),
            taint: None,
            scoped: None,
        });
        Ok(())
    }

    async fn upsert_segment_embedding(
        &self,
        _segment_id: &str,
        _model_signature: &str,
        _embedding: &[f32],
        _created_at: f64,
    ) -> Result<(), MemoryError> {
        self.record(Call::plain("episodic.upsert_segment_embedding"));
        Ok(())
    }
}
#[async_trait]
impl MemoryProfile for RecordingProvider {
    async fn list_active_facets(&self) -> Result<Vec<ProfileFacet>, MemoryError> {
        self.record(Call::plain("profile.list_active_facets"));
        Ok(vec![])
    }
    async fn list_all_facets(&self) -> Result<Vec<ProfileFacet>, MemoryError> {
        self.record(Call::plain("profile.list_all_facets"));
        Ok(vec![])
    }
    async fn get_facet(&self, _key: &str) -> Result<Option<ProfileFacet>, MemoryError> {
        self.record(Call::plain("profile.get_facet"));
        Ok(None)
    }
    async fn facets_by_type(
        &self,
        _facet_type: FacetType,
    ) -> Result<Vec<ProfileFacet>, MemoryError> {
        self.record(Call::plain("profile.facets_by_type"));
        Ok(vec![])
    }
    async fn upsert_facet(&self, _facet: &ProfileFacet) -> Result<(), MemoryError> {
        self.record(Call::plain("profile.upsert_facet"));
        Ok(())
    }
    async fn upsert_provider_facet(
        &self,
        _facet_id: &str,
        _facet_type: FacetType,
        _key: &str,
        _value: &str,
        _confidence: f64,
        _segment_id: Option<&str>,
        _observed_at: f64,
    ) -> Result<(), MemoryError> {
        self.record(Call::plain("profile.upsert_provider_facet"));
        Ok(())
    }
    async fn set_facet_user_state(
        &self,
        _key: &str,
        _user_state: UserState,
    ) -> Result<bool, MemoryError> {
        self.record(Call::plain("profile.set_facet_user_state"));
        Ok(false)
    }
    async fn delete_facet(&self, _key: &str) -> Result<bool, MemoryError> {
        self.record(Call::plain("profile.delete_facet"));
        Ok(false)
    }
    async fn delete_facet_by_id(&self, _facet_id: &str) -> Result<bool, MemoryError> {
        self.record(Call::plain("profile.delete_facet_by_id"));
        Ok(false)
    }
    async fn drop_facets_below(&self, _threshold: f64) -> Result<usize, MemoryError> {
        self.record(Call::plain("profile.drop_facets_below"));
        Ok(0)
    }
    async fn workflow_identity_matches(&self, _pattern: &str, _value: &str) -> bool {
        self.record(Call::plain("profile.workflow_identity_matches"));
        false
    }
}

#[async_trait]
impl MemoryChunks for RecordingProvider {
    async fn list_chunks(
        &self,
        _query: &ChunkQuery,
        scope: Option<&SourceScope>,
    ) -> Result<Vec<Chunk>, MemoryError> {
        self.record(Call {
            method: "chunks.list_chunks".into(),
            content: rendered_scope(scope),
            taint: None,
            scoped: Some(scope.is_some()),
        });
        Ok(vec![])
    }

    async fn get_chunk(&self, _chunk_id: &str) -> Result<Option<Chunk>, MemoryError> {
        self.record(Call::plain("chunks.get_chunk"));
        Ok(None)
    }

    async fn chunk_detail(&self, _chunk_id: &str) -> Result<Option<ChunkDetail>, MemoryError> {
        self.record(Call::plain("chunks.chunk_detail"));
        Ok(None)
    }

    async fn storage_kinds(&self) -> Result<Vec<String>, MemoryError> {
        self.record(Call::plain("chunks.storage_kinds"));
        Ok(vec![])
    }

    async fn chunk_embeddings(
        &self,
        _chunk_ids: &[String],
        _model_signature: &str,
    ) -> Result<Vec<ChunkEmbedding>, MemoryError> {
        self.record(Call::plain("chunks.chunk_embeddings"));
        Ok(vec![])
    }

    async fn chunk_score(
        &self,
        _chunk_id: &str,
    ) -> Result<Option<tinymemory_api::provider::chunks::ChunkScore>, MemoryError> {
        self.record(Call::plain("chunks.chunk_score"));
        Ok(None)
    }

    async fn source_ingest_status(
        &self,
        _source_prefixes: &[tinymemory_api::provider::chunks::SourceIngestQuery],
    ) -> Result<Vec<tinymemory_api::provider::chunks::SourceIngestStatus>, MemoryError> {
        self.record(Call::plain("chunks.source_ingest_status"));
        Ok(vec![])
    }
}

#[async_trait]
impl MemoryRetrieval for RecordingProvider {
    async fn fast_retrieve(
        &self,
        _query: &str,
        _options: FastRetrieveQuery,
        scope: Option<&SourceScope>,
    ) -> Result<RetrievalResponse, MemoryError> {
        self.record(Call {
            method: "retrieval.fast_retrieve".into(),
            content: rendered_scope(scope),
            taint: None,
            scoped: Some(scope.is_some()),
        });
        Ok(lock(&self.fast_retrieve_result).clone())
    }

    async fn cover_window(
        &self,
        _window: &CoverWindowQuery,
        scope: Option<&SourceScope>,
    ) -> Result<RetrievalResponse, MemoryError> {
        self.record(Call {
            method: "retrieval.cover_window".into(),
            content: rendered_scope(scope),
            taint: None,
            scoped: Some(scope.is_some()),
        });
        Ok(RetrievalResponse::default())
    }

    async fn retrieve_source(
        &self,
        _query: &SourceRetrievalQuery,
        scope: Option<&SourceScope>,
    ) -> Result<RetrievalResponse, MemoryError> {
        self.record(Call {
            method: "retrieval.retrieve_source".into(),
            content: rendered_scope(scope),
            taint: None,
            scoped: Some(scope.is_some()),
        });
        Ok(RetrievalResponse::default())
    }

    async fn retrieve_children(
        &self,
        _node_id: &str,
        _max_depth: u32,
        _query: Option<&str>,
        _limit: Option<usize>,
        scope: Option<&SourceScope>,
    ) -> Result<Vec<RetrievalHit>, MemoryError> {
        self.record(Call {
            method: "retrieval.retrieve_children".into(),
            content: rendered_scope(scope),
            taint: None,
            scoped: Some(scope.is_some()),
        });
        Ok(vec![])
    }

    async fn retrieve_leaves(
        &self,
        _chunk_ids: &[String],
        scope: Option<&SourceScope>,
    ) -> Result<Vec<RetrievalHit>, MemoryError> {
        self.record(Call {
            method: "retrieval.retrieve_leaves".into(),
            content: rendered_scope(scope),
            taint: None,
            scoped: Some(scope.is_some()),
        });
        Ok(vec![])
    }

    async fn recall_namespace_scored(
        &self,
        namespace: &str,
        _query: &str,
        limit: usize,
        _exclude_session_id: Option<&str>,
    ) -> Result<Vec<NamespaceMemoryHit>, MemoryError> {
        // Honours the two request parameters a caller can get wrong — the
        // namespace it asks for and the page it accepts — and records them,
        // so a test can assert both rather than only the content it got.
        self.record(Call {
            method: "retrieval.recall_namespace_scored".into(),
            content: Some(format!("namespace={namespace} limit={limit}")),
            taint: None,
            scoped: None,
        });
        Ok(lock(&self.namespace_hits)
            .iter()
            .filter(|hit| hit.namespace == namespace)
            .take(limit)
            .cloned()
            .collect())
    }

    async fn recall_namespace_recent(
        &self,
        _namespace: &str,
        _limit: usize,
    ) -> Result<Vec<NamespaceMemoryHit>, MemoryError> {
        self.record(Call::plain("retrieval.recall_namespace_recent"));
        Ok(vec![])
    }

    async fn search_entities(
        &self,
        _query: &str,
        kinds: Option<&[String]>,
        _limit: usize,
    ) -> Result<Vec<EntityMatch>, MemoryError> {
        self.record(Call::plain("retrieval.search_entities"));
        // Validating the filter is a contract obligation, not an engine
        // nicety: `MemoryRetrieval::search_entities` documents `Invalid` for an
        // unrecognised kind precisely because "silently matching nothing would
        // look identical to a genuine empty result". This driver answers no
        // matches, so it is the one driver where skipping the check is
        // invisible — and answering `Ok(vec![])` to a typo is exactly the
        // confusion the rule exists to prevent.
        //
        // The vocabulary is open on the *response* side (`EntityMatch::kind` is
        // a passthrough string, so an engine may emit a kind this build has not
        // heard of) and closed on the *request* side. `KNOWN_ENTITY_KINDS` is
        // the request-side list the contract's module docs enumerate.
        if let Some(kinds) = kinds {
            for kind in kinds {
                if !KNOWN_ENTITY_KINDS.contains(&kind.as_str()) {
                    return Err(MemoryError::Invalid(format!("unknown entity kind: {kind}")));
                }
            }
        }
        Ok(vec![])
    }
}

#[async_trait]
impl MemoryPeople for RecordingProvider {
    async fn list_people(&self, _limit: Option<usize>) -> Result<Vec<RankedPerson>, MemoryError> {
        self.record(Call::plain("people.list_people"));
        Ok(vec![])
    }

    async fn get_person(&self, _person_id: &str) -> Result<Option<PersonRecord>, MemoryError> {
        self.record(Call::plain("people.get_person"));
        Ok(None)
    }

    async fn resolve_handle(
        &self,
        _handle: &PersonHandle,
        _create_if_missing: bool,
    ) -> Result<Option<ResolvedPerson>, MemoryError> {
        self.record(Call::plain("people.resolve_handle"));
        Ok(None)
    }

    async fn add_handle_alias(
        &self,
        _person_id: &str,
        _handle: &PersonHandle,
    ) -> Result<(), MemoryError> {
        self.record(Call::plain("people.add_handle_alias"));
        Ok(())
    }

    async fn score_person(&self, _person_id: &str) -> Result<Option<PersonScore>, MemoryError> {
        self.record(Call::plain("people.score_person"));
        Ok(None)
    }

    async fn record_interaction(
        &self,
        _interaction: &PersonInteraction,
    ) -> Result<(), MemoryError> {
        self.record(Call::plain("people.record_interaction"));
        Ok(())
    }

    async fn seed_from_address_book(&self) -> Result<AddressBookSeedOutcome, MemoryError> {
        self.record(Call::plain("people.seed_from_address_book"));
        Ok(AddressBookSeedOutcome::default())
    }
}

#[async_trait]
impl MemoryScoring for RecordingProvider {
    async fn extract_entities(&self, query: &str) -> Result<Vec<String>, MemoryError> {
        self.record(Call {
            method: "scoring.extract_entities".into(),
            content: Some(query.to_string()),
            taint: None,
            scoped: None,
        });
        Ok(Vec::new())
    }

    async fn embed_text(&self, text: &str) -> Result<Vec<f32>, MemoryError> {
        self.record(Call {
            method: "scoring.embed_text".into(),
            content: Some(text.to_string()),
            taint: None,
            scoped: None,
        });
        Ok(Vec::new())
    }

    async fn embedder_slug(&self) -> Result<String, MemoryError> {
        self.record(Call::plain("scoring.embedder_slug"));
        Ok(String::new())
    }
}

// ── The v1.13.7 typed-ingestion round + Answer ──────────────────────────────
// Same contract as every family above: `capabilities()` answers all(), so the
// audit demands a live accessor and a recording impl for each.

#[async_trait]
impl MemoryDocumentIngest for RecordingProvider {
    async fn ingest_document(&self, document: IngestItem) -> Result<IngestOutcome, MemoryError> {
        self.record(Call {
            method: "document_ingest.ingest_document".into(),
            content: Some(document.content),
            taint: Some(document.taint),
            scoped: None,
        });
        Ok(IngestOutcome::default())
    }
}

#[async_trait]
impl MemoryConversationIngest for RecordingProvider {
    async fn ingest_conversation(
        &self,
        messages: Vec<IngestItem>,
    ) -> Result<IngestOutcome, MemoryError> {
        for message in messages {
            self.record(Call {
                method: "conversation_ingest.ingest_conversation".into(),
                content: Some(message.content),
                taint: Some(message.taint),
                scoped: None,
            });
        }
        Ok(IngestOutcome::default())
    }
}

#[async_trait]
impl MemoryLearningIngest for RecordingProvider {
    async fn ingest_learning(
        &self,
        _learning: tinymemory_api::learning::LearningCandidate,
    ) -> Result<IngestOutcome, MemoryError> {
        self.record(Call {
            method: "learning_ingest.ingest_learning".into(),
            content: None,
            taint: None,
            scoped: None,
        });
        Ok(IngestOutcome::default())
    }
}

#[async_trait]
impl MemoryEventIngest for RecordingProvider {
    async fn ingest_event(
        &self,
        _event: tinymemory_api::provider::operations::RawMemoryEvent,
    ) -> Result<IngestOutcome, MemoryError> {
        self.record(Call {
            method: "event_ingest.ingest_event".into(),
            content: None,
            taint: None,
            scoped: None,
        });
        Ok(IngestOutcome::default())
    }
}

#[async_trait]
impl MemoryAnswer for RecordingProvider {
    async fn answer(
        &self,
        _request: tinymemory_api::provider::operations::AnswerRequest,
    ) -> Result<tinymemory_api::provider::operations::AnswerResponse, MemoryError> {
        self.record(Call {
            method: "answer.answer".into(),
            content: None,
            taint: None,
            scoped: None,
        });
        Ok(tinymemory_api::provider::operations::AnswerResponse {
            answer: String::new(),
            model: None,
            citations: Vec::new(),
            steps: Vec::new(),
        })
    }
}
// Fixtures for the retrieval family's scored answers. Included into
// `test_support.rs` after the provider parts, so the imports there are in scope.

/// A [`NamespaceSummary`] saying `namespace` holds `count` entries.
pub fn namespace_summary(namespace: &str, count: usize) -> NamespaceSummary {
    NamespaceSummary {
        namespace: namespace.into(),
        count,
        last_updated: None,
    }
}

/// A [`NamespaceMemoryHit`] with only the vector component set — the signal the
/// vector-floored recall paths (Lane B, the contradiction check) filter on.
pub fn namespace_hit(
    namespace: &str,
    key: &str,
    content: &str,
    vector_similarity: f64,
) -> NamespaceMemoryHit {
    NamespaceMemoryHit {
        id: format!("{namespace}/{key}"),
        kind: tinymemory_api::types::MemoryItemKind::Kv,
        namespace: namespace.into(),
        key: key.into(),
        title: None,
        content: content.into(),
        category: "core".into(),
        source_type: None,
        updated_at: 0.0,
        score: vector_similarity,
        score_breakdown: tinymemory_api::types::RetrievalScoreBreakdown {
            vector_similarity,
            ..Default::default()
        },
        document_id: None,
        chunk_id: None,
        supporting_relations: Vec::new(),
        taint: MemoryTaint::default(),
    }
}
