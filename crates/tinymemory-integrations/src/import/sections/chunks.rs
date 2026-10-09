//! `memory_tree/chunks.db`: one item per ingested source.
//!
//! Chunks are grouped by `(source_kind, source_id)` and read in
//! `(seq_in_source, id)` order. Each chunk's text is its full body from
//! `memory_tree/content/<content_path>` when that column is set and the file
//! exists, else the stored preview (`content`, at most 500 characters).
//!
//! A `chat` source becomes a conversation with one [`Role::User`] turn per
//! chunk: chat chunks are transcripts of host channels, whose speakers are
//! people rather than the assistant. Every other source kind (`document`,
//! `email`) becomes one document whose body is its chunks joined by blank
//! lines.

use std::io::ErrorKind;
use std::path::{Component, Path};

use rusqlite::params;
use tinymemory_api::{DocumentBody, Role, StoreItem, Turn, TurnRange};

use super::connector;
use super::{Mark, Scanned, import_meta, push_unique, sql_limit};
use crate::import::checkpoint::ChunkCursor;
use crate::import::convert;
use crate::import::error::{Error, Result};
use crate::import::workspace::{ChunkStore, LegacyWorkspace};

/// One chunk with its body resolved.
#[derive(Debug)]
struct Chunk {
    text: String,
    timestamp_ms: i64,
    tags_json: String,
}

/// The next page of sources after `after`; empty when the workspace has no
/// chunk store.
pub(super) fn page(
    ws: &LegacyWorkspace,
    after: Option<&ChunkCursor>,
    limit: usize,
) -> Result<Vec<Scanned>> {
    let Some(store) = &ws.chunks else {
        return Ok(Vec::new());
    };
    let mut stmt = store.conn.prepare(
        "SELECT DISTINCT source_kind, source_id FROM mem_tree_chunks \
         WHERE (?1 IS NULL OR (source_kind, source_id) > (?1, ?2)) \
         ORDER BY source_kind, source_id LIMIT ?3",
    )?;
    let kind = after.map(|cursor| cursor.source_kind.as_str());
    let id = after.map(|cursor| cursor.source_id.as_str());
    let sources = stmt
        .query_map(params![kind, id, sql_limit(limit)], |row| {
            Ok(ChunkCursor {
                source_kind: row.get(0)?,
                source_id: row.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    sources
        .into_iter()
        .map(|source| {
            let item = if skipped(ws, store, &source)? {
                None
            } else {
                source_item(ws, store, &source)?
            };
            Ok(Scanned {
                item,
                mark: Mark::Chunk(source),
            })
        })
        .collect()
}

/// Sources with a chunk that has text, resolved by the same reader the
/// import uses: a chunk's text may live in a file, so this reads the chunk
/// bodies (but decodes no item).
pub(super) fn count(ws: &LegacyWorkspace) -> Result<u64> {
    let Some(store) = &ws.chunks else {
        return Ok(0);
    };
    let mut stmt = store
        .conn
        .prepare("SELECT DISTINCT source_kind, source_id FROM mem_tree_chunks")?;
    let sources = stmt
        .query_map([], |row| {
            Ok(ChunkCursor {
                source_kind: row.get(0)?,
                source_id: row.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut total = 0;
    for source in sources {
        if !skipped(ws, store, &source)? && !chunks(store, &source)?.is_empty() {
            total = u64::saturating_add(total, 1);
        }
    }
    Ok(total)
}

/// Whether the workspace leaves this source out as a connector sync: its
/// kind or id says so ([`connector::chunk_source_by_identity`]), or, when the
/// store has an `owner` column, any of its chunks has an owner matching
/// [`connector::OWNER_SYNC_PATTERN`]. The one test `page` and [`count`] share.
fn skipped(ws: &LegacyWorkspace, store: &ChunkStore, source: &ChunkCursor) -> Result<bool> {
    if !ws.skip_connector_syncs {
        return Ok(false);
    }
    if connector::chunk_source_by_identity(&source.source_kind, &source.source_id) {
        return Ok(true);
    }
    if !store.owner {
        return Ok(false);
    }
    let by_owner = store.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM mem_tree_chunks \
         WHERE source_kind = ?1 AND source_id = ?2 AND owner LIKE ?3)",
        params![
            source.source_kind,
            source.source_id,
            connector::OWNER_SYNC_PATTERN
        ],
        |row| row.get::<_, bool>(0),
    )?;
    Ok(by_owner)
}

fn source_item(
    ws: &LegacyWorkspace,
    store: &ChunkStore,
    source: &ChunkCursor,
) -> Result<Option<StoreItem>> {
    let chunks = chunks(store, source)?;
    if chunks.is_empty() {
        return Ok(None);
    }
    let mut meta = import_meta(
        ws,
        format!(
            "mem_tree_chunks:{}:{}",
            source.source_kind, source.source_id
        ),
    );
    let mut tags = Vec::new();
    for chunk in &chunks {
        for tag in convert::string_array(&chunk.tags_json) {
            push_unique(&mut tags, tag);
        }
    }
    push_unique(&mut tags, format!("source_kind:{}", source.source_kind));
    meta.tags = tags;
    meta.observed_at = chunks
        .iter()
        .map(|chunk| chunk.timestamp_ms)
        .max()
        .and_then(convert::from_unix_millis);
    if source.source_kind == "chat" {
        meta.thread_id = Some(source.source_id.clone());
        meta.turns = Some(TurnRange {
            first: 0,
            last: u32::try_from(chunks.len() - 1).unwrap_or(u32::MAX),
        });
        let turns = chunks
            .into_iter()
            .map(|chunk| Turn {
                at: convert::from_unix_millis(chunk.timestamp_ms),
                ..Turn::new(Role::User, chunk.text)
            })
            .collect();
        return Ok(Some(StoreItem::Conversation { turns, meta }));
    }
    let body = chunks
        .into_iter()
        .map(|chunk| chunk.text)
        .collect::<Vec<_>>()
        .join("\n\n");
    Ok(Some(StoreItem::Document {
        title: None,
        body: DocumentBody::Text(body),
        mime: None,
        meta,
    }))
}

/// The source's non-blank chunks in order, with full bodies resolved.
fn chunks(store: &ChunkStore, source: &ChunkCursor) -> Result<Vec<Chunk>> {
    let content_path = if store.content_path {
        "content_path"
    } else {
        "NULL"
    };
    let sql = format!(
        "SELECT content, {content_path}, timestamp_ms, tags_json FROM mem_tree_chunks \
         WHERE source_kind = ?1 AND source_id = ?2 ORDER BY seq_in_source, id"
    );
    let mut stmt = store.conn.prepare(&sql)?;
    let rows = stmt.query_map(params![source.source_kind, source.source_id], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?.unwrap_or_default(),
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<i64>>(2)?.unwrap_or_default(),
            row.get::<_, Option<String>>(3)?.unwrap_or_default(),
        ))
    })?;
    let mut chunks = Vec::new();
    for row in rows {
        let (preview, path, timestamp_ms, tags_json) = row?;
        let full = match path.as_deref() {
            Some(path) => full_body(&store.content_dir, path)?,
            None => None,
        };
        let text = full.unwrap_or(preview);
        if text.trim().is_empty() {
            continue;
        }
        chunks.push(Chunk {
            text,
            timestamp_ms,
            tags_json,
        });
    }
    Ok(chunks)
}

/// Reads `content_dir/<relative>`; `None` when the path is not a plain
/// relative path inside the content directory or the file does not exist.
fn full_body(content_dir: &Path, relative: &str) -> Result<Option<String>> {
    let relative = Path::new(relative);
    let contained = relative.components().next().is_some()
        && relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if !contained {
        return Ok(None);
    }
    let path = content_dir.join(relative);
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::Io { path, source }),
    }
}

#[cfg(test)]
#[path = "chunks_tests.rs"]
mod tests;
