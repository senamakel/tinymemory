//! The import sections and how each pages through its legacy table.
//!
//! A section scans its table in pages ordered by a stable key and returns one
//! [`Scanned`] per row (or per group of rows): the key, as a [`Mark`] that
//! advances a [`Checkpoint`], and the item the row maps to, or `None` when the
//! row is skipped (it belongs to another section, is a raw event, is blank,
//! or was dropped). Skipped rows still advance the scan, so a page of skipped
//! rows never stalls the iterator.

mod chunks;
pub(crate) mod connector;
mod episodic;
mod events;
mod files;
mod graph;
mod memory_docs;
mod profile;

pub use memory_docs::EXTERNAL_SYNC_TAG;

use tinymemory_api::{MemoryMeta, SourceKind, StoreItem};

use crate::import::checkpoint::{Checkpoint, ChunkCursor};
use crate::import::error::Result;
use crate::import::workspace::LegacyWorkspace;

/// One import section, in the order [`ORDER`] walks them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    /// `memory_docs` rows that are documents.
    Documents,
    /// `memory_tree/chunks.db` sources.
    Chunks,
    /// `episodic_log` threads.
    Conversations,
    /// `memory_docs` rows that are learnings or `global`.
    Learnings,
    /// `user_profile` facets.
    Profile,
    /// `event_log` events.
    Events,
    /// `episodic_log` lessons.
    Lessons,
    /// `graph_global` relations.
    GraphGlobal,
    /// `graph_namespace` relations.
    GraphNamespace,
    /// Workspace files: the goals document and persona directives.
    Files,
}

/// The fixed section order.
///
/// New sections are appended, never inserted: a checkpoint persisted before
/// a section existed then still resumes exactly.
pub(crate) const ORDER: [Section; 10] = [
    Section::Documents,
    Section::Chunks,
    Section::Conversations,
    Section::Learnings,
    Section::Profile,
    Section::Events,
    Section::Lessons,
    Section::GraphGlobal,
    Section::GraphNamespace,
    Section::Files,
];

/// A scanned key, naming the checkpoint field it advances.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Mark {
    /// A `memory_docs.document_id` in the documents section.
    Document(String),
    /// A chunk source.
    Chunk(ChunkCursor),
    /// An `episodic_log.session_id`.
    Conversation(String),
    /// A `memory_docs.document_id` in the learnings section.
    Learning(String),
    /// A `user_profile.facet_id`.
    Profile(String),
    /// An `event_log.event_id`.
    Event(String),
    /// An `episodic_log.id` with a lesson.
    Lesson(i64),
    /// A `graph_global` rowid.
    GraphGlobal(i64),
    /// A `graph_namespace` rowid.
    GraphNamespace(i64),
    /// A workspace file's name.
    File(String),
}

impl Mark {
    /// Records this key as the section's position in `checkpoint`.
    pub(crate) fn apply(self, checkpoint: &mut Checkpoint) {
        match self {
            Self::Document(id) => checkpoint.documents = Some(id),
            Self::Chunk(cursor) => checkpoint.chunks = Some(cursor),
            Self::Conversation(id) => checkpoint.conversations = Some(id),
            Self::Learning(id) => checkpoint.learnings = Some(id),
            Self::Profile(id) => checkpoint.profile = Some(id),
            Self::Event(id) => checkpoint.events = Some(id),
            Self::Lesson(id) => checkpoint.lessons = Some(id),
            Self::GraphGlobal(id) => checkpoint.graph_global = Some(id),
            Self::GraphNamespace(id) => checkpoint.graph_namespace = Some(id),
            Self::File(name) => checkpoint.files = Some(name),
        }
    }
}

/// One scanned row or row group.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Scanned {
    /// Its key.
    pub(crate) mark: Mark,
    /// The item it maps to, or `None` when skipped.
    pub(crate) item: Option<StoreItem>,
}

impl Section {
    /// The next page of at most `limit` keys after this section's position in
    /// `scan`. An empty page means the section is exhausted.
    pub(crate) fn page(
        self,
        ws: &LegacyWorkspace,
        scan: &Checkpoint,
        limit: usize,
    ) -> Result<Vec<Scanned>> {
        match self {
            Self::Documents => memory_docs::documents(ws, scan.documents.as_deref(), limit),
            Self::Chunks => chunks::page(ws, scan.chunks.as_ref(), limit),
            Self::Conversations => episodic::page(ws, scan.conversations.as_deref(), limit),
            Self::Learnings => memory_docs::learnings(ws, scan.learnings.as_deref(), limit),
            Self::Profile => profile::page(ws, scan.profile.as_deref(), limit),
            Self::Events => events::page(ws, scan.events.as_deref(), limit),
            Self::Lessons => episodic::lessons(ws, scan.lessons, limit),
            Self::GraphGlobal => graph::page(ws, graph::Table::Global, scan.graph_global, limit),
            Self::GraphNamespace => {
                graph::page(ws, graph::Table::Namespace, scan.graph_namespace, limit)
            }
            Self::Files => files::page(ws, scan.files.as_deref(), limit),
        }
    }
}

impl Section {
    /// How many items this section yields, counted with one aggregate
    /// query and without reading any chunk body from disk. See
    /// [`crate::import::LegacyCounts`] for where it may overcount.
    pub(crate) fn count(self, ws: &LegacyWorkspace) -> Result<u64> {
        match self {
            Self::Documents => memory_docs::count(ws, false),
            Self::Chunks => chunks::count(ws),
            Self::Conversations => episodic::count(ws),
            Self::Learnings => memory_docs::count(ws, true),
            Self::Profile => profile::count(ws),
            Self::Events => events::count(ws),
            Self::Lessons => episodic::count_lessons(ws),
            Self::GraphGlobal => graph::count(ws, graph::Table::Global),
            Self::GraphNamespace => graph::count(ws, graph::Table::Namespace),
            Self::Files => files::count(ws),
        }
    }
}

/// A SQL condition true when `column` holds text that is not blank by
/// [`str::trim`], the test the sections apply to the rows they read.
pub(crate) fn has_text(column: &str) -> String {
    format!("{}({column})", crate::import::workspace::HAS_TEXT_FN)
}

/// A SQLite count as `u64`.
pub(crate) fn count_of(count: i64) -> u64 {
    u64::try_from(count).unwrap_or(0)
}

/// Metadata every imported item starts from: `source.kind = Import`, the
/// section-scoped legacy id, and the workspace path. An item from a
/// per-profile store has its legacy id prefixed with the store directory
/// (`memory-1/`); [`tag_store`] tags it once the section is done with it.
pub(crate) fn import_meta(ws: &LegacyWorkspace, legacy_id: String) -> MemoryMeta {
    let legacy_id = if ws.suffix.is_empty() {
        legacy_id
    } else {
        format!("memory{}/{legacy_id}", ws.suffix)
    };
    MemoryMeta {
        workspace: Some(ws.workspace_id.clone()),
        ..MemoryMeta::from_source(SourceKind::Import, Some(legacy_id))
    }
}

/// Tags an item from a per-profile store `store:memory<suffix>`; an item
/// from the main store is left as it is.
pub(crate) fn tag_store(ws: &LegacyWorkspace, item: &mut StoreItem) {
    if !ws.suffix.is_empty() {
        push_unique(
            &mut item.meta_mut().tags,
            format!("store:memory{}", ws.suffix),
        );
    }
}

/// `limit` as a SQLite integer.
pub(crate) fn sql_limit(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(i64::MAX)
}

/// Appends `tag` unless it is already present.
pub(crate) fn push_unique(tags: &mut Vec<String>, tag: String) {
    if !tags.contains(&tag) {
        tags.push(tag);
    }
}
