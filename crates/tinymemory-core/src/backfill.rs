//! Re-file already-stored connector documents into the memory tree (#6012).
//!
//! openhuman#6007 fixed the *routing*: connector items now reach
//! `mem_tree_chunks` as they are synced. It did nothing for the records already
//! on disk, and it cannot — the per-item sync gate treats an ingested document
//! as done, so re-syncing fetches nothing and creates no tree rows. On the
//! profile that reported the bug that is ~3000 documents, fully embedded in the
//! namespace store and invisible to every tree-backed surface.
//!
//! This walks the connector namespaces and feeds each stored document through
//! the same funnel the sync path uses, so a backfilled row and a freshly-synced
//! one are the same row. It deliberately reads
//! [`crate::engine::ingest_connector_item_into_tree`] rather than re-deriving
//! the `{toolkit}:{connection_id}` identity: two call sites owning one rule is
//! exactly what produced #6007.
//!
//! # Idempotent by construction, resumable by asking first
//!
//! The ingest pipeline answers `already_ingested` when its transaction persists
//! nothing, so running this twice writes nothing the second time. There is no
//! watermark to keep and no way for an interrupted run to corrupt anything —
//! the worst case is repeated work.
//!
//! `limit` bounds *cost*: it counts the documents a pass reads and files, not
//! the documents it looks at. A document the tree already holds is recognised
//! by asking the ingest gate first — one keyed lookup, by the funnel's own
//! identity — and is never charged. That is what makes "call again" a real
//! resume story. The first version charged the limit before the gate could
//! answer, and because `list_documents` yields newest first — exactly the
//! documents the sync path had already filed — a large account re-examined the
//! same `limit` filed documents on every pass and never reached the rest
//! (openhuman#6051).
//!
//! # Why it costs what it costs
//!
//! `list_documents` carries no `content` column, so each document needs its own
//! read, and each ingest embeds its chunks. A full pass over a large mailbox is
//! thousands of reads and thousands of embeddings — which is why nothing calls
//! this automatically. It is an operator action with a `dry_run` preview, not
//! something that should fire on upgrade and quietly spend a user's embedding
//! budget (openhuman#5324).

use std::collections::BTreeMap;

use crate::sources::SourceKind;
use crate::store::MemoryClientRef;
use crate::Config;

/// Documents read and filed per pass when the caller names no bound.
///
/// Deliberately modest: a pass is resumable (just call again), and a caller
/// that wants the whole account can say so. The default protects the operator
/// who clicks once without reading the cost note above.
pub const DEFAULT_BACKFILL_LIMIT: u64 = 500;

/// How many distinct skip reasons to carry back before truncating.
///
/// The notes are for a human deciding what to do next, and the same reason
/// repeated a thousand times tells them nothing the first one did not.
const MAX_NOTES: usize = 20;

/// What one backfill pass examined and wrote.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BackfillReport {
    /// Documents charged against the limit: read and filed, or — on a dry run
    /// — the ones a real pass would read and file. A document the tree already
    /// holds is not counted here.
    pub scanned: u64,
    /// Documents that produced new memory-tree rows.
    pub ingested: u64,
    /// Documents the tree already held. Not a failure — this is the counter
    /// that makes a repeated run readable as "nothing left to do". Recognised
    /// before any budget is spent, so they never stand between a pass and the
    /// documents behind them.
    pub already_present: u64,
    /// Documents left alone: no resolvable scope, or a tolerated read/ingest
    /// failure. Never filed under a guess.
    pub skipped: u64,
    /// Whether the pass stopped on its limit with documents still waiting to be
    /// filed.
    pub more_pending: bool,
    /// Bounded, human-readable reasons behind `skipped`.
    pub notes: Vec<String>,
}

impl BackfillReport {
    fn note(&mut self, reason: String) {
        if self.notes.len() < MAX_NOTES && !self.notes.contains(&reason) {
            self.notes.push(reason);
        }
    }
}

/// One namespace to sweep, and the tree scope its documents belong to.
struct Target {
    namespace: String,
    toolkit: String,
    connection_id: String,
}

/// Walk the connector namespaces, feeding stored documents into the memory tree.
///
/// `dry_run` reports what a real pass would read and file — and what the tree
/// already holds — without reading any content or writing anything, which is
/// the only honest way to show an operator the size of the job before they pay
/// for it.
pub async fn backfill_connector_trees(
    config: &Config,
    client: &MemoryClientRef,
    limit: Option<u64>,
    dry_run: bool,
) -> anyhow::Result<BackfillReport> {
    let limit = limit.unwrap_or(DEFAULT_BACKFILL_LIMIT);
    let mut report = BackfillReport::default();
    let targets = resolve_targets(config, &mut report)?;

    // Tolerated (non-corrupt) per-document failures, counted the same way the
    // connector sync counts them. A corrupt store still aborts: it fails every
    // later document identically, so continuing would burn the whole limit
    // producing the same error (openhuman#5820).
    let failures = std::sync::atomic::AtomicU32::new(0);

    'targets: for target in targets {
        let listed = match client.list_documents(Some(&target.namespace)).await {
            Ok(listed) => listed,
            Err(error) => {
                report.note(format!(
                    "{}: could not be listed ({error})",
                    target.namespace
                ));
                continue;
            }
        };
        let documents = listed
            .get("documents")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();

        for document in documents {
            let Some(key) = document.get("key").and_then(serde_json::Value::as_str) else {
                // A document row with no key cannot be read back or addressed
                // in the tree; counting it as skipped keeps `scanned` honest.
                report.skipped = report.skipped.saturating_add(1);
                report.note(format!(
                    "{}: a document row carries no key",
                    target.namespace
                ));
                continue;
            };

            // Ask before spending. A document the tree already holds costs one
            // keyed lookup and none of the limit; only a document that still
            // needs reading and filing is charged, so a pass that stops on its
            // limit resumes past everything already filed when called again.
            // The probe has to come before the limit check, or a pass could not
            // tell "more to file" from "more already filed".
            match crate::engine::connector_item_already_treed(
                config,
                &target.toolkit,
                &target.connection_id,
                key,
            ) {
                Ok(Some(true)) => {
                    report.already_present = report.already_present.saturating_add(1);
                    // A filed document is the only kind this loop handles
                    // without awaiting anything, and on a large account they
                    // come in long runs — every document the sync path has
                    // already treed. Hand the runtime a turn per document so
                    // a pass over tens of thousands of them does not hold its
                    // worker thread for the whole run.
                    tokio::task::yield_now().await;
                    continue;
                }
                Ok(Some(false)) => {}
                // The scope was built from the registry above, so this is close
                // to unreachable — but the funnel would refuse the same item,
                // and it is counted the way that refusal is.
                Ok(None) => {
                    report.skipped = report.skipped.saturating_add(1);
                    continue;
                }
                Err(error) => {
                    let rendered = format!("{error:#}");
                    crate::corruption::escalate_or_count(
                        "connector tree backfill",
                        config,
                        error,
                        &failures,
                    )?;
                    report.skipped = report.skipped.saturating_add(1);
                    report.note(format!(
                        "{}: the tree gate could not be read ({rendered})",
                        target.namespace
                    ));
                    continue;
                }
            }

            if report.scanned >= limit {
                report.more_pending = true;
                break 'targets;
            }
            report.scanned = report.scanned.saturating_add(1);
            if dry_run {
                continue;
            }

            let stored = match client.get_document(&target.namespace, key).await {
                Ok(Some(stored)) => stored,
                // Listed but unreadable: it was deleted between the list and
                // the read, or the row is damaged. Neither is worth failing the
                // pass over.
                Ok(None) => {
                    report.skipped = report.skipped.saturating_add(1);
                    continue;
                }
                Err(error) => {
                    report.skipped = report.skipped.saturating_add(1);
                    report.note(format!(
                        "{}: a document could not be read ({error})",
                        target.namespace
                    ));
                    continue;
                }
            };

            match crate::engine::ingest_connector_item_into_tree(
                config,
                &target.toolkit,
                &target.connection_id,
                key,
                &stored.title,
                &stored.content,
            )
            .await
            {
                Ok(Some(result)) if result.already_ingested => {
                    report.already_present = report.already_present.saturating_add(1);
                }
                Ok(Some(_)) => report.ingested = report.ingested.saturating_add(1),
                // The funnel refused the scope. It was built from the registry
                // above, so this is close to unreachable — but counting it is
                // cheaper than assuming it cannot happen.
                Ok(None) => report.skipped = report.skipped.saturating_add(1),
                Err(error) => {
                    let rendered = format!("{error:#}");
                    crate::corruption::escalate_or_count(
                        "connector tree backfill",
                        config,
                        error,
                        &failures,
                    )?;
                    report.skipped = report.skipped.saturating_add(1);
                    report.note(format!(
                        "{}: an ingest failed ({rendered})",
                        target.namespace
                    ));
                }
            }
        }
    }

    tracing::info!(
        scanned = report.scanned,
        ingested = report.ingested,
        already_present = report.already_present,
        skipped = report.skipped,
        more_pending = report.more_pending,
        dry_run,
        "[tinycortex:backfill] connector tree backfill pass complete"
    );
    Ok(report)
}

/// The namespaces worth sweeping, derived from the source registry.
///
/// Built from the registry rather than from `list_namespaces`, because the
/// registry is what the *writers* used: `accept_source_items` composes
/// `source:{toolkit}:{connection_id}` from the same row, so reconstructing it
/// the same way cannot drift. Parsing a namespace string back into its halves
/// would have to guess where the toolkit ends.
///
/// The legacy `skill-{toolkit}` namespaces are the awkward half.
/// `store_skill_sync` took an `_integration_id` it never persisted, so those
/// pre-migration documents record no connection at all — and the tree scope
/// needs one. Where the registry holds exactly one connection for the toolkit
/// there is only one answer and it is used; where it holds several there is no
/// way to tell which account a document came from, and a wrong attribution in a
/// memory system is worse than a missing one, so they are skipped and named.
fn resolve_targets(config: &Config, report: &mut BackfillReport) -> anyhow::Result<Vec<Target>> {
    let sources = crate::sources::registry::list_sources_in(config).map_err(anyhow::Error::msg)?;

    let mut targets = Vec::new();
    let mut by_toolkit: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for source in sources.iter().filter(|s| s.kind == SourceKind::Composio) {
        let (Some(toolkit), Some(connection_id)) =
            (source.toolkit.as_deref(), source.connection_id.as_deref())
        else {
            continue;
        };
        let toolkit = toolkit.trim().to_ascii_lowercase();
        let connection_id = connection_id.trim().to_string();
        if toolkit.is_empty() || connection_id.is_empty() {
            continue;
        }
        targets.push(Target {
            namespace: format!("source:{toolkit}:{connection_id}"),
            toolkit: toolkit.clone(),
            connection_id: connection_id.clone(),
        });
        by_toolkit.entry(toolkit).or_default().push(connection_id);
    }

    for (toolkit, connections) in &by_toolkit {
        match connections.as_slice() {
            [only] => targets.push(Target {
                namespace: format!("skill-{toolkit}"),
                toolkit: toolkit.clone(),
                connection_id: only.clone(),
            }),
            several => report.note(format!(
                "skill-{toolkit}: skipped — {} connections are registered for this toolkit and \
                 the pre-migration documents record none, so the account they belong to cannot \
                 be determined",
                several.len()
            )),
        }
    }

    Ok(targets)
}

#[cfg(test)]
#[path = "backfill_tests.rs"]
mod tests;
