//! OpenHuman service adapters for tinycortex live synchronization.

use async_trait::async_trait;
use std::sync::Arc;
use tinycortex::memory::sync::{
    ExternalSourceReader, GithubRepoSyncPipeline, LocalDocument, LocalDocumentSink, SkillDocSink,
    SkillDocument, SyncContext, SyncDispatcher, SyncEvent, SyncEventSink, SyncOutcome,
    SyncPipeline, SyncStage, SyncStateStore, WorkspaceSourcePipeline,
};

use crate::sources::{MemorySourceEntry, SourceKind};
use crate::store::MemoryClientRef;
use crate::Config;

/// The KV namespace Composio sync state is persisted under.
///
/// Re-exported from the engine rather than re-declared. It was a second
/// `const` holding the same literal as
/// `tinycortex::memory::sync::state::STATE_NAMESPACE`, so the host and the
/// engine agreed only by coincidence of the string: change either and the two
/// would silently read and write *different* namespaces, stranding every
/// persisted sync cursor with no error anywhere. A duplicated literal is a
/// drift hazard precisely when the thing it names is durable (#18 §B2).
pub use tinycortex::memory::sync::state::STATE_NAMESPACE as HOST_SYNC_STATE_NAMESPACE;
pub use tinycortex::memory::sync::{RawCoverage, RawFileRef, RealCostAccumulator, RebuildOutcome};

pub struct HostSyncAdapter {
    memory: MemoryClientRef,
    config: Option<Arc<Config>>,
    /// Items whose skill-store write committed but whose (non-corrupt) tree
    /// ingest failed during this adapter's run — the tolerated warns in
    /// `store()`. Read back by [`run_source_pipeline_core`] so the run's
    /// verdict can report "fetched, not tree-ingested" (openhuman#5820).
    tree_ingest_failures: std::sync::atomic::AtomicU32,
}

#[derive(Debug)]
pub struct SourcePipelineFailure {
    pub message: String,
    pub actions_called: u32,
    pub provider_cost_usd: f64,
}

impl std::fmt::Display for SourcePipelineFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl SourcePipelineFailure {
    fn without_usage(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            actions_called: 0,
            provider_cost_usd: 0.0,
        }
    }
}

/// [`SyncOutcome`] plus `tree_ingest_failures` — the count of items whose
/// fetch-and-store committed but whose memory-tree ingest did not
/// (openhuman#5820). The vendored engine's own `SyncOutcome` has no field for
/// this, so [`run_source_pipeline_core`] returns this richer type instead and
/// [`run_source_pipeline`] converts down to the engine type for callers that
/// do not need the tree half's verdict.
pub(crate) struct SourcePipelineOutcome {
    pub records_ingested: u32,
    pub more_pending: bool,
    pub actions_called: u32,
    pub provider_cost_usd: f64,
    pub note: Option<String>,
    pub tree_ingest_failures: u32,
}

impl HostSyncAdapter {
    pub fn new(memory: MemoryClientRef) -> Self {
        Self {
            memory,
            config: None,
            tree_ingest_failures: std::sync::atomic::AtomicU32::new(0),
        }
    }

    fn with_config(memory: MemoryClientRef, config: Arc<Config>) -> Self {
        Self {
            memory,
            config: Some(config),
            tree_ingest_failures: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// Tolerated (non-corrupt) tree-ingest failures recorded so far.
    fn tree_ingest_failure_count(&self) -> u32 {
        self.tree_ingest_failures
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Reconnect one synced connector item to the memory tree (#5473).
///
/// The TinyCortex migration (#4794) dropped the per-provider tree-ingest half of
/// the connector sync: synced items reached the `skill-<toolkit>` document store
/// but never `mem_tree_chunks`, so connector memories fell out of tree-backed
/// recall. This routes each synced item through the engine's document ingest —
/// the same L0-chunk path local folder sources use via [`LocalDocumentSink`] —
/// additively alongside whichever store holds the item.
///
/// A `pub` free function rather than a method, because owning these rules in ONE
/// place is the actual fix for openhuman#6007. #5473 put them on
/// [`HostSyncAdapter`], the *old* `SkillDocSink` path's adapter. The connector
/// migration then built a second ingest path — `MemorySourceSink::accept_source_items`
/// in `tinymemory-tinycortex` — which wrote namespace documents and vector chunks
/// but never learned the tree half, so Gmail synced, embedded, and stayed
/// invisible to every tree-backed surface. A fix that patched only that second
/// call site would leave the same trap for a third. Both paths now call this.
///
/// Scope naming matches the tree retrieval contract: the tree scope
/// (`path_scope`) is `"{toolkit}:{connection_id}"` so `query_source` resolves it
/// by platform prefix (`gmail:` → email, `slack:` → chat, …), while the per-item
/// `source_id` carries the item id so each message admits independently rather
/// than colliding on one dedup key. That pair is *also* the literal prefix
/// OpenHuman counts a Composio source's ingest by (`"{toolkit}:{connection_id}:"`,
/// its `source_id_prefix`), so a drift here empties the source row's status and
/// the memory graph together — silently, because the documents and vectors are
/// still written.
///
/// `ingest_document_with_scope` writes the L0 chunk rows synchronously and
/// enqueues the summary seal on the async extract worker. Retrieval
/// (`query_source`) reads sealed summaries, so an item becomes retrievable once
/// its buffer seals — on the token threshold or the time-based
/// `flush_stale_buffers` — and the seal degrades to a fallback summary when no
/// LLM is available. Chunk rows existing while recall is still thin is that
/// latency, not a second bug.
///
/// Answers `Ok(None)` when the item was skipped for want of a scope, and
/// `Ok(Some(result))` when it reached the pipeline. Callers that only care
/// whether it failed drop the payload; the backfill (#6012) reads
/// `already_ingested` off it to tell a document it has just treed from one the
/// tree already held. That distinction has to come from here rather than from a
/// second call site re-deriving the scope rules, because re-deriving them is
/// what caused openhuman#6007 in the first place.
pub async fn ingest_connector_item_into_tree(
    config: &Config,
    toolkit: &str,
    connection_id: &str,
    item_id: &str,
    title: &str,
    content: &str,
) -> anyhow::Result<Option<crate::ingest_pipeline::IngestResult>> {
    // The caller's own store still holds a scopeless item; it is only the tree
    // that skips it.
    let Some(identity) = connector_item_identity(toolkit, connection_id, item_id) else {
        tracing::debug!(
            item_id = %item_id,
            "[tinycortex:sync] skipping memory-tree ingest: item has no toolkit/connection scope"
        );
        return Ok(None);
    };
    let ConnectorItemIdentity {
        tree_scope,
        source_id,
        owner,
        toolkit,
    } = identity;
    let input = tinycortex::memory::ingest::canonicalize::document::DocumentInput {
        provider: format!("composio:{toolkit}"),
        title: title.to_string(),
        body: content.to_string(),
        modified_at: chrono::Utc::now(),
        source_ref: Some(item_id.to_string()),
    };
    crate::ingest_pipeline::ingest_document_with_scope(
        config,
        &source_id,
        &owner,
        vec![toolkit],
        input,
        Some(tree_scope),
    )
    .await
    .map(Some)
    .map_err(|error| anyhow::anyhow!("memory-tree ingest failed for source `{source_id}`: {error}"))
}

/// The tree identity of one connector item, derived once for every reader and
/// writer of it.
///
/// Three names that must agree with each other and with what the sync path
/// wrote: the tree scope, the per-item source id under it, and the owner. They
/// are built here and nowhere else, so a second reader or writer of the tree
/// cannot spell them differently from the funnel. Two call sites owning this
/// rule is what produced openhuman#6007.
struct ConnectorItemIdentity {
    /// `{toolkit}:{connection_id}` — the `path_scope` retrieval resolves by
    /// platform prefix, and the literal prefix OpenHuman counts a source's
    /// ingest by.
    tree_scope: String,
    /// `{tree_scope}:{item_id}` — the ingest gate's key, one per item so each
    /// message admits independently.
    source_id: String,
    /// `{toolkit}-sync:{connection_id}`.
    owner: String,
    /// The normalised toolkit, for the ingest tag and provider name.
    toolkit: String,
}

/// Derives the identity, or `None` when either scope half is blank.
///
/// A blank toolkit/connection would yield a scope with no platform prefix
/// (`":conn"`), which no retrieval kind matches; callers skip rather than write
/// an unreachable tree.
fn connector_item_identity(
    toolkit: &str,
    connection_id: &str,
    item_id: &str,
) -> Option<ConnectorItemIdentity> {
    let toolkit = toolkit.trim().to_ascii_lowercase();
    let connection_id = connection_id.trim();
    if toolkit.is_empty() || connection_id.is_empty() {
        return None;
    }
    let tree_scope = format!("{toolkit}:{connection_id}");
    Some(ConnectorItemIdentity {
        source_id: format!("{tree_scope}:{item_id}"),
        owner: format!("{toolkit}-sync:{connection_id}"),
        tree_scope,
        toolkit,
    })
}

/// [`ingest_connector_item_into_tree`] plus the failure policy every connector
/// sync path needs, so no path has to reach for `crate::corruption` itself.
///
/// The tree is a secondary index over a store that has *already* committed, so an
/// ordinary failure here must NOT abort the sync run. Most providers do not
/// tolerate scope errors, so a propagated error becomes a run-aborting `Err` in
/// the orchestrator, and one deterministically-poisonous item then stalls the
/// whole connection and re-fetches the page — real Composio spend — on every
/// retry. Count it, warn, and continue: the per-item source gate re-attempts the
/// item on a later sync, and an operator rebuild can backfill.
///
/// Corruption is the exception (openhuman#5820). A malformed `chunks.db` fails
/// every later item identically, so it escalates through the shared recovery and
/// aborts the run — there is nothing per-item about it. That split belongs to
/// `crate::corruption::escalate_or_count`, which stays `pub(crate)` deliberately:
/// callers reach the *policy* through this function rather than the primitive, so
/// a new call site cannot quietly implement a weaker one.
pub async fn ingest_connector_item_tolerated(
    config: &Config,
    toolkit: &str,
    connection_id: &str,
    item_id: &str,
    title: &str,
    content: &str,
    counter: &std::sync::atomic::AtomicU32,
) -> anyhow::Result<()> {
    if let Err(error) =
        ingest_connector_item_into_tree(config, toolkit, connection_id, item_id, title, content)
            .await
            .map(|_| ())
    {
        let rendered = format!("{error:#}");
        crate::corruption::escalate_or_count("connector tree ingest", config, error, counter)?;
        tracing::warn!(
            toolkit = %toolkit,
            connection_id = %connection_id,
            item_id = %item_id,
            error = %rendered,
            "[tinycortex:sync] memory-tree ingest failed; the item's own store write is retained"
        );
    }
    Ok(())
}

/// Read persisted sync audit records for best-effort RPC and reporting surfaces.
///
/// Backed by `crate::sync::audit` — the host-owned log — not the engine;
/// this stays in the engine module only because OpenHuman reaches it through
/// the engine shim path.
pub fn read_audit_log(config: &Config) -> Vec<crate::sync::audit::SyncAuditEntry> {
    crate::sync::audit::read_audit_log(config.workspace_dir()).unwrap_or_default()
}

/// Estimate sync inference cost using TinyCortex's canonical pricing model.
/// Delegates to the host-owned pricing (#18 §B1); kept because OpenHuman
/// reaches it through the engine shim path.
pub fn estimate_cost_usd(input_tokens: u64, output_tokens: u64) -> f64 {
    crate::sync::audit::estimate_cost_usd(input_tokens, output_tokens)
}

/// Measure coverage of a raw archive by its TinyCortex memory tree.
pub fn raw_coverage(
    config: &Config,
    tree_scope: &str,
    archive_source_id: &str,
) -> anyhow::Result<RawCoverage> {
    tracing::debug!("[tinycortex:sync] raw coverage scan starting");
    let memory_config = super::memory_config_from(config, config.workspace_dir().clone());
    let coverage =
        tinycortex::memory::sync::raw_coverage(&memory_config, tree_scope, archive_source_id)
            .map_err(|error| {
                tracing::warn!(%error, "[tinycortex:sync] raw coverage scan failed");
                error
            })?;
    tracing::debug!(
        total = coverage.total,
        covered = coverage.covered,
        pending = coverage.pending.len(),
        "[tinycortex:sync] raw coverage scan completed"
    );
    Ok(coverage)
}

/// Return whether a raw archive contains records absent from its memory tree.
pub fn needs_rebuild(config: &Config, tree_scope: &str, archive_source_id: &str) -> bool {
    let memory_config = super::memory_config_from(config, config.workspace_dir().clone());
    let required =
        tinycortex::memory::sync::needs_rebuild(&memory_config, tree_scope, archive_source_id);
    tracing::debug!(
        required,
        "[tinycortex:sync] raw rebuild requirement evaluated"
    );
    required
}

/// Rebuild a memory tree from its raw archive through the host summarizer.
pub async fn rebuild_tree_from_raw(
    config: &Config,
    tree_scope: &str,
    archive_source_id: &str,
) -> anyhow::Result<RebuildOutcome> {
    tracing::info!("[tinycortex:sync] raw rebuild starting");
    let memory_config = super::memory_config_from(config, config.workspace_dir().clone());
    let summariser = super::HostSummariser::new(config.to_arc());
    let outcome = tinycortex::memory::sync::rebuild_tree_from_raw(
        &memory_config,
        tree_scope,
        archive_source_id,
        &summariser,
    )
    .await
    .map_err(|error| {
        tracing::warn!(%error, "[tinycortex:sync] raw rebuild failed");
        error
    })?;
    tracing::info!(
        files_read = outcome.files_read,
        batches = outcome.batches,
        "[tinycortex:sync] raw rebuild completed"
    );
    Ok(outcome)
}

/// Run a registered GitHub repository source through TinyCortex synchronization.
pub async fn run_github_sync(
    source: &MemorySourceEntry,
    config: &Config,
) -> anyhow::Result<SyncOutcome> {
    tracing::info!("[tinycortex:sync] GitHub repository sync starting");
    if crate::global::client_if_ready().is_none() {
        tracing::debug!("[tinycortex:sync] GitHub sync initializing memory client");
        crate::global::init(config.workspace_dir().clone())
            .map_err(anyhow::Error::msg)
            .map_err(|error| {
                tracing::warn!(%error, "[tinycortex:sync] GitHub sync memory initialization failed");
                error
            })?;
    }
    let outcome = run_source_pipeline(source, config)
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))
        .map_err(|error| {
            tracing::warn!(%error, "[tinycortex:sync] GitHub repository sync failed");
            error
        })?;
    tracing::info!(
        records_ingested = outcome.records_ingested,
        more_pending = outcome.more_pending,
        actions_called = outcome.actions_called,
        "[tinycortex:sync] GitHub repository sync completed"
    );
    Ok(outcome)
}

#[async_trait]
impl ExternalSourceReader for HostSyncAdapter {
    async fn list_items(
        &self,
        source: &tinycortex::memory::sources::MemorySourceEntry,
    ) -> anyhow::Result<Vec<tinycortex::memory::sources::SourceItem>> {
        let config = self
            .config
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("external source reader requires host config"))?;
        let host_source: MemorySourceEntry = serde_json::from_value(serde_json::to_value(source)?)?;
        // A kind with no reader is an error, not an empty listing: answering
        // "0 items" for a source nobody read would let the caller record the
        // sync as complete and move its cursor past everything it skipped.
        let reader = crate::sources::readers::reader_for(&host_source.kind).ok_or_else(|| {
            anyhow::anyhow!(
                "no reader for source kind {:?}: it is fetched outside this crate",
                host_source.kind
            )
        })?;
        let items = reader
            .list_items(&host_source, &**config)
            .await
            .map_err(anyhow::Error::msg)?;
        serde_json::from_value(serde_json::to_value(items)?).map_err(Into::into)
    }

    async fn read_item(
        &self,
        source: &tinycortex::memory::sources::MemorySourceEntry,
        item_id: &str,
    ) -> anyhow::Result<tinycortex::memory::sources::SourceContent> {
        let config = self
            .config
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("external source reader requires host config"))?;
        let host_source: MemorySourceEntry = serde_json::from_value(serde_json::to_value(source)?)?;
        let reader = crate::sources::readers::reader_for(&host_source.kind).ok_or_else(|| {
            anyhow::anyhow!(
                "no reader for source kind {:?}: it is fetched outside this crate",
                host_source.kind
            )
        })?;
        let content = reader
            .read_item(&host_source, item_id, &**config)
            .await
            .map_err(anyhow::Error::msg)?;
        serde_json::from_value(serde_json::to_value(content)?).map_err(Into::into)
    }
}

pub fn sync_context(memory: MemoryClientRef) -> SyncContext {
    let adapter = std::sync::Arc::new(HostSyncAdapter::new(memory));
    SyncContext {
        events: adapter.clone(),
        documents: adapter.clone(),
        state: adapter,
        local_documents: None,
        external_sources: None,
        summariser: None,
    }
}

/// [`source_sync_context`] over a caller-held adapter, so the caller can read
/// the adapter's per-run counters after the pipeline finishes.
fn context_over_adapter(
    adapter: std::sync::Arc<HostSyncAdapter>,
    config: &Config,
    local: bool,
) -> SyncContext {
    SyncContext {
        events: adapter.clone(),
        documents: adapter.clone(),
        state: adapter.clone(),
        local_documents: local.then(|| adapter.clone() as std::sync::Arc<dyn LocalDocumentSink>),
        external_sources: local.then_some(adapter as std::sync::Arc<dyn ExternalSourceReader>),
        summariser: local.then(|| {
            std::sync::Arc::new(super::HostSummariser::new(config.to_arc()))
                as std::sync::Arc<dyn tinycortex::memory::tree::Summariser>
        }),
    }
}

pub async fn run_source_pipeline(
    source: &MemorySourceEntry,
    config: &Config,
) -> Result<SyncOutcome, SourcePipelineFailure> {
    // Engine-typed view over `run_source_pipeline_core` for callers that speak
    // the engine's `SyncOutcome`. The conversion drops `tree_ingest_failures`
    // (the engine type has no field for it) — a caller that must see the tree
    // half's verdict calls the `_core` variant instead.
    let outcome = run_source_pipeline_core(source, config).await?;
    Ok(SyncOutcome {
        records_ingested: outcome.records_ingested,
        more_pending: outcome.more_pending,
        actions_called: outcome.actions_called,
        provider_cost_usd: outcome.provider_cost_usd,
        note: outcome.note,
    })
}

/// [`run_source_pipeline`] returning [`SourcePipelineOutcome`], which
/// additionally carries `tree_ingest_failures` — the "fetch committed, tree
/// ingest did not" count a sync verdict must not launder into success
/// (openhuman#5820). The engine's outcome type stays untouched; this is the
/// boundary where the richer count would otherwise be dropped. Crate-private:
/// it is the seam `crate::sources::sync` reads through, not host surface.
pub(crate) async fn run_source_pipeline_core(
    source: &MemorySourceEntry,
    config: &Config,
) -> Result<SourcePipelineOutcome, SourcePipelineFailure> {
    // Composio sources are read by the connector module, not here: reaching a
    // connected account needs a credential this crate does not hold and must
    // not. The host fetches through `tinyconnectors` and hands the records
    // back through `MemorySourceSink::accept_source_items`.
    //
    // Refused rather than skipped. A pipeline that answered "0 records, no
    // error" for a source it never read would advance the caller's cursor past
    // items nobody looked at, and report a healthy sync while the user's mail
    // stopped arriving.
    if source.kind == SourceKind::Composio {
        return Err(SourcePipelineFailure::without_usage(
            "composio sources are synced through the connector module, not this pipeline",
        ));
    }

    let memory = crate::global::client_if_ready()
        .ok_or_else(|| SourcePipelineFailure::without_usage("memory client is not ready"))?;
    let mut memory_config = super::memory_config_from(config, config.workspace_dir().clone());
    memory_config.sync.interval_secs = config.memory_sync_interval_secs();
    memory_config.sync.budget.max_items = source.max_items;
    memory_config.sync.budget.max_tokens_per_sync = source.max_tokens_per_sync;
    memory_config.sync.budget.max_cost_per_sync_usd = source.max_cost_per_sync_usd;
    memory_config.sync.budget.sync_depth_days = source.sync_depth_days;

    let pipeline = build_pipeline(source, config, &mut memory_config)
        .map_err(SourcePipelineFailure::without_usage)?;
    let pipeline_id = pipeline.id().to_owned();
    let mut dispatcher = SyncDispatcher::new();
    dispatcher
        .register(pipeline)
        .map_err(|error| SourcePipelineFailure::without_usage(error.to_string()))?;
    // Built from an adapter handle this fn keeps, rather than through
    // `source_sync_context`, so the tolerated tree-ingest failure count can be
    // read back after the run.
    let adapter = std::sync::Arc::new(HostSyncAdapter::with_config(memory, config.to_arc()));
    let context =
        context_over_adapter(adapter.clone(), config, source.kind != SourceKind::Composio);
    let outcome = dispatcher
        .tick(&pipeline_id, &memory_config, &context)
        .await
        .map_err(|error| {
            let usage = error.downcast_ref::<tinycortex::memory::sync::SyncRunError>();
            SourcePipelineFailure {
                message: error.to_string(),
                actions_called: usage.map_or(0, |error| error.actions_called),
                provider_cost_usd: usage.map_or(0.0, |error| error.provider_cost_usd),
            }
        })?;
    Ok(SourcePipelineOutcome {
        records_ingested: outcome.records_ingested,
        more_pending: outcome.more_pending,
        actions_called: outcome.actions_called,
        provider_cost_usd: outcome.provider_cost_usd,
        note: outcome.note,
        tree_ingest_failures: adapter.tree_ingest_failure_count(),
    })
}

fn build_pipeline(
    source: &MemorySourceEntry,
    _config: &Config,
    _memory_config: &mut tinycortex::memory::config::MemoryConfig,
) -> Result<std::sync::Arc<dyn SyncPipeline>, String> {
    if source.kind != SourceKind::Composio {
        let crate_source: tinycortex::memory::sources::MemorySourceEntry = serde_json::from_value(
            serde_json::to_value(source).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        if source.kind == SourceKind::GithubRepo {
            return GithubRepoSyncPipeline::new(crate_source)
                .map(|pipeline| std::sync::Arc::new(pipeline) as std::sync::Arc<dyn SyncPipeline>)
                .map_err(|error| error.to_string());
        }
        return WorkspaceSourcePipeline::new(crate_source)
            .map(|pipeline| std::sync::Arc::new(pipeline) as std::sync::Arc<dyn SyncPipeline>)
            .map_err(|error| error.to_string());
    }

    // Composio sources never reach this seam: `run_source_pipeline` routes
    // them to `crate::sync::pipelines` (#18 §B1) before building. Only the
    // tree-coupled kinds are built here.
    Err(format!(
        "engine seam does not build composio pipelines (kind {:?} unexpected here)",
        source.kind
    ))
}

#[async_trait]
impl SkillDocSink for HostSyncAdapter {
    async fn store(&self, document: SkillDocument) -> anyhow::Result<()> {
        tracing::debug!(
            toolkit = %document.toolkit,
            connection_id = %document.connection_id,
            document_id = %document.document_id,
            "[tinycortex:sync] storing synchronized document"
        );
        self.memory
            .store_skill_sync(
                &document.namespace_skill_id,
                &document.connection_id,
                &document.title,
                &document.content,
                Some("tinycortex-sync".into()),
                Some(document.metadata.clone()),
                Some("medium".into()),
                None,
                None,
                Some(document.document_id.clone()),
            )
            .await
            .map_err(anyhow::Error::msg)?;

        // #5473: additively reconnect the synced item to the memory tree. The
        // skill store above is the source of truth and has already committed;
        // `ingest_connector_item_tolerated` owns both the scope rules and the
        // best-effort-except-corruption policy, so this path and the connector
        // path in `tinymemory-tinycortex` cannot drift apart again (openhuman#6007).
        //
        // The config-less adapter (`sync_context`) has no ingest pipeline and is
        // not on the connector sync path, so it skips tree ingest entirely.
        if let Some(config) = self.config.as_deref() {
            ingest_connector_item_tolerated(
                config,
                &document.toolkit,
                &document.connection_id,
                &document.document_id,
                &document.title,
                &document.content,
                &self.tree_ingest_failures,
            )
            .await?;
        }
        Ok(())
    }

    async fn delete(&self, namespace_skill_id: &str, document_id: &str) -> anyhow::Result<()> {
        let namespace = format!("skill-{}", namespace_skill_id.trim());
        tracing::debug!(
            namespace,
            document_id,
            "[tinycortex:sync] deleting synchronized document"
        );
        self.memory
            .delete_document(&namespace, document_id)
            .await
            .map(|_| ())
            .map_err(anyhow::Error::msg)
    }
}

#[async_trait]
impl LocalDocumentSink for HostSyncAdapter {
    async fn upsert(&self, document: LocalDocument) -> anyhow::Result<()> {
        let config = self
            .config
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("local document sink missing host config"))?;
        let input = tinycortex::memory::ingest::canonicalize::document::DocumentInput {
            provider: "memory_sources:local".into(),
            title: document.title,
            body: document.body,
            modified_at: document.modified_at,
            source_ref: document.source_ref,
        };
        crate::ingest_pipeline::ingest_document_with_scope(
            &**config,
            &document.source_id,
            &document.owner,
            document.tags,
            input,
            document.path_scope,
        )
        .await
        .map(|_| ())
        .map_err(anyhow::Error::msg)
    }

    async fn delete(&self, source_id: &str) -> anyhow::Result<()> {
        let config = self
            .config
            .clone()
            .ok_or_else(|| anyhow::anyhow!("local document sink missing host config"))?;
        let source_id = source_id.to_owned();
        tokio::task::spawn_blocking(move || {
            crate::store::chunks::store::delete_chunks_by_source(
                &*config,
                crate::store::chunks::types::SourceKind::Document,
                &source_id,
            )
        })
        .await
        .map_err(|error| anyhow::anyhow!("local delete task failed: {error}"))??;
        Ok(())
    }
}

#[async_trait]
impl SyncStateStore for HostSyncAdapter {
    async fn get(&self, namespace: &str, key: &str) -> anyhow::Result<Option<serde_json::Value>> {
        self.memory
            .kv_get(Some(namespace), key)
            .await
            .map_err(anyhow::Error::msg)
    }

    async fn set(
        &self,
        namespace: &str,
        key: &str,
        value: &serde_json::Value,
    ) -> anyhow::Result<()> {
        self.memory
            .kv_set(Some(namespace), key, value)
            .await
            .map_err(anyhow::Error::msg)
    }
}

#[async_trait]
impl SyncEventSink for HostSyncAdapter {
    async fn emit(&self, event: SyncEvent) -> anyhow::Result<()> {
        crate::events::publish(crate::events::MemoryEvent::SyncStageChanged {
            trigger: "tinycortex".into(),
            stage: stage_name(event.stage).into(),
            provider: Some(event.toolkit),
            connection_id: event.connection_id,
            detail: event.message,
            source_id: Some(event.source_id),
        });
        Ok(())
    }
}

fn stage_name(stage: SyncStage) -> &'static str {
    match stage {
        SyncStage::Requested => "requested",
        SyncStage::Fetching => "fetching",
        SyncStage::Stored => "stored",
        SyncStage::Ingesting => "ingesting",
        SyncStage::Completed => "completed",
        SyncStage::Failed => "failed",
    }
}

#[cfg(test)]
#[path = "sync_tests.rs"]
mod tests;
