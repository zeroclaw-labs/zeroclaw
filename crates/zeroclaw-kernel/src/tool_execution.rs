//! Shared outcome of the existing engine effect executor.
use std::time::Duration;

pub struct ToolExecutionOutcome {
    /// Text handed to the model and persisted to provider history. The
    /// success path carries raw bytes; the failure paths of `execute_one_tool`
    /// fold a tool's detailed error body (which can reflect a token or signed
    /// URL) into this text and credential-scrub it before storing it here.
    pub output: String,
    /// Attachments the tool declared for this result (`ToolOutput::attachments`).
    /// They reach history through the carrier grammar and are never recovered
    /// by scanning `output`.
    pub attachments: Vec<zeroclaw_api::media::RenderedMarker>,
    /// Structured output when the tool declared one (`ToolOutput::data`).
    /// Feeds SOP step capture and data-flow surfaces; the LLM sees only
    /// `output`. Stored raw — consumers scrub at their own rendering boundary.
    pub output_data: Option<serde_json::Value>,
    pub success: bool,
    /// Raw, unscrubbed failure text for trusted in-process consumers (SOP step
    /// capture, data-flow surfaces). Credential scrubbing is a rendering
    /// concern applied at each human-facing surface (observer events,
    /// post-execution log line, CLI progress) and, unlike this field, on the
    /// model-visible `output`.
    pub error_reason: Option<String>,
    pub duration: Duration,
    /// Cryptographic HMAC receipt proving this tool actually executed.
    /// Present only when tool receipts are enabled in config.
    pub receipt: Option<String>,
}
