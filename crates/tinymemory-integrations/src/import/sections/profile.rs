//! `user_profile`: one preference learning per live facet.
//!
//! Facets are walked by `facet_id`. When the columns exist, facets with
//! `state = 'dropped'` (the v1 engine retired them) or
//! `user_state = 'forgotten'` (the user asked to forget them) are skipped in
//! the query itself. A facet whose value is blank is skipped too.

use rusqlite::params;
use tinymemory_api::{LearningKind, StoreItem};

use super::{Mark, Scanned, count_of, has_text, import_meta, push_unique, sql_limit};
use crate::import::convert;
use crate::import::error::Result;
use crate::import::workspace::LegacyWorkspace;

/// One `user_profile` row.
#[derive(Debug)]
struct FacetRow {
    facet_id: String,
    facet_type: String,
    key: String,
    value: String,
    confidence: Option<f64>,
    last_seen_at: Option<f64>,
    class: Option<String>,
    evidence: Option<String>,
}

/// The live facets `page` keeps, with a value that has text.
pub(super) fn count(ws: &LegacyWorkspace) -> Result<u64> {
    let Some(memory) = &ws.memory else {
        return Ok(0);
    };
    let sql = format!(
        "SELECT COUNT(*) FROM user_profile WHERE {}{}",
        has_text("value"),
        live_filters(ws)
    );
    Ok(count_of(memory.query_row(&sql, [], |row| row.get(0))?))
}

/// The `AND …` filters that drop retired and forgotten facets, for the
/// columns this store has.
fn live_filters(ws: &LegacyWorkspace) -> String {
    let mut filters = String::new();
    if ws.skip_connector_syncs {
        filters.push_str(" AND ");
        filters.push_str(super::connector::PROFILE_KEPT);
    }
    if ws.schema.profile_state {
        filters.push_str(" AND state IS NOT 'dropped'");
    }
    if ws.schema.profile_user_state {
        filters.push_str(" AND user_state IS NOT 'forgotten'");
    }
    filters
}

/// The next page of facets after `after`.
pub(super) fn page(
    ws: &LegacyWorkspace,
    after: Option<&str>,
    limit: usize,
) -> Result<Vec<Scanned>> {
    let Some(memory) = &ws.memory else {
        return Ok(Vec::new());
    };
    let schema = ws.schema;
    let class = if schema.profile_class {
        "class"
    } else {
        "NULL"
    };
    let evidence = if schema.profile_evidence {
        "evidence_refs_json"
    } else {
        "NULL"
    };
    // Same rows `count` counts: live facets whose value has text.
    let filters = format!(" AND {}{}", has_text("value"), live_filters(ws));
    let sql = format!(
        "SELECT facet_id, facet_type, key, value, confidence, last_seen_at, {class}, {evidence} \
         FROM user_profile WHERE (?1 IS NULL OR facet_id > ?1){filters} \
         ORDER BY facet_id LIMIT ?2"
    );
    let mut stmt = memory.prepare(&sql)?;
    let rows = stmt.query_map(params![after, sql_limit(limit)], |row| {
        Ok(FacetRow {
            facet_id: row.get(0)?,
            facet_type: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            key: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
            value: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
            confidence: row.get(4)?,
            last_seen_at: row.get(5)?,
            class: row.get(6)?,
            evidence: row.get(7)?,
        })
    })?;
    rows.map(|row| {
        let row = row?;
        Ok(Scanned {
            item: facet(ws, &row),
            mark: Mark::Profile(row.facet_id),
        })
    })
    .collect()
}

fn facet(ws: &LegacyWorkspace, row: &FacetRow) -> Option<StoreItem> {
    if row.value.trim().is_empty() {
        return None;
    }
    let mut meta = import_meta(ws, format!("user_profile:{}", row.facet_id));
    let mut tags = Vec::new();
    for tag in [Some(&row.facet_type), row.class.as_ref()]
        .into_iter()
        .flatten()
    {
        if !tag.trim().is_empty() {
            push_unique(&mut tags, tag.clone());
        }
    }
    meta.tags = tags;
    meta.observed_at = row.last_seen_at.and_then(convert::from_unix_seconds);
    Some(StoreItem::Learning {
        text: format!("{}: {}", row.key, row.value),
        kind: LearningKind::Preference,
        confidence: convert::confidence(row.confidence),
        evidence: row
            .evidence
            .as_ref()
            .filter(|evidence| !evidence.trim().is_empty())
            .cloned(),
        meta,
    })
}
