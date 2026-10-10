//! Forget: find the items' events, then remove them by `memory_ids`.
//!
//! - **By id**: every scope the engine holds (the root's and each
//!   namespace node's, service sandboxes included, see `scopes`) is searched
//!   for the ids' labels (one listing per batch of ids), and every event of a
//!   found item is removed.
//!   Ids are not confined to a reach.
//! - **By id within a reach** (`forget_within`): only the scopes the reach
//!   admits are searched, found exactly as a `get` with that reach finds
//!   them (its known nodes, or a listing under its own node when it reads
//!   descendants), so no scope outside the reach is listed or read. The
//!   listing must be complete, as for an id looked up everywhere.
//! - **By filter**: the filter must not be empty. Every scope it reads (its
//!   kinds within its reach) is walked, narrowed by the filter's label when
//!   it has one, and the full filter decides which items match; their events
//!   are then removed.
//!
//! An empty selector is never sent: a scope with nothing to remove sends no
//! request at all (see `log::forget`). `forgotten` counts items, not events.

use std::collections::{HashMap, HashSet};

use tinymemory_api::{ForgetReport, ForgetTarget, ItemId, MetaFilter, Reach, forget_within_ids};

use super::CortexEngine;
use super::items::keeps;
use super::scopes::KindScope;
use crate::cortex::envelope::{decode_event, labels};
use crate::cortex::error::Result;

impl CortexEngine {
    /// See the module docs.
    pub(super) async fn forget_items(&self, target: ForgetTarget) -> Result<ForgetReport> {
        target.validate()?;
        let mut forgotten = HashSet::new();
        match target {
            ForgetTarget::Ids(ids) => {
                let ids: Vec<String> = ids
                    .into_iter()
                    .map(|id| id.0)
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect();
                let scopes = self.every_scope().await?;
                self.forget_ids_in(&scopes, &ids, &mut forgotten).await?;
            }
            ForgetTarget::Filter(filter) => {
                for scope in self.scopes_for(&filter).await? {
                    let matched = self.matching_events(&scope, &filter).await?;
                    let mut events = Vec::new();
                    for (id, ids) in matched {
                        events.extend(ids);
                        forgotten.insert(id);
                    }
                    self.log.forget_events(&scope.path, &events).await?;
                }
            }
        }
        Ok(ForgetReport {
            forgotten: forgotten.len(),
        })
    }

    /// See the module docs.
    pub(super) async fn forget_items_within(
        &self,
        ids: Vec<ItemId>,
        reach: Reach,
    ) -> Result<ForgetReport> {
        let ids: Vec<String> = forget_within_ids(ids)?.into_iter().map(|id| id.0).collect();
        let filter = MetaFilter {
            reach: Some(reach),
            ..MetaFilter::default()
        };
        let scopes = self.all_scopes_for(&filter).await?;
        let mut forgotten = HashSet::new();
        self.forget_ids_in(&scopes, &ids, &mut forgotten).await?;
        Ok(ForgetReport {
            forgotten: forgotten.len(),
        })
    }

    /// Removes every event of the items `ids` name in each of `scopes`,
    /// recording each found item in `forgotten`.
    async fn forget_ids_in(
        &self,
        scopes: &[KindScope],
        ids: &[String],
        forgotten: &mut HashSet<String>,
    ) -> Result<()> {
        for scope in scopes {
            let grouped = self.item_events(scope, ids).await?;
            let mut events = Vec::new();
            for (id, decoded) in grouped {
                events.extend(decoded.into_iter().map(|d| d.event_id));
                forgotten.insert(id);
            }
            self.log.forget_events(&scope.path, &events).await?;
        }
        Ok(())
    }

    /// Every event in `scope` of every item `filter` matches, grouped by
    /// item id.
    async fn matching_events(
        &self,
        scope: &KindScope,
        filter: &MetaFilter,
    ) -> Result<HashMap<String, Vec<String>>> {
        let kind = scope.kind;
        let narrowing = labels::narrowing(filter);
        let mut grouped: HashMap<String, Vec<String>> = HashMap::new();
        for event in self.log.walk(&scope.path, narrowing.as_deref()).await? {
            let Some(decoded) = decode_event(&event) else {
                continue;
            };
            if keeps(filter, kind, &decoded.envelope) {
                grouped
                    .entry(decoded.envelope.id)
                    .or_default()
                    .push(decoded.event_id);
            }
        }
        Ok(grouped)
    }
}
