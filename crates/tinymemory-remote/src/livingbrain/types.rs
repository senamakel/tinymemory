//! Typed request and response values for the LivingBrain API.

use serde::Deserialize;
use serde_json::{json, Value};

/// A kind of content LivingBrain can capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureKind {
    /// A text note.
    Note,
    /// Arbitrary text content.
    Text,
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
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::Text => "text",
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
    /// Host provenance retained by LivingBrain with the capture.
    pub source: Option<String>,
    /// An optional display label.
    pub label: Option<String>,
}

impl Capture {
    pub(super) fn validate(&self) -> anyhow::Result<()> {
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
        for (name, value) in [
            ("LivingBrain origin_ref", self.origin_ref.as_deref()),
            ("LivingBrain source", self.source.as_deref()),
        ] {
            if let Some(value) = value {
                anyhow::ensure!(!value.trim().is_empty(), "{name} must not be empty");
            }
        }
        Ok(())
    }

    pub(super) fn to_json(&self) -> Value {
        json!({
            "kind": self.kind.as_str(),
            "content": self.content,
            "fetchUrl": self.fetch_url,
            "originRef": self.origin_ref,
            "source": self.source,
            "label": self.label,
        })
    }
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
    pub(super) fn as_str(self) -> &'static str {
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
    pub(super) fn validate(&self) -> anyhow::Result<()> {
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

    pub(super) fn to_json(&self) -> Value {
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

/// A source created or accepted by LivingBrain capture ingestion.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CaptureReceipt {
    /// Service-assigned ingest-source id.
    pub id: String,
    /// Native ingestion status, when returned by the endpoint.
    #[serde(default)]
    pub status: Option<String>,
}

/// Per-source outcomes returned from a batch capture submission.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CaptureBatchReceipt {
    /// The individual sources accepted or rejected by the service.
    pub items: Vec<CaptureReceipt>,
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
pub(super) struct SourceList {
    pub(super) items: Vec<CaptureSource>,
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
