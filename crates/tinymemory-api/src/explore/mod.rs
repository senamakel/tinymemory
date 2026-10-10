//! Exploring what memory holds: [`Facet`]s, [`ExploreRequest`] and
//! [`ExplorePage`], and [`GetRequest`] for reading items whole.
//!
//! An explorer (a UI tree, a CLI, an audit script) walks stored items by
//! *facet*: a metadata dimension such as the item kind, the source, the
//! workspace, the folder or the thread. [`crate::MemoryEngine::explore`]
//! groups the items a [`MetaFilter`] admits by one facet and counts each
//! value; [`Facet::narrow`] turns a chosen value back into a filter field, so
//! drilling down is `explore` → pick a bucket → `narrow` → `explore` (or
//! `list`) again, identically for every engine and every client.
//!
//! The facets are fixed by the contract rather than by an engine's storage
//! layout, so an explorer written once works on any engine. An engine that
//! can aggregate server-side overrides `explore`; every other engine gets
//! [`explore_by_listing`], which pages through `list` up to
//! [`ExploreRequest::scan_limit`] items and reports whether it stopped early.
//! Likewise [`get_by_listing`] is the default `get`, and
//! [`forget_within_by_get`] the default reach-confined forget by id.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::engine::MemoryEngine;
use crate::error::{Error, Result};
use crate::item::{ItemId, ItemKind};
use crate::meta::{MemoryMeta, MetaFilter, SourceKind};
use crate::namespace::{Namespace, Reach};
use crate::query::{ForgetReport, ForgetTarget, Hit, ListRequest};

/// Most buckets one [`ExplorePage`] may return.
pub const MAX_BUCKETS: usize = 500;

/// Default for [`ExploreRequest::scan_limit`].
const DEFAULT_SCAN_LIMIT: usize = 5_000;

/// Most items a listing-based explore reads.
pub const MAX_SCAN_LIMIT: usize = 50_000;

/// Most ids one [`GetRequest`] may name.
pub const MAX_GET_IDS: usize = 200;

/// Page size [`explore_by_listing`] and [`get_by_listing`] read with.
const SCAN_PAGE: usize = 200;

/// A metadata dimension items are grouped by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Facet {
    /// The item kind (`document`, `conversation`, `learning`).
    Kind,
    /// The kind of source the item came from (`folder`, `agent`, ...).
    Source,
    /// The configured source's own id (`meta.source.id`).
    SourceId,
    /// `meta.workspace`.
    Workspace,
    /// `meta.folder`.
    Folder,
    /// `meta.file_path`.
    FilePath,
    /// `meta.language`.
    Language,
    /// `meta.repo`.
    Repo,
    /// `meta.url`.
    Url,
    /// `meta.thread_id`.
    Thread,
    /// `meta.agent_id`.
    Agent,
    /// The producing tool call's name.
    ToolCall,
    /// One of `meta.tags`; an item with several tags counts in each.
    Tag,
    /// `meta.namespace`: the memory node, `root` for the root. Narrowing
    /// reads exactly that node.
    Namespace,
}

impl Facet {
    /// The stable snake_case wire string.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Kind => "kind",
            Self::Source => "source",
            Self::SourceId => "source_id",
            Self::Workspace => "workspace",
            Self::Folder => "folder",
            Self::FilePath => "file_path",
            Self::Language => "language",
            Self::Repo => "repo",
            Self::Url => "url",
            Self::Thread => "thread",
            Self::Agent => "agent",
            Self::ToolCall => "tool_call",
            Self::Tag => "tag",
            Self::Namespace => "namespace",
        }
    }

    /// The values an item of `kind` carrying `meta` has for this facet:
    /// none when the field is unset, several only for [`Facet::Tag`].
    #[must_use]
    pub fn values(self, kind: ItemKind, meta: &MemoryMeta) -> Vec<String> {
        let one = |value: Option<&String>| value.into_iter().cloned().collect();
        match self {
            Self::Kind => vec![kind.as_str().to_string()],
            Self::Source => vec![meta.source.kind.as_str().to_string()],
            Self::SourceId => one(meta.source.id.as_ref()),
            Self::Workspace => one(meta.workspace.as_ref()),
            Self::Folder => one(meta.folder.as_ref()),
            Self::FilePath => one(meta.file_path.as_ref()),
            Self::Language => one(meta.language.as_ref()),
            Self::Repo => one(meta.repo.as_ref()),
            Self::Url => one(meta.url.as_ref()),
            Self::Thread => one(meta.thread_id.as_ref()),
            Self::Agent => one(meta.agent_id.as_ref()),
            Self::ToolCall => one(meta.tool_call.as_ref().map(|call| &call.name)),
            Self::Tag => meta.tags.clone(),
            Self::Namespace => vec![meta.namespace.to_string()],
        }
    }

    /// Narrows `filter` to items whose value for this facet is `value`.
    ///
    /// [`Facet::Folder`] and [`Facet::FilePath`] narrow by path prefix, as
    /// their filter fields do, so a folder also admits its subfolders.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidRequest`] for a blank value, or for a [`Facet::Kind`]
    /// or [`Facet::Source`] value that names no kind, or a
    /// [`Facet::Namespace`] value that is not a namespace.
    pub fn narrow(self, filter: &mut MetaFilter, value: &str) -> Result<()> {
        if value.trim().is_empty() {
            return Err(Error::InvalidRequest(format!(
                "a `{}` value must not be blank",
                self.as_str()
            )));
        }
        let owned = Some(value.to_string());
        match self {
            Self::Kind => filter.kinds = vec![parse_kind(value)?],
            Self::Source => filter.sources = vec![parse_source(value)?],
            Self::SourceId => filter.source_id = owned,
            Self::Workspace => filter.workspace = owned,
            Self::Folder => filter.folder = owned,
            Self::FilePath => filter.file_path = owned,
            Self::Language => filter.language = owned,
            Self::Repo => filter.repo = owned,
            Self::Url => filter.url = owned,
            Self::Thread => filter.thread_id = owned,
            Self::Agent => filter.agent_id = owned,
            Self::ToolCall => filter.tool_call = owned,
            Self::Tag => filter.tags_any = vec![value.to_string()],
            Self::Namespace => filter.reach = Some(Reach::exact(value.parse::<Namespace>()?)),
        }
        Ok(())
    }
}

fn parse_kind(value: &str) -> Result<ItemKind> {
    ItemKind::ALL
        .into_iter()
        .find(|kind| kind.as_str() == value)
        .ok_or_else(|| Error::InvalidRequest(format!("`{value}` is not an item kind")))
}

fn parse_source(value: &str) -> Result<SourceKind> {
    SourceKind::ALL
        .into_iter()
        .find(|kind| kind.as_str() == value)
        .ok_or_else(|| Error::InvalidRequest(format!("`{value}` is not a source kind")))
}

/// Group the items `filter` admits by `facet`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExploreRequest {
    /// The dimension to group by.
    pub facet: Facet,
    /// Which items to group.
    #[serde(default)]
    pub filter: MetaFilter,
    /// Most buckets to return, largest first; `1..=`[`MAX_BUCKETS`].
    pub limit: usize,
    /// Most items a listing-based engine reads before it stops and reports
    /// [`ExplorePage::truncated`]; `1..=`[`MAX_SCAN_LIMIT`], 5,000 when
    /// omitted. An engine that aggregates server-side may ignore it.
    #[serde(default = "default_scan_limit")]
    pub scan_limit: usize,
}

fn default_scan_limit() -> usize {
    DEFAULT_SCAN_LIMIT
}

impl ExploreRequest {
    /// Groups everything by `facet`, returning at most `limit` buckets.
    #[must_use]
    pub fn new(facet: Facet, limit: usize) -> Self {
        Self {
            facet,
            filter: MetaFilter::default(),
            limit,
            scan_limit: DEFAULT_SCAN_LIMIT,
        }
    }

    /// Checks the limits.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidRequest`] for a limit out of range.
    pub fn validate(&self) -> Result<()> {
        if !(1..=MAX_BUCKETS).contains(&self.limit) {
            return Err(Error::InvalidRequest(format!(
                "explore limit must be between 1 and {MAX_BUCKETS}"
            )));
        }
        if !(1..=MAX_SCAN_LIMIT).contains(&self.scan_limit) {
            return Err(Error::InvalidRequest(format!(
                "explore scan_limit must be between 1 and {MAX_SCAN_LIMIT}"
            )));
        }
        Ok(())
    }
}

/// One value of a facet and how many items carry it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FacetBucket {
    /// The value, as [`Facet::narrow`] takes it.
    pub value: String,
    /// Items carrying it.
    pub count: u64,
}

/// What [`crate::MemoryEngine::explore`] found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplorePage {
    /// The facet grouped by.
    pub facet: Facet,
    /// The values, most items first (ties by value), at most the request's
    /// `limit`.
    pub buckets: Vec<FacetBucket>,
    /// Items the filter admitted (that were read, when `truncated`).
    pub total: u64,
    /// Of those, items with no value for the facet.
    pub missing: u64,
    /// Values beyond `limit` that were left out.
    pub more_buckets: u64,
    /// Whether the scan stopped at `scan_limit` before the end, so the counts
    /// are a lower bound.
    pub truncated: bool,
}

/// Read stored items whole, by id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetRequest {
    /// The ids; `1..=`[`MAX_GET_IDS`].
    pub ids: Vec<ItemId>,
    /// Only items in this reach are returned; `None` reads every namespace
    /// except a service sandbox ([`Reach::admitted_by`]).
    /// An id outside the reach is left out as if it named nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reach: Option<Reach>,
}

impl GetRequest {
    /// Checks the ids.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidRequest`] for no ids, too many, or a blank one.
    pub fn validate(&self) -> Result<()> {
        if self.ids.is_empty() || self.ids.len() > MAX_GET_IDS {
            return Err(Error::InvalidRequest(format!(
                "get takes between 1 and {MAX_GET_IDS} ids"
            )));
        }
        if self.ids.iter().any(|id| id.as_str().trim().is_empty()) {
            return Err(Error::InvalidRequest("an id must not be blank".to_string()));
        }
        Ok(())
    }
}

/// [`crate::MemoryEngine::explore`] by paging through `list`: the default
/// every engine gets.
///
/// # Errors
///
/// An invalid request, and the engine's own `list` failures.
pub async fn explore_by_listing<E: MemoryEngine + ?Sized>(
    engine: &E,
    req: ExploreRequest,
) -> Result<ExplorePage> {
    req.validate()?;
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut total = 0_u64;
    let mut missing = 0_u64;
    let mut read = 0_usize;
    let mut cursor = None;
    let truncated = loop {
        let page = engine
            .list(ListRequest {
                filter: req.filter.clone(),
                limit: SCAN_PAGE.min(req.scan_limit - read),
                cursor: cursor.take(),
            })
            .await?;
        for hit in &page.items {
            total += 1;
            let values = req.facet.values(hit.kind, &hit.meta);
            if values.is_empty() {
                missing += 1;
            }
            for value in values {
                *counts.entry(value).or_default() += 1;
            }
        }
        read += page.items.len();
        match page.next_cursor {
            None => break false,
            Some(_) if read >= req.scan_limit => break true,
            Some(next) => cursor = Some(next),
        }
    };
    Ok(explore_page_of(
        req.facet, counts, req.limit, total, missing, truncated,
    ))
}

/// Builds an [`ExplorePage`] from per-value counts: largest first, ties by
/// value, cut to `limit`. Shared by [`explore_by_listing`] and engines that
/// override [`crate::MemoryEngine::explore`] with their own tally, so every
/// engine orders and cuts buckets the same way.
#[must_use]
pub fn explore_page_of(
    facet: Facet,
    counts: BTreeMap<String, u64>,
    limit: usize,
    total: u64,
    missing: u64,
    truncated: bool,
) -> ExplorePage {
    let mut buckets: Vec<FacetBucket> = counts
        .into_iter()
        .map(|(value, count)| FacetBucket { value, count })
        .collect();
    buckets.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.value.cmp(&b.value)));
    let more_buckets = buckets.len().saturating_sub(limit) as u64;
    buckets.truncate(limit);
    ExplorePage {
        facet,
        buckets,
        total,
        missing,
        more_buckets,
        truncated,
    }
}

/// [`crate::MemoryEngine::get`] by paging through `list` until every id is
/// found: the default every engine gets. Returns hits in the order the ids
/// were named; an id that names nothing is left out.
///
/// # Errors
///
/// An invalid request, and the engine's own `list` failures.
pub async fn get_by_listing<E: MemoryEngine + ?Sized>(
    engine: &E,
    req: GetRequest,
) -> Result<Vec<Hit>> {
    req.validate()?;
    let mut found: BTreeMap<ItemId, Hit> = BTreeMap::new();
    let mut cursor = None;
    loop {
        let page = engine
            .list(ListRequest {
                filter: MetaFilter {
                    reach: req.reach.clone(),
                    ..MetaFilter::default()
                },
                limit: SCAN_PAGE,
                cursor: cursor.take(),
            })
            .await?;
        for hit in page.items {
            if req.ids.contains(&hit.id) {
                found.insert(hit.id.clone(), hit);
            }
        }
        if found.len() == req.ids.len() {
            break;
        }
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    Ok(in_request_order(&req.ids, found))
}

/// [`crate::MemoryEngine::forget_within`] by reading the ids back with
/// [`crate::MemoryEngine::get`] and `reach` (in batches of [`MAX_GET_IDS`]),
/// then forgetting only what came back: the default every engine gets. An id
/// outside the reach, or naming nothing, is not counted. Correct for any
/// engine; confined to the reach as long as the engine's own `forget` by id
/// reads only where the ids it is given live.
///
/// # Errors
///
/// [`Error::InvalidRequest`] for no ids or a blank one, and the engine's own
/// `get` and `forget` failures.
pub async fn forget_within_by_get<E: MemoryEngine + ?Sized>(
    engine: &E,
    ids: Vec<ItemId>,
    reach: Reach,
) -> Result<ForgetReport> {
    let ids = forget_within_ids(ids)?;
    let mut found = Vec::new();
    for batch in ids.chunks(MAX_GET_IDS) {
        let hits = engine
            .get(GetRequest {
                ids: batch.to_vec(),
                reach: Some(reach.clone()),
            })
            .await?;
        found.extend(hits.into_iter().map(|hit| hit.id));
    }
    if found.is_empty() {
        return Ok(ForgetReport::default());
    }
    engine.forget(ForgetTarget::Ids(found)).await
}

/// The ids of a [`crate::MemoryEngine::forget_within`] call, each once in
/// the order first named, checked as [`ForgetTarget::validate`] and
/// [`GetRequest::validate`] check theirs.
///
/// # Errors
///
/// [`Error::InvalidRequest`] for no ids or a blank one.
pub fn forget_within_ids(ids: Vec<ItemId>) -> Result<Vec<ItemId>> {
    ForgetTarget::Ids(ids.clone()).validate()?;
    if ids.iter().any(|id| id.as_str().trim().is_empty()) {
        return Err(Error::InvalidRequest("an id must not be blank".to_string()));
    }
    let mut seen = std::collections::BTreeSet::new();
    Ok(ids.into_iter().filter(|id| seen.insert(id.clone())).collect())
}

/// The hits of `found` in the order of `ids`, each once.
#[must_use]
pub fn in_request_order(ids: &[ItemId], mut found: BTreeMap<ItemId, Hit>) -> Vec<Hit> {
    ids.iter().filter_map(|id| found.remove(id)).collect()
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
