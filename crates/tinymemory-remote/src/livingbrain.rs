//! LivingBrain's hosted Brain API client.
//!
//! This is deliberately not a [`tinymemory_api::traits::Memory`] adapter.
//! LivingBrain accepts asynchronous captures and exposes compiled pages, not
//! TinyMemory's exact `(namespace, key)` record contract.

use reqwest::Method;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::common::{Attempts, HttpClient};

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
        self.client
            .json(
                Method::POST,
                &format!("v1/brains/{}/captures", self.brain_id),
                Some(&capture.to_json()),
                Attempts::Once,
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
        value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')),
        "{name} must be a URL-safe identifier"
    );
    Ok(())
}

/// A kind of content LivingBrain can capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureKind {
    /// A text note.
    Note,
    /// A remote or uploaded file.
    File,
    /// A web URL.
    Url,
    /// An audio or textual transcript.
    Transcript,
    /// An agent/user chat turn.
    ChatTurn,
    /// Content received from an integration.
    Integration,
}

impl CaptureKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::File => "file",
            Self::Url => "url",
            Self::Transcript => "transcript",
            Self::ChatTurn => "chat_turn",
            Self::Integration => "integration",
        }
    }
}

/// A capture submitted to LivingBrain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    /// The source-content kind.
    pub kind: CaptureKind,
    /// Inline textual content, mutually exclusive with [`Self::fetch_url`].
    pub content: Option<String>,
    /// A URL LivingBrain should fetch, mutually exclusive with [`Self::content`].
    pub fetch_url: Option<String>,
    /// Stable host event id used by LivingBrain for deduplication.
    pub origin_ref: Option<String>,
    /// An optional display label.
    pub label: Option<String>,
}

/// A sender role for a conversation turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatSender {
    /// A turn authored by the end user.
    User,
    /// A turn authored by an agent.
    Agent,
}

impl ChatSender {
    fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
        }
    }
}

/// A conversation turn submitted for optional capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatTurn {
    /// Text spoken in the turn.
    pub text: String,
    /// Who authored the turn.
    pub sender: ChatSender,
    /// Stable host event id used for deduplication when the turn is captured.
    pub origin_ref: Option<String>,
    /// Optional name of the responding agent.
    pub agent_name: Option<String>,
}

impl ChatTurn {
    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.text.trim().is_empty(),
            "LivingBrain chat turn text must not be empty"
        );
        if let Some(origin_ref) = &self.origin_ref {
            anyhow::ensure!(
                !origin_ref.trim().is_empty(),
                "LivingBrain chat turn origin_ref must not be empty"
            );
        }
        Ok(())
    }

    fn to_json(&self) -> Value {
        json!({
            "text": self.text,
            "sender": self.sender.as_str(),
            "originRef": self.origin_ref,
            "agentName": self.agent_name,
        })
    }
}

/// LivingBrain's decision about a submitted conversation turn.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ChatTurnReceipt {
    /// Whether LivingBrain accepted the turn for capture.
    pub worthy: bool,
    /// The service's explanation for the decision.
    pub reason: String,
    /// The ingest-source id when the turn was accepted.
    #[serde(rename = "sourceId", default)]
    pub source_id: Option<String>,
}

impl Capture {
    fn validate(&self) -> anyhow::Result<()> {
        let has_content = self
            .content
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
        let has_url = self
            .fetch_url
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
        anyhow::ensure!(
            has_content != has_url,
            "LivingBrain capture needs exactly one of content or fetch_url"
        );
        if let Some(origin_ref) = &self.origin_ref {
            anyhow::ensure!(
                !origin_ref.trim().is_empty(),
                "LivingBrain origin_ref must not be empty"
            );
        }
        Ok(())
    }

    fn to_json(&self) -> Value {
        json!({
            "kind": self.kind.as_str(),
            "content": self.content,
            "fetchUrl": self.fetch_url,
            "originRef": self.origin_ref,
            "label": self.label,
        })
    }
}

/// A source created or accepted by LivingBrain capture ingestion.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CaptureReceipt {
    /// Service-assigned ingest-source id.
    pub id: String,
    /// Native ingestion status, when returned by the endpoint.
    #[serde(default)]
    pub status: Option<String>,
}

/// A captured source and its current asynchronous-ingestion state.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CaptureSource {
    /// Service-assigned ingest-source id.
    pub id: String,
    /// Native ingestion status, when returned by the endpoint.
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Deserialize)]
struct SourceList {
    items: Vec<CaptureSource>,
}

/// One result from LivingBrain's page search.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct LivingBrainSearchResult {
    /// Stable page id.
    #[serde(rename = "pageId")]
    pub page_id: String,
    /// URL-safe page identifier.
    pub slug: String,
    /// Page title.
    pub title: String,
    /// Search-result summary.
    pub summary: String,
    /// LivingBrain's page type.
    #[serde(rename = "pageType")]
    pub page_type: String,
    /// Current LivingBrain page state.
    pub status: String,
    /// Native similarity score in the inclusive range zero to one.
    pub similarity: f64,
}

/// LivingBrain's markdown export payload.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct LivingBrainExport {
    /// The brain represented by this export.
    #[serde(rename = "brainId")]
    pub brain_id: String,
    /// The service's complete markdown bundle.
    pub markdown: String,
}

#[cfg(test)]
#[path = "livingbrain_test.rs"]
mod test;
