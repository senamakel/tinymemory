//! Value types that exist only because the *driver contract* needs them.
//!
//! Everything here is inert data: serde-derived, dependency-light, and free of
//! any engine or host type. They are separated from [`crate::types`] because
//! that module carries the historical engine value types (which the engine
//! crate aliases back into `tinycortex::memory::types`), whereas these are new
//! shapes introduced by the provider contract itself.
//!
//! ## Why these types and not the engine's
//!
//! Several families the contract exposes (diff, entities, sources,
//! maintenance) have richer types inside the `tinycortex` engine — for example
//! `memory::diff::types::DiffResult`. Those types are *implementation* shapes:
//! they carry git commit SHAs, ledger paths, and engine-specific enums. A
//! third-party driver cannot produce them and must not be required to.
//!
//! So the contract defines the narrower shape a *caller* actually needs, with
//! wire strings deliberately identical to the engine's where they overlap
//! (`added`/`removed`/`modified`), so the embedded driver's conversion is a
//! field-for-field map rather than a translation.
//!
//! ## What is deliberately absent
//!
//! No type here names a configuration struct. `MemoryConfig` stayed engine-side
//! in the M0 carve-out and stays there: a driver holds its own configuration
//! and the contract passes only domain arguments. If a future method cannot be
//! expressed without configuration, that is a signal the family was designed
//! wrong, not that the contract should widen.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::chunks::{DataSource, SourceRef};
use crate::types::MemoryTaint;

/// A per-turn allowlist of memory sources, passed **into** the driver as a
/// query predicate.
///
/// ## Why this is a parameter and not a post-filter
///
/// The host computes a per-turn source allowlist from product policy. If that
/// allowlist were applied after the driver returned rows, a `limit` would be
/// consumed by rows the caller is not allowed to see — so a scoped query could
/// return fewer results than it should, or none at all, purely as an artefact
/// of filtering order. Worse, an out-of-process driver would have already been
/// handed a query it should never have answered in full.
///
/// The predicate therefore travels with the call. `None` means unrestricted;
/// `Some(scope)` means the driver must apply it *inside* its query.
///
/// ## Matching rule (fail-closed)
///
/// [`SourceScope::allows_source_id`] encodes the embedded engine's SQL
/// semantics verbatim: a source-attributed id is in scope when it either equals
/// an allowed id outright, or begins with `mem_src:{allowed}:`. An **empty**
/// allow list therefore matches nothing — a scope that lists no sources denies
/// all source-attributed content rather than waving it through.
///
/// Content that is not attributed to a memory source at all (no
/// `memory_sources` provenance) is outside this predicate's remit; the driver
/// decides that, exactly as the engine's SQL does today.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceScope {
    /// Allowed memory-source identifiers. Empty denies all source-attributed
    /// content.
    pub allow: Vec<String>,
}

impl SourceScope {
    /// Builds a scope from any iterator of source identifiers.
    pub fn new(allow: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            allow: allow.into_iter().map(Into::into).collect(),
        }
    }

    /// Whether this scope lists no sources — in which case it denies all
    /// source-attributed content. See the type docs for why that is the
    /// fail-closed reading and not "unrestricted".
    pub fn is_empty(&self) -> bool {
        self.allow.is_empty()
    }

    /// Whether `source_id` is in scope, using the engine's equality-or-prefix
    /// rule.
    ///
    /// ```
    /// use tinymemory_bus::provider::types::SourceScope;
    ///
    /// let scope = SourceScope::new(["src-abc"]);
    /// assert!(scope.allows_source_id("src-abc"));
    /// assert!(scope.allows_source_id("mem_src:src-abc:item-1"));
    /// assert!(!scope.allows_source_id("src-xyz"));
    ///
    /// // An empty scope denies everything.
    /// assert!(!SourceScope::default().allows_source_id("src-abc"));
    /// ```
    pub fn allows_source_id(&self, source_id: &str) -> bool {
        self.allow.iter().any(|allowed| {
            source_id == allowed || source_id.starts_with(&format!("mem_src:{allowed}:"))
        })
    }
}

/// One unit of content handed to `MemoryIngest`.
///
/// The driver owns chunking, embedding, and persistence — this type carries
/// only what the driver cannot know: where the content came from, when, who it
/// belongs to, and how far it may be trusted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestItem {
    /// Target namespace; `None` means the driver's default namespace.
    #[serde(default)]
    pub namespace: Option<String>,
    /// Concrete upstream provider the content came from.
    pub source: DataSource,
    /// Stable logical id for the ingestion group (channel id, thread id, doc
    /// id). This is the dedupe key, not a display value.
    pub source_id: String,
    /// Account or user the content belongs to; empty for anonymous/system
    /// sources.
    #[serde(default)]
    pub owner: String,
    /// Opaque pointer back to the raw source record, for citation and
    /// drill-down.
    #[serde(default)]
    pub source_ref: Option<SourceRef>,
    /// The content itself, already decoded to text.
    pub content: String,
    /// MIME type of [`Self::content`] when the caller knows it.
    #[serde(default)]
    pub mime: Option<String>,
    /// Event time used for ordering and tree placement; the driver substitutes
    /// ingest time when absent.
    #[serde(default)]
    pub timestamp: Option<DateTime<Utc>>,
    /// Labels carried through from the source. Ingest does not interpret them.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Who spoke this item, when that is not [`Self::owner`].
    ///
    /// A chat batch from an agent session has owner = the session the memory
    /// belongs to and author = the speaking role (`user`, `assistant`). The
    /// previous mapping collapsed the two — every message attributed to the
    /// owner — which destroys role attribution in the stored transcript.
    /// Absent means "the owner spoke", which is true of the single-speaker
    /// sources this field predates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Display label for the conversation, when it is not [`Self::source_id`].
    ///
    /// `source_id` is the dedupe key and may be a constant ("all agent
    /// sessions share one tree source"); the label is what a human reads in a
    /// summary. Absent means the id is readable enough to double as the label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_label: Option<String>,
    /// Platform string to store verbatim, when [`DataSource::as_str`] is not
    /// it. Migrating a caller that has always written a bespoke platform value
    /// must not silently rewrite what is on disk; absent keeps the enum's
    /// name, which is right for every new caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// Recipients, for a mail item. Rendered as the `To:` line.
    ///
    /// Empty for every non-mail source, which is why this is a plain `Vec`
    /// rather than an `Option` — "no recipients" and "not mail" are the same
    /// statement to every reader of it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub to: Vec<String>,
    /// Carbon copies, rendered as the `Cc:` line. Empty as above.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cc: Vec<String>,
    /// This message's own subject, when it differs from the thread's.
    ///
    /// Absent means the thread subject stands, which is the common case: a
    /// reply carries the thread's subject and only a renamed thread differs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// The `List-Unsubscribe` header, verbatim.
    ///
    /// Not decoration: it is the input an unsubscribe flow reads back out of
    /// stored mail, so a pipeline that drops it makes that flow impossible
    /// rather than merely less pretty. Absent for mail that carries no such
    /// header, and for everything that is not mail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list_unsubscribe: Option<String>,
    /// Provenance taint. The **host** stamps this; a driver must persist what it
    /// is given and must never assign or upgrade it.
    #[serde(default)]
    pub taint: MemoryTaint,
    /// Overrides `source_id` for on-disk path grouping only; `source_id`
    /// remains the dedupe key.
    #[serde(default)]
    pub path_scope: Option<String>,
}

/// What an ingest call actually persisted.
///
/// Counts rather than content, so the caller can report progress and detect a
/// silently-dropping driver without holding the written material in memory.
///
/// ## Why "nothing was written" takes more than one field to explain
///
/// A driver answers `written: 0` for two unrelated reasons: it produced units
/// and dropped them, or it recognised the logical source as one it has already
/// ingested and did nothing at all. [`Self::skipped`] cannot carry both — a
/// `1` there would mean either "one unit was dropped" or "the whole call was a
/// no-op", and no caller can tell which. That ambiguity is not academic: a
/// source gate left claimed after the content behind it was wiped reports
/// exactly the "0 written, 0 jobs" of a legitimate no-op, which is what made
/// that class of bug expensive to see.
///
/// So the two facts are separate fields, and `skipped` counts dropped units
/// only.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestOutcome {
    /// Units the driver newly persisted.
    pub written: u32,
    /// Units the driver produced and did not admit.
    ///
    /// Dropped units only. A call the driver refused outright is
    /// [`Self::already_ingested`], not a skip of one unit.
    pub skipped: u32,
    /// Driver-assigned ids for the written units, when the driver exposes them.
    /// May be empty even when [`Self::written`] is non-zero — an external
    /// backend is not obliged to surface its internal ids.
    #[serde(default)]
    pub ids: Vec<String>,
    /// Whether the call was a no-op because this source had been ingested
    /// before.
    ///
    /// The gate is keyed on the logical source, not on the content, so
    /// re-sending *changed* material under a claimed `source_id` still writes
    /// nothing. A caller that re-ingests deliberately — after a wipe, after a
    /// failed import, after clearing a gate by hand — has to tell that refusal
    /// from an empty result, because only the first is a reason to go and
    /// clear the gate.
    ///
    /// A driver with no such gate leaves this `false`, which is true of it.
    #[serde(default, skip_serializing_if = "is_not_set")]
    pub already_ingested: bool,
    /// Follow-up derivation jobs this call scheduled.
    ///
    /// Lower than [`Self::written`] when an earlier call already queued the
    /// same unit — the enqueue is keyed, so a duplicate is a no-op rather than
    /// a second job — and zero on a driver that derives nothing in the
    /// background. Read next to `written` it answers whether the material just
    /// handed over will actually be picked up: rows can land with nothing
    /// scheduled to derive from them, and `written` alone reports that as
    /// success.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub extract_jobs_enqueued: u32,
}

/// Serde predicates that keep a widened struct's *empty* wire form byte-for-byte
/// what it was before the field existed.
///
/// `#[serde(default)]` alone makes a new field decode on an old payload; these
/// make the new payload decode on an old *peer*, which is the other half of an
/// additive change and the half that is easy to forget. A field whose value is
/// the default it would have been given anyway carries no information, so
/// omitting it costs nothing and keeps the common case identical to what a
/// version-skewed reader already handles.
fn is_not_set(value: &bool) -> bool {
    !*value
}

/// See [`is_not_set`]; the same rule for a count whose empty is zero.
fn is_zero(value: &u32) -> bool {
    *value == 0
}

/// One line of the portability stream.
///
/// Export and import are defined over records rather than bytes so the contract
/// stays free of an async runtime and of any streaming abstraction: the host
/// adapter turns a page of records into NDJSON (and back) at the transport
/// boundary.
///
/// [`Self::kind`] is a driver-defined string rather than an enum. A backend has
/// record kinds this crate has never heard of, and a migration between two
/// backends must round-trip them untouched rather than drop what it cannot
/// classify.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportRecord {
    /// Driver-defined record kind (e.g. `entry`, `document`, `chunk`).
    pub kind: String,
    /// Driver-assigned id, unique within [`Self::kind`].
    pub id: String,
    /// Owning namespace, when the record has one.
    #[serde(default)]
    pub namespace: Option<String>,
    /// Provenance taint of the record's content. Preserved across
    /// export → import; an importing driver must not re-stamp it.
    #[serde(default)]
    pub taint: MemoryTaint,
    /// The record body, in the exporting driver's own shape.
    pub payload: serde_json::Value,
}

/// One page of an export, plus the cursor that continues it.
///
/// Paging (rather than a stream) keeps `MemoryPortability`
/// object-safe and runtime-agnostic while still bounding memory: the caller
/// decides the page size and drives the loop.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ExportPage {
    /// Records in this page. May be empty on the final page.
    pub records: Vec<ExportRecord>,
    /// Opaque cursor to pass to the next call. `None` means the export is
    /// complete — this, not an empty [`Self::records`], is the terminator.
    #[serde(default)]
    pub next_cursor: Option<String>,
}

/// What an import call actually accepted.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportOutcome {
    /// Records written.
    pub imported: u32,
    /// Records recognised as already present and skipped.
    pub skipped: u32,
    /// Records rejected. A non-zero value with an empty [`Self::errors`] is a
    /// driver bug: a rejection the operator cannot diagnose.
    pub failed: u32,
    /// Operator-facing reasons for the failures, bounded by the driver. Must
    /// not contain record content or credentials — this is logged.
    #[serde(default)]
    pub errors: Vec<String>,
}

/// Identity of an entity in the driver's index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityRef {
    /// Canonical, driver-stable entity id.
    pub id: String,
    /// Entity kind as a wire string (`person`, `organization`, `topic`, …).
    /// A string rather than an enum because the taxonomy is the driver's, and a
    /// kind this build does not recognise must still round-trip.
    pub kind: String,
    /// Display name.
    pub name: String,
}

/// An entity together with its recency/frequency signals.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EntityHit {
    /// The entity itself.
    pub entity: EntityRef,
    /// Driver-computed hotness, higher is hotter. Not normalised across
    /// drivers — compare within one driver's results only.
    pub hotness: f64,
    /// Number of times the entity was observed.
    pub mentions: u32,
}

/// One row of the entity **occurrence** index: an entity as it was actually
/// observed, and how many observations are behind the row.
///
/// ## Why this is not [`EntityHit`]
///
/// [`EntityHit`] answers "what is this namespace about, and what is warm right
/// now": it is namespace-scoped, ranked by a driver-computed hotness, and its
/// [`EntityRef::name`] is a canonical display name the driver stands behind.
/// This answers a different question — "what is in the index" — and every one
/// of those three properties differs: it spans the whole store, it ranks by
/// raw observation count, and it carries a [`Self::surface`], which is one of
/// the literal forms the source text used and nothing more.
///
/// Folding the two together would need a field to lie. A surface placed in
/// `name` reads as canonical to every caller that renders it, and a hotness
/// synthesised for an index row would rank on a number no decay curve
/// produced. Two shapes, each true about its own query, is the cheaper answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityOccurrence {
    /// Canonical, driver-stable entity id — the same id space as
    /// [`EntityRef::id`], so an id read here can be passed straight back to an
    /// entity-keyed call.
    pub entity_id: String,
    /// Entity kind as a wire string (`person`, `email`, `topic`, …), the same
    /// open vocabulary as [`EntityRef::kind`]: a kind this build does not
    /// recognise must still round-trip.
    pub kind: String,
    /// One surface form the entity was observed under — a sample, not a name.
    ///
    /// Which sample is the driver's choice, and it may change as rows are
    /// added, so this is for showing a caller how the text read, never for
    /// identity. Empty is legitimate: an index that records occurrences
    /// without keeping the source form has nothing truthful to put here.
    #[serde(default)]
    pub surface: String,
    /// How many indexed observations this row aggregates.
    ///
    /// The unit is the driver's occurrence row, not "times the word appeared":
    /// an index keyed per `(entity, node)` counts a node once no matter how
    /// often the entity is named inside it, while one keyed per span counts
    /// every span. Compare within one driver's results only.
    pub mentions: u32,
}

/// One [`EntityOccurrence`] together with the chunk it was observed in.
///
/// ## Why the chunk id is on the row
///
/// Asking one chunk what it is about needs no such field: the chunk id was the
/// argument, and every row answers for it. Asking fifteen hundred chunks in
/// one call returns a single flat list, and without the id on each row there
/// is no way back from a row to the content it describes.
///
/// The alternative shape — a list per chunk, or a map keyed by chunk id — was
/// rejected twice over. It encodes the grouping in the type, so every chunk
/// the extractor has not reached yet costs an empty vector on the wire; and a
/// map keyed by chunk id serialises as a JSON object whose keys are
/// caller-supplied text, which is the encoding [`super::chunks::ChunkEmbedding`]
/// is a list to avoid.
///
/// The occurrence is **flattened** rather than nested, so the wire form is an
/// [`EntityOccurrence`] object carrying one extra `chunk_id` key. A caller that
/// already decodes occurrences reads these under the same field names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkEntityOccurrence {
    /// The chunk — or the summary node — this observation came from.
    ///
    /// Rows are **not** unique by this: one chunk contributes one row per
    /// distinct `(entity, surface)` it was observed under, for
    /// [`EntityOccurrence::surface`]'s reason. Group by it; never index by
    /// position against the ids that were asked for.
    pub chunk_id: String,
    /// The observation itself.
    #[serde(flatten)]
    pub occurrence: EntityOccurrence,
}

/// Identity of a captured snapshot.
///
/// The engine's own snapshot type additionally carries the git commit SHA and
/// ledger trailers that back it; those are implementation, so the contract
/// exposes only the identity and the counts a caller can act on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotRef {
    /// Driver-stable snapshot id.
    pub id: String,
    /// Logical source this snapshot covers.
    pub source_id: String,
    /// Human-readable source label at capture time.
    #[serde(default)]
    pub label: String,
    /// Number of items materialised into the snapshot.
    pub item_count: u32,
    /// Capture time in milliseconds since the Unix epoch.
    pub taken_at_ms: i64,
}

/// What happened to one item between two snapshots.
///
/// Wire strings are identical to the engine's `memory::diff::types::ChangeKind`
/// so the embedded adapter maps rather than translates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// Present in the later snapshot only.
    Added,
    /// Present in the earlier snapshot only.
    Removed,
    /// Present in both, with differing content.
    Modified,
}

impl ChangeKind {
    /// Stable wire string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Modified => "modified",
        }
    }
}

/// A single item-level change inside a [`DiffReport`].
///
/// Item identity is the item id, never the title, so a rename reports as a
/// removal plus an addition rather than a modification.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceChange {
    /// Stable item id.
    pub item_id: String,
    /// Display title, or the id when the driver has no better label.
    #[serde(default)]
    pub title: String,
    /// What kind of change occurred.
    pub kind: ChangeKind,
    /// Content hash on the earlier side; absent for an addition.
    #[serde(default)]
    pub old_content_hash: Option<String>,
    /// Content hash on the later side; absent for a removal.
    #[serde(default)]
    pub new_content_hash: Option<String>,
}

/// The result of diffing one source between two snapshots.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffReport {
    /// Source this diff covers.
    pub source_id: String,
    /// Baseline snapshot id; `None` for a first-ever diff, where everything is
    /// an addition.
    #[serde(default)]
    pub from_snapshot_id: Option<String>,
    /// Target snapshot id.
    pub to_snapshot_id: String,
    /// Items added.
    pub added: u32,
    /// Items removed.
    pub removed: u32,
    /// Items modified.
    pub modified: u32,
    /// Items present and unchanged.
    pub unchanged: u32,
    /// Per-item changes. May be truncated by the driver; the counts above are
    /// authoritative.
    #[serde(default)]
    pub changes: Vec<SourceChange>,
}

/// One item handed to `MemorySourceSink` by the host's sync
/// machinery.
///
/// The host owns credentials, scheduling, and fetching; the driver owns storage
/// and indexing. This type is the whole of what crosses that line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceItem {
    /// Stable per-source item id. Dedupe key; not a display value.
    pub item_id: String,
    /// Display title.
    #[serde(default)]
    pub title: String,
    /// Item body, already decoded to text.
    pub content: String,
    /// MIME type of [`Self::content`] when known.
    #[serde(default)]
    pub mime: Option<String>,
    /// Canonical URL back to the item, when it has one.
    #[serde(default)]
    pub url: Option<String>,
    /// Upstream last-modified time in milliseconds since the Unix epoch.
    #[serde(default)]
    pub updated_at_ms: Option<i64>,
    /// Labels carried through from the source.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Which stored content a selective forget removes.
///
/// ## Why an enum and not a struct of options
///
/// The four arms are mutually exclusive and each maps 1:1 onto a delete the
/// engine already implements — by chunk id, by exact source, by source-id
/// prefix, by owner. A struct of `Option` fields would admit combinations none
/// of those deletes has a meaning for (`chunk_id` *and* `owner`; a prefix *and*
/// an exact id), and the driver would have to invent a precedence rule and
/// document it, for a **destructive** call where guessing wrong deletes the
/// wrong content. The enum makes the illegal combinations unrepresentable
/// instead of merely discouraged.
///
/// ## Why `source_kind` is a wire string
///
/// The same reason `MemorySourceSink::accept_source_items` takes one: the set
/// of source kinds belongs to the host's sync machinery and grows without a
/// contract change. A driver parses it and answers
/// [`MemoryError::Invalid`](crate::error::MemoryError::Invalid) for a kind it
/// does not recognise — never an outcome of zero. On a read a silent zero is a
/// misleading empty list; here it is an operator told their content was
/// already gone when nothing was even looked at.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case")]
pub enum ForgetSelector {
    /// One chunk, by id.
    ///
    /// The narrowest arm, and the only one that does not name a source: a
    /// caller removing a single row it is looking at has the id and nothing
    /// else. An id the store does not hold removes nothing and is not an
    /// error, matching every other idempotent delete in this contract.
    Chunk {
        /// The chunk to remove.
        chunk_id: String,
    },
    /// Everything stored under one exact `(source_kind, source_id)`.
    ///
    /// Exact, never a prefix, so sibling sources sharing a leading segment are
    /// untouched — that is what [`Self::SourcePrefix`] is for, and conflating
    /// the two is how a single disconnect takes a workspace with it.
    Source {
        /// Kind of the source, as a wire string.
        source_kind: String,
        /// The exact logical source id.
        source_id: String,
    },
    /// Everything whose source id begins with `source_id_prefix`, under one
    /// kind.
    ///
    /// The disconnect path for a provider that files one logical connection
    /// under many derived ids. The prefix is matched literally: it is not a
    /// pattern, so `%` and `_` in a provider id mean themselves.
    SourcePrefix {
        /// Kind of the sources, as a wire string.
        source_kind: String,
        /// Literal prefix the source ids must start with.
        source_id_prefix: String,
    },
    /// Everything owned by one owner, under one kind.
    ///
    /// Owner is the account the content came in through, so this is the arm a
    /// caller reaches for when one connection of several is removed and the
    /// others must survive on the same source.
    Owner {
        /// Kind of the sources, as a wire string.
        source_kind: String,
        /// The owner whose content is removed.
        owner: String,
    },
}

/// What a selective forget removed.
///
/// Two counts because they are two different deletions, not a total and a
/// part. Chunks are what the caller asked to remove; a summary tree is
/// *derived* from chunks and is cleaned only once every chunk under it has
/// gone, so a forget that removes rows may clean no tree and a forget that
/// removes none may still clean one left stranded by an earlier partial
/// delete. Summed into one number, neither case can be told from the other.
///
/// A count rather than the `bool` the single-source path used before it: an
/// exact-source delete can orphan at most one tree, but a prefix or owner
/// selector spans many sources and can orphan several, and a `bool` would have
/// to report three as "yes".
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForgetOutcome {
    /// Chunk rows removed, together with the per-chunk side rows and content
    /// files that hang off them.
    pub chunks_removed: u64,
    /// Summary trees cascaded away because nothing was left under them.
    pub trees_cleaned: u64,
}

/// Outcome of one maintenance operation.
///
/// A single shape covers reembed, compact, consolidate, and doctor because the
/// caller does the same thing with all four: report progress and surface
/// findings. A per-operation result type would multiply the contract surface
/// without giving any caller more to act on.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintenanceReport {
    /// Which operation ran (`reembed`, `compact`, `consolidate`, `doctor`).
    pub operation: String,
    /// Units the driver examined.
    pub examined: u64,
    /// Units the driver changed. Always `0` for `doctor`, which is read-only.
    pub changed: u64,
    /// Operator-facing findings and notes. Must not contain memory content or
    /// credentials — this is logged and shown in status output.
    #[serde(default)]
    pub findings: Vec<String>,
}

/// Aggregate counts over what the driver has stored.
///
/// Separate from [`MaintenanceReport`] because the caller does something
/// different with it: a report is read by an operator, these are read by code.
/// `findings: Vec<String>` cannot answer "how far behind is the pipeline"
/// without parsing prose back into numbers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreStats {
    /// Chunks the driver holds.
    pub chunks: u64,
    /// Of those chunks, how many the driver has extracted structure from.
    ///
    /// A count rather than the ratio a caller displays, because the ratio is
    /// only meaningful against the denominator it was measured with. Read
    /// separately, the two can be sampled either side of a write and produce a
    /// coverage above 1.0; read together they cannot.
    ///
    /// A driver that does not extract structure leaves this at zero, which
    /// reads as "nothing extracted" — correct for it, and the reason a caller
    /// should show the pair rather than the ratio alone.
    pub chunks_with_structure: u64,
    /// Timestamp of the most recently stored chunk, if any.
    ///
    /// `None` for an empty store — distinct from `Some(0)`, which would be a
    /// chunk stamped at the epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub most_recent_chunk_ms: Option<i64>,
}

/// The ingest and re-embed queue's state, as counts rather than rows.
///
/// Every field answers a question an operator or a health probe asks about
/// throughput. A driver with no queue answers all-zero rather than refusing:
/// "nothing is backed up" is true of a driver that cannot back up.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueStats {
    /// Jobs waiting, whatever their scheduled time.
    pub ready: u64,
    /// Jobs a worker currently holds.
    pub running: u64,
    /// Jobs that finished successfully.
    pub done: u64,
    /// Jobs that ended in a terminal failure.
    pub failed: u64,
    /// Of those failures, how many the driver will not retry on its own.
    ///
    /// The distinction is what separates an alert from a shrug: transient
    /// failures self-heal on the next attempt, and a caller that escalates on
    /// [`Self::failed`] alone pages someone for a queue that is already
    /// recovering. Counted with `failed` rather than beside it, because two
    /// reads can land either side of a retry and report more unrecoverable
    /// failures than there are failures.
    pub failed_unrecoverable: u64,
    /// Ready jobs whose scheduled time has already passed.
    ///
    /// The difference between this and [`Self::ready`] is deferred work, and
    /// conflating them reads a healthy backlog of future jobs as a stall.
    pub eligible_now: u64,
    /// When the queue last settled a job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_completed_ms: Option<i64>,
    /// The scheduled time of the oldest job eligible to run now.
    ///
    /// With [`Self::last_completed_ms`] this is what an idle-time calculation
    /// needs: how long something runnable has been waiting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_eligible_ms: Option<i64>,
}

/// The most recent terminal queue failure.
///
/// Carries the driver's own words rather than a class this contract invents:
/// the caller shows it to an operator, and a re-classification here would lose
/// what the engine actually said.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueFailure {
    /// The failure's own message. Must carry no memory content.
    pub reason: String,
    /// The driver's classification, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    /// When the failing job settled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<i64>,
    /// When the queue last completed a job *successfully*, read together with
    /// the failure above rather than in a second call.
    ///
    /// A caller deciding whether to show this failure asks whether anything
    /// has succeeded since it — a success after the failure means the queue
    /// recovered and the failure is stale. Answering that from two separate
    /// calls lets a job settle in between and flip the decision, so the two
    /// values are read as one observation.
    ///
    /// This is not [`QueueStats::last_completed_ms`]: that one counts a
    /// failure as progress, because a fast-failing queue is not a stalled
    /// one. Supersession needs the opposite reading — only a success clears a
    /// failure — so it takes the newest *successful* completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_success_ms: Option<i64>,
}

/// What a flush of pending buffered work did, and what was pending.
///
/// Both numbers, because either alone misleads. `enqueued: false` with
/// `stale_buffers: 0` means there was nothing to do; `enqueued: false` with
/// `stale_buffers: 3` means the driver deduplicated against work it had
/// already scheduled — the same answer for opposite reasons, and a caller
/// showing "nothing to flush" in the second case is wrong.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlushOutcome {
    /// Whether this call scheduled work. `false` when an equivalent flush is
    /// already scheduled — a deduplication, not a failure.
    pub enqueued: bool,
    /// Buffers old enough to be flushed, at the moment the driver looked.
    pub stale_buffers: u64,
}

/// What one connector-tree backfill pass examined and wrote (#6012).
///
/// Four counters rather than one, because "did nothing" has three very
/// different causes a caller has to be able to tell apart: the tree already
/// held everything (`already_present`), nothing could be addressed
/// (`skipped`), or there was nothing to file at all (`scanned: 0` with the
/// other two at zero). Collapsing them would make an account whose scope
/// could not be resolved read exactly like one that is fully backfilled.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackfillTreesOutcome {
    /// Documents charged against the limit this pass: read and filed, or on a
    /// dry run the ones a real pass would read and file. A document the tree
    /// already holds is reported under `already_present` instead.
    pub scanned: u64,
    /// Documents that produced new memory-tree rows.
    pub ingested: u64,
    /// Documents the tree already held. Not a failure: this is what makes a
    /// repeated pass readable as "nothing left to do". Recognised before any
    /// budget is spent, so they never stand between a pass and the documents
    /// behind them (openhuman#6051).
    pub already_present: u64,
    /// Documents left alone — no resolvable scope, or a tolerated failure.
    /// Never filed under a guess.
    pub skipped: u64,
    /// Whether the pass stopped on its limit with documents still waiting to
    /// be filed. The caller resumes by calling again; there is no cursor to
    /// carry, because a document already filed costs the next pass nothing.
    pub more_pending: bool,
    /// Bounded, human-readable reasons behind `skipped`.
    pub notes: Vec<String>,
}

/// How much of the backfill to attempt, and whether to write at all.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackfillTreesRequest {
    /// Documents to read and file at most. `None` leaves the bound to the
    /// driver.
    ///
    /// A bound rather than a cursor because the work is idempotent: the pass
    /// asks the ingest gate before it spends, a document the tree already holds
    /// costs nothing, and so resuming is just calling again (openhuman#6051).
    pub limit: Option<u64>,
    /// Report what a real pass would examine, and write nothing.
    ///
    /// The honest way to show an operator the size of the job before they pay
    /// for it — a full pass is one read and one embedding per document.
    pub dry_run: bool,
}

/// What resetting the derived index deleted, requeued and scheduled.
///
/// Three numbers rather than a `MaintenanceReport`'s two, because they are not
/// a ratio: rows deleted, chunks put back in scope, and jobs scheduled to
/// re-derive from them are three independent counts, and collapsing any pair
/// loses the ability to tell "nothing to re-derive" from "re-derivation was
/// not scheduled".
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetOutcome {
    /// Rows removed from the derived tables.
    pub rows_deleted: u64,
    /// Source chunks returned to the pool the index is derived from.
    pub chunks_requeued: u64,
    /// Re-derivation jobs scheduled. Lower than `chunks_requeued` when some
    /// were already queued — the enqueue is keyed, so a duplicate is a no-op
    /// rather than a second job.
    pub jobs_enqueued: u64,
}

/// What wiping the whole store deleted.
///
/// One field, and a struct rather than a bare `u64`, for two reasons that both
/// only show up later. The unit is named where an integer return would leave
/// it to a call site to remember — these are *rows*, across every table the
/// driver owns, not chunks. And a wipe is the one operation whose reporting
/// will want to grow: a driver that later counts the vault files it discarded,
/// or the tables it truncated, adds a field, where a bare integer would have
/// to be replaced and every caller changed with it.
///
/// Deliberately **not** a [`ResetOutcome`]. That one deletes derived rows and
/// schedules their re-derivation, so its three counts describe work that
/// continues; nothing continues after this.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PurgeOutcome {
    /// Database rows the driver deleted, summed across its own tables.
    ///
    /// Rows only, and deliberately not a second count of files: a file count
    /// is not comparable between drivers, and a driver that keeps bodies
    /// in-row would report zero and read as having done less than one that
    /// does not. What a wipe reaches beyond the database is the driver's own
    /// question — `MemoryMaintenance::purge_all` says where that line falls.
    pub rows_deleted: u64,
}

#[cfg(test)]
#[path = "types_tests.rs"]
mod tests;
