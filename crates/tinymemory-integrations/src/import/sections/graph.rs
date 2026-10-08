//! `graph_global` and `graph_namespace`: one learning per relation.
//!
//! The v1 engine kept the entity relations it extracted while ingesting as
//! subject–predicate–object triples, workspace-wide (`graph_global`) or per
//! namespace (`graph_namespace`). Each relation whose subject and object have
//! text becomes a [`LearningKind::Fact`] learning, `"<subject> <predicate>
//! <object>"`, tagged `graph` (and `ns:<namespace>` for a namespaced one),
//! observed at its `updated_at`. Rows are walked by `rowid`: the store is no
//! longer written, so its rowids are stable. A table that is missing, or
//! lacks a column, is skipped.

use rusqlite::params;
use tinymemory_api::{LearningKind, StoreItem};

use super::{Mark, Scanned, count_of, has_text, import_meta, sql_limit};
use crate::import::convert;
use crate::import::error::Result;
use crate::import::workspace::LegacyWorkspace;

/// Which of the two graph tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Table {
    /// `graph_global`.
    Global,
    /// `graph_namespace`.
    Namespace,
}

impl Table {
    fn name(self) -> &'static str {
        match self {
            Self::Global => "graph_global",
            Self::Namespace => "graph_namespace",
        }
    }

    fn present(self, ws: &LegacyWorkspace) -> bool {
        match self {
            Self::Global => ws.schema.graph_global,
            Self::Namespace => ws.schema.graph_namespace,
        }
    }

    fn mark(self, rowid: i64) -> Mark {
        match self {
            Self::Global => Mark::GraphGlobal(rowid),
            Self::Namespace => Mark::GraphNamespace(rowid),
        }
    }
}

/// The `AND …` filter dropping connector-synced namespaces, shared by `page`
/// and `count`. `graph_global` has no namespace and is never filtered.
fn connector_filter(ws: &LegacyWorkspace, table: Table) -> String {
    if ws.skip_connector_syncs && table == Table::Namespace {
        format!(" AND {}", super::connector::GRAPH_NAMESPACE_KEPT)
    } else {
        String::new()
    }
}

/// The next page of `table`'s relations after the row `after`.
pub(super) fn page(
    ws: &LegacyWorkspace,
    table: Table,
    after: Option<i64>,
    limit: usize,
) -> Result<Vec<Scanned>> {
    let Some(memory) = ws.memory.as_ref().filter(|_| table.present(ws)) else {
        return Ok(Vec::new());
    };
    let namespace = match table {
        Table::Global => "NULL",
        Table::Namespace => "namespace",
    };
    let sql = format!(
        "SELECT rowid, subject, predicate, object, updated_at, {namespace} FROM {} \
         WHERE (?1 IS NULL OR rowid > ?1) AND {} AND {}{} ORDER BY rowid LIMIT ?2",
        table.name(),
        has_text("subject"),
        has_text("object"),
        connector_filter(ws, table)
    );
    let mut stmt = memory.prepare(&sql)?;
    let rows = stmt.query_map(params![after, sql_limit(limit)], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            row.get::<_, Option<String>>(2)?.unwrap_or_default(),
            row.get::<_, Option<String>>(3)?.unwrap_or_default(),
            row.get::<_, Option<f64>>(4)?,
            row.get::<_, Option<String>>(5)?,
        ))
    })?;
    rows.map(|row| {
        let (rowid, subject, predicate, object, updated_at, namespace) = row?;
        let (subject, predicate, object) = (subject.trim(), predicate.trim(), object.trim());
        let item = (!subject.is_empty() && !object.is_empty()).then(|| {
            let mut meta = import_meta(ws, format!("{}:{rowid}", table.name()));
            meta.tags = vec!["graph".to_string()];
            if let Some(namespace) = namespace.filter(|ns| !ns.trim().is_empty()) {
                meta.tags.push(format!("ns:{namespace}"));
            }
            meta.observed_at = updated_at.and_then(convert::from_unix_seconds);
            let text = [subject, predicate, object]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            StoreItem::Learning {
                text,
                kind: LearningKind::Fact,
                confidence: convert::DEFAULT_CONFIDENCE,
                evidence: None,
                meta,
            }
        });
        Ok(Scanned {
            item,
            mark: table.mark(rowid),
        })
    })
    .collect()
}

/// `table`'s relations whose subject and object have text.
pub(super) fn count(ws: &LegacyWorkspace, table: Table) -> Result<u64> {
    let Some(memory) = ws.memory.as_ref().filter(|_| table.present(ws)) else {
        return Ok(0);
    };
    let sql = format!(
        "SELECT COUNT(*) FROM {} WHERE {} AND {}{}",
        table.name(),
        has_text("subject"),
        has_text("object"),
        connector_filter(ws, table)
    );
    Ok(count_of(memory.query_row(&sql, [], |row| row.get(0))?))
}
