//! What counts as a connector sync, for `LegacyWorkspace::skip_connector_syncs`.
//!
//! v1 synced outside services (Gmail, Slack, Notion, Linear, GitHub, ClickUp,
//! ... through Composio and the connector path) into its stores. That data is
//! re-synced by the connectors themselves, so an opt-in import leaves it out.
//! Every predicate here is used by both the scan and `counts()`, so the counts
//! stay exactly what `items()` yields.
//!
//! - `memory_docs`: logical namespace `skill-*` (Composio `SkillDoc` sync,
//!   `skill-{toolkit}`) or `source:*` (connector path,
//!   `source:{toolkit}:{conn}`). Taint is deliberately not consulted: v1 also
//!   marked the agent's own `global` and flow notes `external_sync`.
//! - chunks: see [`chunk_source_by_identity`] and [`OWNER_SYNC_PATTERN`].
//! - `user_profile`: `facet_id` starting `skill-` (Composio identity facets
//!   `skill-{toolkit}-{conn}-{kind}`).
//! - `graph_namespace`: namespace starting `skill-`, `source:` or `source_`.

/// Connector toolkits whose chunk `source_id` is `{toolkit}:{conn}[:{item}]`
/// (Composio before July: slack chat `slack:{conn}`, docs
/// `{toolkit}:{conn}:{id}`; current: `{toolkit}:{conn}:{item}`).
pub(crate) const CONNECTOR_TOOLKIT_PREFIXES: [&str; 6] = [
    "gmail:", "slack:", "notion:", "linear:", "github:", "clickup:",
];

/// The `source_kind` v1 gave Gmail threads.
pub(crate) const EMAIL_SOURCE_KIND: &str = "email";

/// SQL `LIKE` pattern for a chunk `owner` written by a connector
/// (`{toolkit}-sync:{conn}`).
pub(crate) const OWNER_SYNC_PATTERN: &str = "%-sync:%";

/// Whether a `memory_docs` logical namespace is a connector sync.
pub(crate) fn is_connector_namespace(logical: &str) -> bool {
    logical.starts_with("skill-") || logical.starts_with("source:")
}

/// Whether a chunk source is a connector sync judging by its kind and id
/// alone (the `owner` column is checked by the chunk reader).
pub(crate) fn chunk_source_by_identity(source_kind: &str, source_id: &str) -> bool {
    source_kind == EMAIL_SOURCE_KIND
        || CONNECTOR_TOOLKIT_PREFIXES
            .iter()
            .any(|prefix| source_id.starts_with(prefix))
}

/// SQL condition true for a `graph_namespace` row that is NOT a connector
/// sync (`skill-*`, `source:*`, or the sanitised `source_*`).
pub(crate) const GRAPH_NAMESPACE_KEPT: &str = "NOT (substr(COALESCE(namespace, ''), 1, 6) = 'skill-' \
     OR substr(COALESCE(namespace, ''), 1, 7) = 'source:' \
     OR substr(COALESCE(namespace, ''), 1, 7) = 'source_')";

/// SQL condition true for a `user_profile` row that is NOT a Composio
/// identity facet (`facet_id` starting `skill-`).
pub(crate) const PROFILE_KEPT: &str = "substr(facet_id, 1, 6) != 'skill-'";

#[cfg(test)]
#[path = "connector_tests.rs"]
mod tests;
