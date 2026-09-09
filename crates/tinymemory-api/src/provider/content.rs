//! Optional families that put content *into* memory and navigate it:
//! [`MemoryIngest`], [`MemoryDocuments`], and [`MemoryTree`].
//!
//! All three are optional. A driver that advertises none of them is still a
//! memory backend — it just accepts entries only through
//! [`crate::provider::MemoryCore::store`] and has no document tier and no
//! summary tree. The kernel unregisters the matching RPC methods and omits the
//! matching agent tools rather than registering handlers that fail.
//!
//! ## No configuration crosses this boundary
//!
//! Chunk sizes, embedding models, summariser prompts, seal thresholds, and
//! cascade policy are all *driver* concerns. None of them appear in these
//! signatures: the embedded driver reads them from the `MemoryConfig` it
//! already holds, and an external driver has its own. This was the sharpest
//! test of whether the M0 crate carve-out drew the line in the right place —
//! the families that looked most config-dependent turned out not to need any.

use async_trait::async_trait;
// Named through the wire crate rather than as a dependency of this one: the
// contract crate is deliberately dependency-light, and `tinymemory-bus`
// re-exports the chrono it serializes with precisely so a signature here names
// the same crate a frame decodes into.
use tinymemory_bus::chrono::{DateTime, Utc};

use crate::capabilities::Capability;
use crate::chunks::Chunk;
use crate::error::MemoryError;
use crate::provider::types::{IngestItem, IngestOutcome, SourceScope};
use crate::tree::{IngestRequest, QueryResult, SummaryForest, TreeLeaf, TreeNode, TreeStatus};
use crate::types::{NamespaceDocumentInput, NamespaceRetrievalContext, StoredMemoryDocument};

// The value types the summariser door exchanges. They are defined in
// `tinymemory-bus` — they cross the module boundary, and a host that only makes
// calls must be able to name them without compiling this trait — and
// re-exported here so the family's vocabulary is reachable from the family, the
// same arrangement `provider::chunks` uses. They are re-exported at
// `crate::tree` too, alongside the rest of the tree vocabulary; both paths name
// the same items, not twins of them.
pub use tinymemory_bus::tree::{RootSummary, SummaryContext, SummaryInput, SummaryOutput};

/// Bulk content ingestion — the driver owns chunking and embedding.
///
/// The distinction from [`crate::provider::MemoryCore::store`] is ownership of
/// the pipeline: `store` persists exactly one entry the caller has already
/// shaped, whereas ingest hands over raw source material and lets the driver
/// decide how to split, embed, and index it.
#[async_trait]
pub trait MemoryIngest: Send + Sync {
    /// Ingest one standalone document.
    ///
    /// # Errors
    ///
    /// [`MemoryError::Invalid`] for content the driver refuses (empty body,
    /// unsupported MIME), otherwise backend failures.
    async fn ingest_document(&self, item: IngestItem) -> Result<IngestOutcome, MemoryError>;

    /// Ingest a run of chat messages that share a conversation.
    ///
    /// Taken as a batch rather than one call per message because chat chunking
    /// is inherently cross-message: a driver needs neighbouring turns to decide
    /// where a chunk boundary belongs. Ordering within `messages` is
    /// significant and must be preserved by the caller.
    ///
    /// # Errors
    ///
    /// As [`Self::ingest_document`]. Partial success is reported through the
    /// counts in [`IngestOutcome`], not as an error.
    async fn ingest_chat(&self, messages: Vec<IngestItem>) -> Result<IngestOutcome, MemoryError>;

    /// Ingest one email thread as its ordered run of messages.
    ///
    /// The argument shape is [`Self::ingest_chat`]'s, and the method is
    /// separate anyway, because the difference is on the way *in*: a driver
    /// splits an email thread at message boundaries and renders per-message
    /// headers, where a chat batch is chunked across turns. Routing mail
    /// through the chat method stores it as a conversation and loses the split,
    /// which is what a citation back to a single message stands on. Flattening
    /// it into [`Self::ingest_document`] loses the per-message structure
    /// entirely.
    ///
    /// Every item must carry the same `source_id` — the thread is the
    /// ingestion group, exactly as the conversation is for chat — and
    /// `timestamp` is what orders the messages, so a caller that omits it gets
    /// ingest time and a thread ordered by arrival.
    ///
    /// # Errors
    ///
    /// [`MemoryError::Unsupported`] from a driver that predates this operation
    /// or has no mail path — it is *not* implied by the rest of the family,
    /// and a caller must be prepared for a driver that ingests documents and
    /// chat but not mail. Otherwise as [`Self::ingest_document`].
    async fn ingest_email(&self, _messages: Vec<IngestItem>) -> Result<IngestOutcome, MemoryError> {
        Err(MemoryError::unsupported(Capability::Ingest))
    }
}

/// The namespace-document tier: whole documents addressed by `(namespace, key)`.
///
/// Distinct from [`crate::provider::MemoryCore`] in granularity and in what is
/// stored: entries are short facts, documents are bodies with titles, tags,
/// source types, and structured metadata, and they carry their own ranked query
/// surface.
#[async_trait]
pub trait MemoryDocuments: Send + Sync {
    /// Upsert a document, returning its driver-assigned id.
    ///
    /// Keyed by `(namespace, key)` from the input: reusing a key replaces the
    /// existing document rather than creating a second one.
    ///
    /// # Errors
    ///
    /// [`MemoryError::Invalid`] for a rejected input, otherwise backend
    /// failures.
    async fn put_document(&self, input: NamespaceDocumentInput) -> Result<String, MemoryError>;

    /// Fetch a document by `(namespace, key)`.
    ///
    /// # Errors
    ///
    /// A missing document is `Ok(None)`; `Err` is reserved for backend
    /// failures.
    async fn get_document(
        &self,
        namespace: &str,
        key: &str,
    ) -> Result<Option<StoredMemoryDocument>, MemoryError>;

    /// List document summaries, optionally restricted to one namespace.
    ///
    /// # Shape
    ///
    /// ```json
    /// { "count": 1, "documents": [ {
    ///     "documentId": "...", "namespace": "...", "key": "...",
    ///     "title": "...", "sourceType": "...", "priority": "...",
    ///     "createdAt": 0.0, "updatedAt": 0.0, "taint": "internal"
    /// } ] }
    /// ```
    ///
    /// `count` equals the length of `documents`. Rows are newest-first by
    /// `updatedAt`, and a caller must not depend on the order of rows sharing
    /// one timestamp.
    ///
    /// This is written down because the return type cannot say it. Two drivers
    /// disagreed on it in exactly the way an untyped payload invites — one
    /// snake_case without a `count`, one camelCase with — and both satisfied
    /// every check that existed. `assert_documents_round_trip` in
    /// `tinymemory-conformance` is the executable half of this paragraph.
    ///
    /// # Errors
    ///
    /// Backend failures only.
    async fn list_documents(
        &self,
        namespace: Option<&str>,
    ) -> Result<serde_json::Value, MemoryError>;

    /// List every namespace containing documents.
    ///
    /// # Errors
    ///
    /// Backend failures only.
    async fn list_namespaces(&self) -> Result<Vec<String>, MemoryError>;

    /// Delete a document by its driver-assigned id.
    ///
    /// # Shape
    ///
    /// ```json
    /// { "deleted": true, "namespace": "...", "documentId": "..." }
    /// ```
    ///
    /// `deleted` is `false` when no such document existed — see the error note
    /// below. `namespace` is the namespace as the driver stores it, which need
    /// not be the string the caller passed: a driver that sanitises namespaces
    /// reports the sanitised form, and that is the point of echoing it back.
    ///
    /// # Errors
    ///
    /// Backend failures only; a missing document is reported in the returned
    /// outcome rather than as an error.
    async fn delete_document(
        &self,
        namespace: &str,
        document_id: &str,
    ) -> Result<serde_json::Value, MemoryError>;

    /// Delete all data belonging to one namespace.
    ///
    /// # Errors
    ///
    /// Backend failures only.
    async fn clear_namespace(&self, namespace: &str) -> Result<(), MemoryError>;

    /// Run a ranked query over one namespace's documents.
    ///
    /// Returns both the ranked hits and the driver's rendered context text, so
    /// a caller that only wants something injectable does not have to
    /// re-assemble it (and re-assemble it differently from every other caller).
    ///
    /// # Errors
    ///
    /// Backend failures only; a query that matches nothing returns an empty
    /// hit list.
    async fn query_documents(
        &self,
        namespace: &str,
        query: &str,
        limit: usize,
    ) -> Result<NamespaceRetrievalContext, MemoryError>;

    /// Recall the highest-ranked context from a namespace without a query.
    ///
    /// This is a distinct engine operation rather than a query with an empty
    /// string: query-less recall applies the namespace's freshness and
    /// priority ranking without introducing a synthetic search term.
    ///
    /// # Errors
    ///
    /// [`MemoryError::Unsupported`] when a provider predating this optional
    /// operation does not implement it, otherwise backend failures. An empty
    /// namespace returns empty context.
    async fn recall_documents(
        &self,
        _namespace: &str,
        _limit: usize,
    ) -> Result<NamespaceRetrievalContext, MemoryError> {
        Err(MemoryError::unsupported(Capability::Documents))
    }
}

/// The time-ordered summary tree: buffered leaves rolled up into hour → day →
/// month → year → root summaries.
///
/// Sealing and cascading are exposed as explicit calls rather than happening
/// implicitly on ingest because the **host** owns scheduling. A driver runs one
/// step when asked; it does not get to install its own background loop. This is
/// the same rule as the engine's `queue::run_once`.
///
/// # Navigating one node, and walking the whole forest
///
/// [`Self::drill_down`] addresses a node by id and returns it with its direct
/// children — enough to descend a tree a caller is already inside.
/// [`Self::summary_forest`] and [`Self::recent_leaves`] answer the question
/// that has no starting id: what trees exist, how they nest, and what content
/// hangs off them. Both are here rather than in
/// [`MemoryRetrieval`](crate::provider::MemoryRetrieval) because neither ranks
/// and neither takes a query; they are structure, not results.
///
/// The embedded driver happens to serve the two from different storage — the
/// markdown time tree on disk, the sealed summary forest in tables — and the
/// contract deliberately does not encode that split. See
/// [`crate::tree`] for the shapes and why they are described separately there.
#[async_trait]
pub trait MemoryTree: Send + Sync {
    /// Append raw content to the ingestion buffer for later sealing.
    ///
    /// # Errors
    ///
    /// [`MemoryError::Invalid`] for a rejected request, otherwise backend
    /// failures.
    async fn append(&self, request: IngestRequest) -> Result<(), MemoryError>;

    /// Retrieve the chunks a single logical source contributed, newest first.
    ///
    /// `scope` is the per-turn allowlist and must be applied **inside** the
    /// driver's query, for the reasons in [`SourceScope`]. `None` means
    /// unrestricted.
    ///
    /// # Errors
    ///
    /// Backend failures only; an unknown `source_id` yields an empty vector.
    async fn query_source(
        &self,
        namespace: &str,
        source_id: &str,
        limit: usize,
        scope: Option<&SourceScope>,
    ) -> Result<Vec<Chunk>, MemoryError>;

    /// Fetch one node together with its direct children, for navigation.
    ///
    /// # Errors
    ///
    /// [`MemoryError::NotFound`] when `node_id` does not exist in `namespace`.
    async fn drill_down(&self, namespace: &str, node_id: &str) -> Result<QueryResult, MemoryError>;

    /// Convert buffered content into leaf nodes, returning the resulting tree
    /// state.
    ///
    /// Idempotent when the buffer is empty: sealing nothing is a successful
    /// no-op, not an error, so a scheduler may call it unconditionally.
    ///
    /// # Errors
    ///
    /// Backend failures only.
    async fn seal(&self, namespace: &str) -> Result<TreeStatus, MemoryError>;

    /// Roll sealed leaves up through the parent levels, returning the resulting
    /// tree state.
    ///
    /// Idempotent for the same reason as [`Self::seal`].
    ///
    /// # Errors
    ///
    /// Backend failures only.
    async fn cascade(&self, namespace: &str) -> Result<TreeStatus, MemoryError>;

    /// Walk every sealed summary the store holds, across every tree.
    ///
    /// # Why [`Self::drill_down`] cannot answer this
    ///
    /// `drill_down` starts from a node id and returns that node with its
    /// direct children. A caller that wants the whole forest has no id to
    /// start from — that is what it is asking for — and no way to discover
    /// one, because nothing else in the contract enumerates trees. Walking it
    /// by repeated `drill_down` would also be one round trip per node, over a
    /// bus, to rebuild a shape the driver already has in one table.
    ///
    /// [`crate::provider::MemoryRetrieval::retrieve_children`] does not answer
    /// it either, for a different reason: it *ranks*. It needs a seed node and
    /// returns scored hits without a parent link, which is a reading list
    /// rather than a graph.
    ///
    /// # `scope` is a predicate, not a post-filter
    ///
    /// The allowlist must be applied **inside** the driver's query for the
    /// reasons in [`SourceScope`], and this member is the one where getting it
    /// wrong is least visible: an unscoped forest walk hands back every source
    /// in the store at once, which is precisely the shape a per-turn source
    /// gate exists to prevent. `None` means unrestricted and must be a
    /// decision, not a default the caller drifted into.
    ///
    /// A driver returns nodes whose tree the scope allows. It may therefore
    /// return a node whose `parent_id` names one it withheld; see
    /// [`crate::tree::TreeSummary::parent_id`] for what a caller does with
    /// that.
    ///
    /// # Bounds
    ///
    /// `limit` caps the nodes returned and the driver clamps it to its own
    /// cap — a caller cannot raise the ceiling by asking for more, the same
    /// rule [`crate::provider::ChunkQuery::limit`] carries. Hitting either
    /// bound sets [`SummaryForest::truncated`] rather than erroring.
    ///
    /// Tombstoned summaries are never returned. A driver that keeps them
    /// filters them out here; "deleted" is not a state a caller has to know
    /// about to draw a graph.
    ///
    /// # Errors
    ///
    /// [`MemoryError::Unsupported`] from a driver that has a tree family but
    /// cannot enumerate it — deliberately not an empty forest, because a
    /// driver with trees reporting none is a lie a caller would render as an
    /// empty store. Backend failures otherwise; a store that has sealed
    /// nothing returns an empty, untruncated forest, which is true of it.
    async fn summary_forest(
        &self,
        _limit: usize,
        _scope: Option<&SourceScope>,
    ) -> Result<SummaryForest, MemoryError> {
        Err(MemoryError::unsupported(Capability::Tree))
    }

    /// The most recent leaves, each with the summary that sealed it, newest
    /// first.
    ///
    /// The forest's bottom edge. [`Self::summary_forest`] returns the summary
    /// nodes and the child ids they sealed over; this returns the leaves
    /// themselves with the back-pointer that says which summary claimed them,
    /// so a caller can attach content to the structure without one lookup per
    /// leaf.
    ///
    /// # Why not [`crate::provider::MemoryChunks::list_chunks`]
    ///
    /// That returns the same rows and drops the link: a [`Chunk`] does not say
    /// which summary sealed it, and the link is what makes a leaf part of a
    /// tree rather than a loose row. It is also the half that changes without
    /// the chunk changing — a leaf gains a parent when the scheduler seals it,
    /// long after ingest.
    ///
    /// Both halves are separate calls rather than one combined read because
    /// the two bounds are separate: a caller may want the whole forest
    /// skeleton and only the newest few hundred leaves, and folding them into
    /// one response would make the smaller bound pay for the larger.
    ///
    /// # Bounds and scope
    ///
    /// As [`Self::summary_forest`]: `limit` is clamped by the driver, and
    /// `scope` is applied inside the query, before the limit, so a disallowed
    /// source cannot starve permitted ones out of the page.
    ///
    /// [`TreeLeaf::preview`] is a label, capped at
    /// [`crate::tree::LEAF_PREVIEW_CHARS`] characters. Bodies are
    /// [`crate::provider::MemoryChunks::chunk_detail`]'s job, one row at a
    /// time; a forest-sized read carrying whole bodies would not fit a frame.
    ///
    /// # Errors
    ///
    /// [`MemoryError::Unsupported`] on the same terms as
    /// [`Self::summary_forest`]. Backend failures otherwise; a store with no
    /// leaves returns an empty vector.
    async fn recent_leaves(
        &self,
        _limit: usize,
        _scope: Option<&SourceScope>,
    ) -> Result<Vec<TreeLeaf>, MemoryError> {
        Err(MemoryError::unsupported(Capability::Tree))
    }

    /// Seal and cascade one source's tree now, and report how many summaries
    /// were written.
    ///
    /// The "flush this source" control, for a user who does not want to wait
    /// for the scheduled window. Everything else in this family is addressed
    /// by *namespace*; this one is addressed by **source scope** — the
    /// `{platform}:{connection}` string a sync writes under — because that is
    /// the identity a caller has when it is looking at one connected source.
    ///
    /// # Why not `seal` plus `cascade` on the same namespace
    ///
    /// Because a source scope is not a namespace, and the mapping between them
    /// is the driver's. A source's content may sit under a tree the driver
    /// created for it, named however the driver names trees; a caller that
    /// tried to derive the namespace would be reimplementing that naming, and
    /// would get it wrong for exactly the sources whose trees were created
    /// before whatever convention it copied.
    ///
    /// It is also one operation rather than two on purpose. Sealing without
    /// cascading leaves a tier of leaves with no summary above them, which
    /// reads as an empty tree to every structural query — and a caller that
    /// made the second call separately would have a window where that is the
    /// state.
    ///
    /// # Why a count and not a tree
    ///
    /// The engine's own flush hands back a live tree object, and the caller's
    /// question is "did anything happen". A handle to a driver's internal
    /// object is precisely what this contract exists not to pass, and once the
    /// labelling decision that flush needs is made driver-side — which is
    /// where it comes from anyway — there is nothing else the object was
    /// carrying that a caller can use.
    ///
    /// # Errors
    ///
    /// [`MemoryError::Unsupported`] from a driver with a tree family but no
    /// source-scoped flush. Backend failures otherwise.
    ///
    /// A scope with nothing buffered is `Ok(0)`, not an error: idempotent for
    /// the same reason [`Self::seal`] is, so a caller may offer the control
    /// unconditionally. An **unknown** scope is also `Ok(0)` — the driver
    /// creates the tree if it has to, so there is no scope it can refuse, and
    /// a caller cannot use this to probe which scopes exist.
    async fn flush_source_tree(&self, _source_scope: &str) -> Result<u64, MemoryError> {
        Err(MemoryError::unsupported(Capability::Tree))
    }

    /// Fold `inputs` into one parent summary, using the driver's own chat
    /// provider, and report what that call cost.
    ///
    /// This is the LLM step of a seal, exposed on its own. Everything else in
    /// this family either writes content ([`Self::append`]), navigates what is
    /// already sealed, or asks the driver to run a whole seal/cascade pass
    /// ([`Self::seal`], [`Self::cascade`], [`Self::flush_source_tree`]). This
    /// one does a single fold and hands the text back, which is what a caller
    /// driving its own cascade needs and what none of the others can be made to
    /// answer: they return tree *state*, and the summary they produced is never
    /// in it.
    ///
    /// # Why the provider is the driver's and not the caller's
    ///
    /// The summariser is configured where the engine is — model, temperature,
    /// output language, rate card. A caller reaching memory over a module has
    /// none of those, so a fold it performed itself would use a different model
    /// than every fold the scheduler performs, and the two would disagree about
    /// the shape of a summary in the same tree. Passing the configuration
    /// across instead is not an option: no signature in this contract names a
    /// config type, for the reasons in [`crate::provider`].
    ///
    /// The consequence is that the usage numbers on [`SummaryOutput`] are the
    /// only record of the spend. Nothing on the caller's side saw the request.
    ///
    /// # The budgets and the ask are inputs, not hints
    ///
    /// A driver applies [`SummaryContext`]'s three token budgets exactly as
    /// given and selects its prompt from [`SummaryContext::ask`]. It does not
    /// substitute its own defaults for a budget it finds implausible: the
    /// caller owns the level it is sealing and therefore owns the budget for
    /// it, and a driver that quietly widened one would produce a node that
    /// overruns the level above. See that type for what each budget bounds and
    /// what a zero does.
    ///
    /// # Errors
    ///
    /// [`MemoryError::Unsupported`] from a driver with a tree family but no
    /// provider-backed summariser — deliberately not an empty summary, which a
    /// caller would seal as a real, blank node.
    ///
    /// [`MemoryError::Invalid`] for a [`SummaryContext::tree_kind`] the driver
    /// does not recognise. Refusing beats folding under a guessed kind: the
    /// summary is written either way and nothing afterwards records which
    /// prompt produced it.
    ///
    /// Otherwise a backend failure, which here includes the provider call — a
    /// model that errors, times out, or refuses. That is a real and recurring
    /// outcome rather than an exceptional one, and the caller is expected to
    /// have a deterministic fallback for it; the driver does not silently
    /// substitute one, because a caller cannot tell a fallback summary from a
    /// model's own work once it is in the tree.
    ///
    /// Nothing to fold is **not** an error: an empty slice, or one whose inputs
    /// are all blank, returns a default [`SummaryOutput`] with empty content and
    /// no usage. That is the same idempotence [`Self::seal`] has, and it is what
    /// lets a cascade call this unconditionally at every level.
    async fn summarise(
        &self,
        _inputs: &[SummaryInput],
        _context: &SummaryContext,
    ) -> Result<SummaryOutput, MemoryError> {
        Err(MemoryError::unsupported(Capability::Tree))
    }

    /// Every namespace's root summary, truncated to a per-namespace cap and a
    /// total cap.
    ///
    /// The top of the markdown time tree, read across all namespaces at once.
    /// Its caller is a prompt builder: this is the block of standing context a
    /// host puts in front of a model, which is why the bounds are in
    /// **characters** rather than in rows, and why the truncation happens
    /// driver-side rather than after the read. A caller that fetched whole
    /// roots and clipped them itself would pay for text it then threw away, and
    /// would clip at a boundary the driver did not choose.
    ///
    /// # Why not [`Self::summary_forest`]
    ///
    /// Different tier and different shape. The forest walks the *sealed summary
    /// forest* — one tree per ingest source, levelled by seal generation — and
    /// returns structure: ids, parents, children, no bodies. This returns the
    /// **markdown time tree**'s root body, one per namespace, and bodies are
    /// the entire point. [`crate::tree`] describes why the two live side by
    /// side.
    ///
    /// # Bounds
    ///
    /// `per_namespace_cap` clips each namespace's body; `total_cap` stops the
    /// walk once the accumulated bodies reach it, so the last body included may
    /// itself be clipped short of its own cap. Both are applied in namespace
    /// order, which is stable and alphabetical — so a total cap that binds
    /// drops the *tail* of the namespace list rather than sampling across it,
    /// exactly the reading [`SummaryForest::truncated`] warns about. A caller
    /// that needs a particular namespace represented cannot rely on a small
    /// total cap to include it.
    ///
    /// A clipped body ends in a `[... truncated]` marker; see
    /// [`RootSummary::body`].
    ///
    /// # Errors
    ///
    /// [`MemoryError::Unsupported`] from a driver with a tree family but no
    /// markdown time tree.
    ///
    /// Otherwise this is deliberately hard to fail. The read is a best-effort
    /// filesystem scan: a namespace whose root cannot be read is skipped and
    /// the rest are returned, and a workspace with no tree at all is an empty
    /// vector. That is the engine's own behaviour and the door does not
    /// manufacture an error it never produced — the result is a prompt block,
    /// and one unreadable namespace is worth less than failing the turn.
    async fn root_summaries_with_caps(
        &self,
        _per_namespace_cap: usize,
        _total_cap: usize,
    ) -> Result<Vec<RootSummary>, MemoryError> {
        Err(MemoryError::unsupported(Capability::Tree))
    }

    // ── The runtime-tree doors ───────────────────────────────────────────
    //
    // The six members below are the markdown time tree addressed node by
    // node — the doors under the host's `tree_summarizer_*` RPC surface, which
    // reported the engine's answers verbatim and therefore needs the engine's
    // exact shapes: the landing path of a buffered write, a single node as an
    // `Option`, a child list, a status row, and the node/status a
    // summarisation pass produced. [`Self::append`], [`Self::drill_down`],
    // [`Self::seal`] and [`Self::cascade`] are the same tree at a coarser
    // grain, and each one folds away a piece of the reply the RPCs carry —
    // which is why migrating that surface onto them would have changed its
    // wire format, and a door that changes what the host reports is not a
    // door, it is a new surface.

    /// Buffer raw content for the markdown time tree, answering with the path
    /// it landed at.
    ///
    /// The finer-grained sibling of [`Self::append`], which is this write with
    /// both ends trimmed off: `append` defaults a missing timestamp to the
    /// driver's "now" and discards the landing path. Here the timestamp is
    /// **required** — the caller's reply echoes the instant it filed the
    /// content under, and a timestamp resolved driver-side would disagree with
    /// the one the caller reports by however long the call took to cross —
    /// and the path comes back, because the reply names it.
    ///
    /// The path is a *report*, not an invitation: it is the driver's own
    /// spelling of the buffer file it wrote, inside the driver's workspace,
    /// and the next seal consumes it. A caller displays it; nothing should
    /// dereference it.
    ///
    /// `metadata` rides into the buffer entry's frontmatter when present, and
    /// the produced node carries it onward — the same field
    /// [`IngestRequest::metadata`] documents.
    ///
    /// # Errors
    ///
    /// [`MemoryError::Invalid`] for a rejected namespace or content that is
    /// blank after trimming, otherwise backend failures.
    async fn runtime_buffer_write(
        &self,
        _namespace: &str,
        _content: &str,
        _timestamp: DateTime<Utc>,
        _metadata: Option<serde_json::Value>,
    ) -> Result<String, MemoryError> {
        Err(MemoryError::unsupported(Capability::Tree))
    }

    /// One time-tree node, or `None` when nothing sits at `node_id`.
    ///
    /// Half of [`Self::drill_down`], unbundled — and the unbundling is the
    /// point, twice over. `drill_down` folds "no such node" into
    /// [`MemoryError::NotFound`] with its own message, which is right for
    /// navigation and wrong for the caller here, which shapes its own miss and
    /// treats "does the root exist yet" as a probe rather than a fault. And it
    /// always pays for the child read, which a caller reporting one node did
    /// not ask for.
    ///
    /// # Errors
    ///
    /// [`MemoryError::Invalid`] for a rejected namespace or a `node_id` that
    /// is not `root` / `YYYY[/MM[/DD[/HH]]]`-shaped, otherwise backend
    /// failures. Absence is `Ok(None)`, never an error.
    async fn runtime_read_node(
        &self,
        _namespace: &str,
        _node_id: &str,
    ) -> Result<Option<TreeNode>, MemoryError> {
        Err(MemoryError::unsupported(Capability::Tree))
    }

    /// The direct children of one time-tree node, in the store's own order.
    ///
    /// The other half of [`Self::drill_down`], on the same terms as
    /// [`Self::runtime_read_node`]. A parent that does not exist has no
    /// children: an empty vector, not an error — the same answer an hour leaf
    /// gives, because a leaf has nothing under it by construction.
    ///
    /// # Errors
    ///
    /// [`MemoryError::Invalid`] on the same terms as
    /// [`Self::runtime_read_node`], otherwise backend failures.
    async fn runtime_read_children(
        &self,
        _namespace: &str,
        _parent_id: &str,
    ) -> Result<Vec<TreeNode>, MemoryError> {
        Err(MemoryError::unsupported(Capability::Tree))
    }

    /// One namespace's time-tree shape: node count, depth, and coverage.
    ///
    /// The read [`Self::seal`] and [`Self::cascade`] already answer with —
    /// exposed on its own so a status panel can poll it without running the
    /// pass it describes. A namespace with no tree yet is the all-empty
    /// status, not an error: zero nodes, zero depth, every timestamp `None`.
    ///
    /// # Errors
    ///
    /// [`MemoryError::Invalid`] for a rejected namespace, otherwise backend
    /// failures.
    async fn runtime_tree_status(&self, _namespace: &str) -> Result<TreeStatus, MemoryError> {
        Err(MemoryError::unsupported(Capability::Tree))
    }

    /// Drain the namespace's buffer into hour leaves and propagate upward,
    /// answering with the last hour node written.
    ///
    /// [`Self::seal`] runs the same pass and answers with tree *state*; this
    /// answers with the *work* — the node the pass produced, which is what the
    /// triggering surface reports ("node `2024/03/15/09`, 340 tokens") and
    /// what a status row cannot be unfolded into. `Ok(None)` is a pass that
    /// found nothing buffered, which is a successful no-op on the same terms
    /// as `seal`'s idempotence.
    ///
    /// The fold runs on the driver's own chat provider, built the way every
    /// scheduled seal builds it — see [`Self::summarise`] for why the provider
    /// cannot be the caller's, and note the consequence is the same here: the
    /// spend happens driver-side, under the driver's configuration.
    ///
    /// `timestamp` is the instant the pass files freshly-drained content
    /// under, supplied by the caller for the reason
    /// [`Self::runtime_buffer_write`] gives.
    ///
    /// # Errors
    ///
    /// [`MemoryError::Invalid`] for a rejected namespace — checked before the
    /// provider is built, so a bad namespace answers the same with or without
    /// one configured. A driver whose summarisation provider cannot be
    /// resolved fails even when the buffer is empty: the caller offered a
    /// "run now" control, and "nothing to do" from a runner that could not
    /// have run is the lie a disabled control exists to avoid. Otherwise
    /// backend failures, which include the provider call itself.
    async fn runtime_summarize(
        &self,
        _namespace: &str,
        _timestamp: DateTime<Utc>,
    ) -> Result<Option<TreeNode>, MemoryError> {
        Err(MemoryError::unsupported(Capability::Tree))
    }

    /// Rebuild the whole time tree above the hour leaves, answering with the
    /// resulting status.
    ///
    /// [`Self::cascade`] with one behavioural difference, preserved on
    /// purpose: `cascade` short-circuits an empty tree without touching the
    /// provider, because a scheduler calls it unconditionally. This member is
    /// a person's explicit "rebuild now", and it resolves the provider
    /// *first* — on the terms [`Self::runtime_summarize`] gives — so a setup
    /// that could never rebuild says so instead of reporting an empty rebuild
    /// that ran nothing.
    ///
    /// # Errors
    ///
    /// As [`Self::runtime_summarize`].
    async fn runtime_rebuild(&self, _namespace: &str) -> Result<TreeStatus, MemoryError> {
        Err(MemoryError::unsupported(Capability::Tree))
    }

    /// The compiled flavoured-root profile for one tree scope, front-matter
    /// included — or `None` while nothing has been distilled for it.
    ///
    /// The read behind a persona tool: flavoured trees are sealed under a
    /// standing ask (see [`SummaryContext::ask`]), and their root compiles to
    /// a small fixed-path markdown artifact. This member collapses the whole
    /// lookup the host used to run against the engine directly — try the
    /// compiled artifact, fall back to the tree, recompile its root — behind
    /// one scope-shaped question.
    ///
    /// # The split with the caller
    ///
    /// The driver owns the lookup and the built/not-built verdict; the caller
    /// owns the vocabulary and the presentation. Scope strings like
    /// `persona/communication` are the caller's naming scheme — the driver
    /// matches them literally and validates nothing about their shape beyond
    /// non-emptiness, so an unknown scope is indistinguishable from an unbuilt
    /// one, deliberately: both answer `Ok(None)`, and only the caller knows
    /// which scopes it ever writes. The returned markdown is the **full
    /// compiled artifact including its front-matter**, because the front
    /// matter is part of what was compiled; a caller that wants only the prose
    /// strips it, and that choice stays on the caller's side of the wire.
    ///
    /// # `None` versus empty
    ///
    /// `Ok(None)` means *not built*: no flavoured tree for the scope, or a
    /// tree whose compiled root has an empty body — a tree that exists but has
    /// never sealed compiles to front-matter over nothing, and a profile with
    /// no prose is not a profile. A driver must never answer `Ok(Some)` with a
    /// body-less artifact, because the caller hands the body to a model and an
    /// empty string reads as "this person has no communication style".
    ///
    /// # Errors
    ///
    /// [`MemoryError::Invalid`] for a blank scope. Otherwise backend failures
    /// — the tree lookup or the compile step failing is an error, distinct
    /// from `None`, because "could not read the store" reported as "not built
    /// yet" tells a user to re-run an ingestion that already worked.
    async fn flavour_profile(&self, _scope: &str) -> Result<Option<String>, MemoryError> {
        Err(MemoryError::unsupported(Capability::Tree))
    }
}
