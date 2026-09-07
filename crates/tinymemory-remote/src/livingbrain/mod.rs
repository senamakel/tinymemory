//! LivingBrain's hosted Brain API client.
//!
//! This is deliberately not a [`tinymemory_api::traits::Memory`] adapter.
//! LivingBrain accepts asynchronous captures and exposes compiled pages, not
//! TinyMemory's exact `(namespace, key)` record contract.

use reqwest::Method;
use serde_json::{json, Value};

use crate::common::{Attempts, HttpClient};

mod types;

use types::SourceList;
pub use types::{
    Capture, CaptureBatchReceipt, CaptureKind, CaptureReceipt, CaptureSource, ChatSender, ChatTurn,
    ChatTurnReceipt, LivingBrainExport, LivingBrainSearchResult,
};

/// Public base URL for LivingBrain's hosted API.
pub const LIVINGBRAIN_API_ENDPOINT: &str = "https://api.livingbrain.com";

/// A client scoped to exactly one LivingBrain brain and one host subject.
#[derive(Debug)]
pub struct LivingBrain {
    client: HttpClient,
    brain_id: String,
}

impl LivingBrain {
    /// Connects to a particular hosted LivingBrain brain.
    ///
    /// The API key is sent only as a sensitive bearer-authentication header;
    /// every request also carries the given `subject_id` as `x-subject-id`.
    ///
    /// # Errors
    ///
    /// Returns an error when connection fields are blank, the endpoint is not
    /// HTTP(S), or the subject id cannot be encoded as an HTTP header.
    pub fn new(
        endpoint: &str,
        api_key: &str,
        subject_id: &str,
        brain_id: &str,
    ) -> anyhow::Result<Self> {
        for (name, value) in [
            ("LivingBrain API key", api_key),
            ("LivingBrain subject id", subject_id),
            ("LivingBrain brain id", brain_id),
        ] {
            anyhow::ensure!(!value.trim().is_empty(), "{name} must not be empty");
        }
        validate_path_segment(brain_id, "LivingBrain brain id")?;
        Ok(Self {
            client: HttpClient::bearer_with_subject(endpoint, api_key, subject_id)?,
            brain_id: brain_id.into(),
        })
    }

    /// Connects to LivingBrain's hosted API.
    ///
    /// # Errors
    ///
    /// Returns an error when a connection field is blank or invalid.
    pub fn cloud(api_key: &str, subject_id: &str, brain_id: &str) -> anyhow::Result<Self> {
        Self::new(LIVINGBRAIN_API_ENDPOINT, api_key, subject_id, brain_id)
    }

    /// Rebuilds the transport with a different per-request deadline.
    ///
    /// # Errors
    ///
    /// Fails only if the underlying HTTP client cannot be rebuilt.
    pub fn with_request_timeout(mut self, timeout: std::time::Duration) -> anyhow::Result<Self> {
        self.client = self.client.clone().with_timeout(timeout)?;
        Ok(self)
    }

    /// Submits one capture for asynchronous ingestion.
    ///
    /// A nonempty `origin_ref` makes a caller retry safe: LivingBrain uses it
    /// as its idempotency key. The client does not generate one on its own.
    ///
    /// # Errors
    ///
    /// Returns an error when the capture shape is invalid or the service
    /// rejects or cannot accept it.
    pub async fn capture(&self, capture: &Capture) -> anyhow::Result<CaptureReceipt> {
        capture.validate()?;
        let attempts = if capture.origin_ref.is_some() {
            Attempts::RetryTransient
        } else {
            Attempts::Once
        };
        self.client
            .json(
                Method::POST,
                &format!("v1/brains/{}/captures", self.brain_id),
                Some(&capture.to_json()),
                attempts,
            )
            .await
    }

    /// Submits a bounded batch of captures for asynchronous ingestion.
    ///
    /// Every capture needs a stable `origin_ref`, so a transient retry cannot
    /// create duplicate sources.
    ///
    /// # Errors
    ///
    /// Returns an error when the batch is empty, exceeds the service limit,
    /// contains an invalid capture, or the service rejects it.
    pub async fn capture_batch(&self, captures: &[Capture]) -> anyhow::Result<CaptureBatchReceipt> {
        const MAX_BATCH_SIZE: usize = 100;
        anyhow::ensure!(
            !captures.is_empty(),
            "LivingBrain capture batch must not be empty"
        );
        anyhow::ensure!(
            captures.len() <= MAX_BATCH_SIZE,
            "LivingBrain capture batch must contain at most {MAX_BATCH_SIZE} captures"
        );
        for capture in captures {
            capture.validate()?;
            anyhow::ensure!(
                capture.origin_ref.is_some(),
                "LivingBrain batch captures require an origin_ref"
            );
        }
        self.client
            .json(
                Method::POST,
                &format!("v1/brains/{}/captures/batch", self.brain_id),
                Some(&json!({ "captures": captures.iter().map(Capture::to_json).collect::<Vec<_>>() })),
                Attempts::RetryTransient,
            )
            .await
    }

    /// Submits one conversation turn for LivingBrain's worthiness classifier.
    ///
    /// The service may successfully decline to capture a turn. In that case
    /// [`ChatTurnReceipt::worthy`] is false and `source_id` is absent.
    ///
    /// # Errors
    ///
    /// Returns an error when the turn is invalid or the service rejects it.
    pub async fn capture_chat_turn(&self, turn: &ChatTurn) -> anyhow::Result<ChatTurnReceipt> {
        turn.validate()?;
        self.client
            .json(
                Method::POST,
                &format!("v1/brains/{}/captures/chat-turn", self.brain_id),
                Some(&turn.to_json()),
                Attempts::Once,
            )
            .await
    }

    /// Searches LivingBrain's compiled pages using its native ranking.
    ///
    /// # Errors
    ///
    /// Returns an error when the query or bounds are invalid, or the service
    /// cannot complete the search.
    pub async fn search(
        &self,
        query: &str,
        top_k: usize,
        min_similarity: Option<f64>,
    ) -> anyhow::Result<Vec<LivingBrainSearchResult>> {
        anyhow::ensure!(
            !query.trim().is_empty(),
            "LivingBrain search query must not be empty"
        );
        anyhow::ensure!(top_k > 0, "LivingBrain search top_k must be positive");
        if let Some(min_similarity) = min_similarity {
            anyhow::ensure!(
                (0.0..=1.0).contains(&min_similarity),
                "LivingBrain minimum similarity must be between zero and one"
            );
        }
        self.client
            .json(
                Method::POST,
                &format!("v1/brains/{}/search", self.brain_id),
                Some(&json!({
                    "query": query,
                    "topK": top_k,
                    "minSimilarity": min_similarity,
                })),
                Attempts::RetryTransient,
            )
            .await
    }

    /// Reads one native LivingBrain page by slug.
    ///
    /// The page model evolves independently of TinyMemory, so this method
    /// preserves it as JSON rather than pretending it is an exact record.
    ///
    /// # Errors
    ///
    /// Returns an error when `slug` is blank or the service cannot read it.
    pub async fn page(&self, slug: &str) -> anyhow::Result<Value> {
        validate_path_segment(slug, "LivingBrain page slug")?;
        self.client
            .json(
                Method::GET,
                &format!("v1/brains/{}/pages/{slug}", self.brain_id),
                None,
                Attempts::RetryTransient,
            )
            .await
    }

    /// Lists the native LivingBrain pages for the configured brain.
    ///
    /// Page fields are intentionally kept as JSON because LivingBrain evolves
    /// this model independently of TinyMemory's exact-record contract.
    ///
    /// # Errors
    ///
    /// Returns an error when the service cannot list the pages.
    pub async fn pages(&self) -> anyhow::Result<Value> {
        self.client
            .json(
                Method::GET,
                &format!("v1/brains/{}/pages", self.brain_id),
                None,
                Attempts::RetryTransient,
            )
            .await
    }

    /// Returns the service's graph payload for this brain.
    ///
    /// # Errors
    ///
    /// Returns an error when the service cannot retrieve the graph.
    pub async fn graph(&self) -> anyhow::Result<Value> {
        self.client
            .json(
                Method::GET,
                &format!("v1/brains/{}/graph", self.brain_id),
                None,
                Attempts::RetryTransient,
            )
            .await
    }

    /// Lists ingestion-source status for the configured brain.
    ///
    /// # Errors
    ///
    /// Returns an error when the service cannot retrieve source status.
    pub async fn sources(&self) -> anyhow::Result<Vec<CaptureSource>> {
        let response: SourceList = self
            .client
            .json(
                Method::GET,
                &format!("v1/brains/{}/sources", self.brain_id),
                None,
                Attempts::RetryTransient,
            )
            .await?;
        Ok(response.items)
    }

    /// Deletes one ingest source created in this brain.
    ///
    /// This is primarily useful for caller-managed cleanup of temporary
    /// captures; it does not delete arbitrary pages by slug.
    ///
    /// # Errors
    ///
    /// Returns an error when `source_id` is invalid or the service rejects the
    /// deletion.
    pub async fn remove_source(&self, source_id: &str) -> anyhow::Result<()> {
        validate_path_segment(source_id, "LivingBrain source id")?;
        self.client
            .empty(
                Method::DELETE,
                &format!("v1/brains/{}/sources/{source_id}", self.brain_id),
                None,
            )
            .await?;
        Ok(())
    }

    /// Exports the configured brain as LivingBrain's markdown bundle.
    ///
    /// # Errors
    ///
    /// Returns an error when the service cannot produce the export.
    pub async fn export_markdown(&self) -> anyhow::Result<LivingBrainExport> {
        self.client
            .json(
                Method::GET,
                &format!("v1/brains/{}/export/markdown", self.brain_id),
                None,
                Attempts::RetryTransient,
            )
            .await
    }
}

/// Rejects characters that could change the shape of a URL path assembled
/// from a caller-controlled brain id or page slug. LivingBrain ids and slugs
/// are opaque but URL-safe identifiers, so accepting query, fragment, or path
/// separators would be an input bug, not a compatibility feature.
fn validate_path_segment(value: &str, name: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!value.trim().is_empty(), "{name} must not be empty");
    anyhow::ensure!(
        !matches!(value, "." | ".."),
        "{name} must not be a dot path segment"
    );
    anyhow::ensure!(
        value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')),
        "{name} must be a URL-safe identifier"
    );
    Ok(())
}

#[cfg(test)]
mod test;
