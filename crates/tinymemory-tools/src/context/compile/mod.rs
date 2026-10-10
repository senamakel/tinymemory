//! [`ContextCompiler`]: `context.md` as a holistic recall.
//!
//! The document is [`crate::recall`] with a fixed shape: one answered
//! section per brief (in order), then the latest learnings as a list, under
//! `# Context` with frontmatter. A brief whose recall fails, or that cites
//! nothing, is skipped (a failure is logged); a failed learnings listing
//! leaves the learnings out. Neither fails the document, so an engine that
//! holds nothing yields an empty document.

use chrono::{DateTime, Utc};
use serde::Serialize;
use tinymemory_api::{ItemId, ItemKind, MemoryEngine, MetaFilter};

use crate::context::error::{Error, Result};
use crate::context::spec::ContextSpec;
use crate::recall::{self, Frontmatter, HolisticRecall, ScopeSection, SectionQuery};

pub use crate::recall::estimate_tokens;

/// Most citations one brief's recall gathers.
const BRIEF_CITATIONS: usize = 8;

/// The document's `#` heading.
const TITLE: &str = "Context";

/// The learnings section's heading.
const LEARNINGS_HEADING: &str = "Learnings";

/// Instructions sent with every brief's recall.
const BRIEF_INSTRUCTIONS: &str = "Answer briefly, as markdown bullet points suitable for a \
     context brief. State only what the stored items support.";

/// A compiled `context.md`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ContextDoc {
    /// The document; empty when the engine had nothing to say.
    pub markdown: String,
    /// The document's estimated tokens, four characters per token.
    pub tokens: usize,
    /// When it was compiled.
    pub generated_at: DateTime<Utc>,
    /// The id of the engine it was compiled from.
    pub engine: String,
    /// Every item the document cites, in order of first citation.
    pub refs: Vec<ItemId>,
}

/// Compiles `context.md` documents.
#[derive(Debug, Clone, Copy, Default)]
pub struct ContextCompiler {
    at: Option<DateTime<Utc>>,
}

impl ContextCompiler {
    /// A compiler stamping documents with the current time.
    #[must_use]
    pub fn new() -> Self {
        Self { at: None }
    }

    /// A compiler stamping every document with `generated_at`, for
    /// reproducible output.
    #[must_use]
    pub fn at(generated_at: DateTime<Utc>) -> Self {
        Self {
            at: Some(generated_at),
        }
    }

    /// Compiles `spec` against `engine`.
    ///
    /// # Errors
    ///
    /// [`crate::context::Error::InvalidSpec`] when the spec cannot produce a document.
    /// Engine failures are not errors: see the module docs.
    pub async fn compile(
        &self,
        engine: &dyn MemoryEngine,
        spec: &ContextSpec,
    ) -> Result<ContextDoc> {
        spec.validate()?;
        let generated_at = self.at.unwrap_or_else(Utc::now);
        let engine_id = engine.descriptor().id;
        let frontmatter = Frontmatter {
            engine: engine_id,
            generated_at,
        };
        let pack = recall::run(engine, &holistic(spec), Some(frontmatter), None, None)
            .await
            .map_err(|error| Error::InvalidSpec(error.to_string()))?;
        log::debug!(
            "[context] compiled engine={engine_id} tokens={} refs={}",
            pack.tokens,
            pack.refs.len()
        );
        Ok(ContextDoc {
            markdown: pack.markdown,
            tokens: pack.tokens,
            generated_at,
            engine: engine_id.to_string(),
            refs: pack.refs,
        })
    }
}

/// Compiles `spec` against `engine`, stamped with the current time.
///
/// # Errors
///
/// [`crate::context::Error::InvalidSpec`] when the spec cannot produce a document.
pub async fn compile(engine: &dyn MemoryEngine, spec: &ContextSpec) -> Result<ContextDoc> {
    ContextCompiler::new().compile(engine, spec).await
}

/// The holistic recall `spec` describes: one answered section per brief,
/// then the latest learnings, every read confined to `spec.reach`.
fn holistic(spec: &ContextSpec) -> HolisticRecall {
    let mut sections: Vec<ScopeSection> = spec
        .briefs
        .iter()
        .map(|brief| {
            let mut filter = brief.filter.clone();
            if spec.reach.is_some() {
                filter.reach = spec.reach.clone();
            }
            ScopeSection {
                heading: brief.heading.clone(),
                filter,
                limit: BRIEF_CITATIONS,
                query: SectionQuery::Answer {
                    question: brief.question.clone(),
                    instructions: Some(BRIEF_INSTRUCTIONS.to_string()),
                    fallback_to_fetch: false,
                },
                max_scopes: None,
            }
        })
        .collect();
    if spec.learnings_limit > 0 {
        sections.push(ScopeSection::latest(
            LEARNINGS_HEADING,
            MetaFilter {
                reach: spec.reach.clone(),
                ..MetaFilter::kinds([ItemKind::Learning])
            },
            spec.learnings_limit,
        ));
    }
    HolisticRecall {
        budget_tokens: spec.budget_tokens,
        title: TITLE.to_string(),
        ..HolisticRecall::new(None, sections)
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
