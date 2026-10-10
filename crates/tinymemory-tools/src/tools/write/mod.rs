//! The write tools: `memory_store` and `memory_forget`.
//!
//! - `memory_store` stores exactly one learning, document or conversation.
//!   Its metadata is built here, never read from the arguments: the
//!   namespace is the scope's `place`, the source is
//!   [`SourceKind::Agent`], the tags are the model's, and `observed_at` is
//!   the time of the call.
//! - `memory_forget` removes by ids or by a non-empty filter. Ids are first
//!   read back with [`MemoryEngine::get`] under the scope's reach, and only
//!   those found are forgotten; the rest are reported as `skipped`, so an id
//!   from outside the reach is never removed. A filter must set at least one
//!   model-facing field before the reach is added (a reach alone would mean
//!   "everything in reach"), and is then confined to the reach.

use chrono::Utc;
use serde_json::Value;
use tinymemory_api::{
    DocumentBody, ForgetReport, ForgetTarget, ItemId, LearningKind, MemoryEngine, MemoryMeta,
    Result, Role, SourceKind, SourceRef, StoreItem, Turn,
};

use super::ToolScope;
use super::args::{Args, invalid, meta_filter};
use super::read::{item_ids, resolve};
use super::render;
use super::spec::schema::{DEFAULT_CONFIDENCE, DEFAULT_LEARNING_KIND, LEARNING_KINDS, ROLES};
use super::spec::{MEMORY_FORGET, MEMORY_STORE};

/// The three shapes `memory_store` takes, exactly one per call.
const STORE_SHAPES: [&str; 3] = ["learning", "document", "conversation"];

/// `memory_store`.
///
/// # Errors
///
/// Invalid arguments (none or several of the three shapes among them), an
/// item the engine refuses as invalid, and the engine's own failures.
pub(crate) async fn store(
    engine: &dyn MemoryEngine,
    scope: &ToolScope,
    value: &Value,
) -> Result<Value> {
    let args = Args::parse(
        MEMORY_STORE,
        value,
        &["learning", "document", "conversation", "tags"],
    )?;
    let shapes: Vec<&str> = STORE_SHAPES
        .into_iter()
        .filter(|shape| args.has(shape))
        .collect();
    let [shape] = shapes.as_slice() else {
        return Err(invalid(
            MEMORY_STORE,
            "pass exactly one of `learning`, `document` or `conversation`",
        ));
    };
    let meta = MemoryMeta {
        namespace: scope.place.clone(),
        source: SourceRef {
            kind: SourceKind::Agent,
            id: None,
        },
        tags: args.strings("tags")?,
        observed_at: Some(Utc::now()),
        ..MemoryMeta::default()
    };
    let item = match *shape {
        "learning" => learning(&args, meta)?,
        "document" => document(&args, meta)?,
        _ => conversation(&args, meta)?,
    };
    Ok(render::store(&engine.store(item).await?))
}

/// `memory_forget`.
///
/// # Errors
///
/// Invalid arguments (neither or both of `ids` and `filter`, or a filter that
/// sets nothing), and the engine's own failures.
pub(crate) async fn forget(
    engine: &dyn MemoryEngine,
    scope: &ToolScope,
    value: &Value,
) -> Result<Value> {
    let args = Args::parse(MEMORY_FORGET, value, &["ids", "filter"])?;
    match (args.has("ids"), args.has("filter")) {
        (true, false) => {
            let ids = item_ids(&args, "ids")?;
            let found: Vec<ItemId> = resolve(engine, scope, &ids)
                .await?
                .into_iter()
                .map(|hit| hit.id)
                .collect();
            let skipped: Vec<ItemId> = ids.into_iter().filter(|id| !found.contains(id)).collect();
            // A scoped forget looks the ids up only inside the scope's reach,
            // never across the whole tree.
            let report = match (&scope.reach, found.is_empty()) {
                (_, true) => ForgetReport::default(),
                (Some(reach), false) => engine.forget_within(found, reach.clone()).await?,
                (None, false) => engine.forget(ForgetTarget::Ids(found)).await?,
            };
            Ok(render::forget(&report, &skipped))
        }
        (false, true) => {
            let mut filter = meta_filter(&args, "filter")?;
            if filter.is_empty() {
                return Err(args.field_error(
                    "filter",
                    "must set at least one field; an empty filter would mean everything",
                ));
            }
            scope.confine(&mut filter);
            let report = engine.forget(ForgetTarget::Filter(filter)).await?;
            Ok(render::forget(&report, &[]))
        }
        _ => Err(invalid(
            MEMORY_FORGET,
            "pass exactly one of `ids` or `filter`",
        )),
    }
}

fn learning(args: &Args<'_>, meta: MemoryMeta) -> Result<StoreItem> {
    let Some(learning) = args.object(
        "learning",
        "learning.",
        &["text", "learning_kind", "confidence", "evidence"],
    )?
    else {
        return Err(args.field_error("learning", "is required"));
    };
    let kind = learning
        .string("learning_kind")?
        .unwrap_or_else(|| DEFAULT_LEARNING_KIND.to_string());
    Ok(StoreItem::Learning {
        text: learning.required_string("text")?,
        kind: learning_kind(&kind).ok_or_else(|| {
            learning.field_error(
                "learning_kind",
                &format!("must be one of {}", LEARNING_KINDS.join(", ")),
            )
        })?,
        confidence: learning.unit("confidence", DEFAULT_CONFIDENCE)?,
        evidence: learning.string("evidence")?,
        meta,
    })
}

fn document(args: &Args<'_>, meta: MemoryMeta) -> Result<StoreItem> {
    let Some(document) = args.object("document", "document.", &["title", "text"])? else {
        return Err(args.field_error("document", "is required"));
    };
    Ok(StoreItem::Document {
        title: document.string("title")?,
        body: DocumentBody::Text(document.required_string("text")?),
        mime: None,
        meta,
    })
}

fn conversation(args: &Args<'_>, meta: MemoryMeta) -> Result<StoreItem> {
    let Some(conversation) = args.object("conversation", "conversation.", &["turns"])? else {
        return Err(args.field_error("conversation", "is required"));
    };
    let raw = conversation.array("turns")?;
    if raw.is_empty() {
        return Err(conversation.field_error("turns", "must hold at least one turn"));
    }
    let turns = raw
        .iter()
        .map(|value| turn(&conversation, value))
        .collect::<Result<Vec<_>>>()?;
    Ok(StoreItem::Conversation { turns, meta })
}

fn turn(conversation: &Args<'_>, value: &Value) -> Result<Turn> {
    let turn = conversation.element("turns", value, "conversation.turns[].", &["role", "text"])?;
    let role_name = turn.required_string("role")?;
    let role = role(&role_name)
        .ok_or_else(|| turn.field_error("role", &format!("must be one of {}", ROLES.join(", "))))?;
    Ok(Turn::new(role, turn.required_string("text")?))
}

fn learning_kind(name: &str) -> Option<LearningKind> {
    serde_json::from_value(Value::String(name.to_string())).ok()
}

fn role(name: &str) -> Option<Role> {
    serde_json::from_value(Value::String(name.to_string())).ok()
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
