//! Disposable authorized history views. These are never a second writer.
use super::*;

#[derive(Clone, Debug)]
pub struct PayloadSelector {
    pub event_sequence: u64,
    /// Event reference followed by registered structural child coordinates.
    pub reference_path: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct HistoricalMessage {
    pub message_id: MessageId,
    pub role: String,
    pub text: String,
    pub files: Vec<String>,
    pub unresolved_parts: Vec<usize>,
    pub unavailable_parts: Vec<UnavailableContentPart>,
    pub content_source: Option<PayloadSelector>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MessageCursor {
    pub through: EventCursor,
    pub before: usize,
    #[serde(default)]
    pub branch: Option<BranchId>,
}

pub struct HistoricalMessagePage {
    pub messages: Vec<HistoricalMessage>,
    pub next_cursor: Option<MessageCursor>,
}

pub use super::catalog::Session as SessionSummary;

/// Bound to an admitted actor and its live authority. Every read rechecks it.
#[async_trait::async_trait]
pub trait SessionHistoryReadPort: Send + Sync {
    async fn current(&self) -> anyhow::Result<SessionSummary>;
    async fn list(&self, limit: usize) -> anyhow::Result<Vec<SessionSummary>>;
    async fn history(
        &self,
        family: &str,
        limit: usize,
    ) -> anyhow::Result<Option<HistoricalMessagePage>>;
}
