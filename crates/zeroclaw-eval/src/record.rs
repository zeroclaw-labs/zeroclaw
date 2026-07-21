//! The artifact produced by running one eval case — what graders score.

use serde::Serialize;
use zeroclaw_api::model_provider::ConversationMessage;

/// The schema tag stamped on every serialized run record.
pub const RECORD_SCHEMA: &str = "zeroclaw-eval/record/v1";

/// Informational stamp of the sandbox posture a case ran under.
#[derive(Debug, Clone, Serialize)]
pub struct SandboxStamp {
    pub autonomy: String,
    pub workspace_only: bool,
}

/// Everything captured from a single agent run, ready for grading and for a
/// comparable, dumpable receipt.
#[derive(Debug, Serialize)]
pub struct RunRecord {
    /// Schema tag: always [`RECORD_SCHEMA`].
    pub schema: String,
    /// The execution mode that produced this record.
    pub mode: crate::Mode,
    /// The case's report identity (`trace.display_id()`).
    pub case_id: String,
    /// SHA-256 hex of the case's canonical JSON, for comparability.
    pub case_hash: String,
    /// Provider identity: `"scripted"` for replay; `"<type>.<alias>:<model>"` for live.
    pub provider_ref: String,
    /// Sorted effective tool allowlist (`[]` for replay / echo-only).
    pub tool_surface: Vec<String>,
    /// The sandbox posture the case ran under.
    pub sandbox: SandboxStamp,
    /// The agent's final text response for the case.
    pub final_response: String,
    /// The full conversation trajectory (messages + tool calls + tool results).
    pub history: Vec<ConversationMessage>,
    /// Names of tools that were dispatched, in call order.
    pub tools_called: Vec<String>,
    /// Whether every dispatched tool call succeeded.
    pub all_tools_succeeded: bool,
    /// Accumulated input tokens reported by the provider.
    pub input_tokens: u64,
    /// Accumulated output tokens reported by the provider.
    pub output_tokens: u64,
    /// Wall-clock duration of the turns loop, in milliseconds.
    pub duration_ms: u64,
    /// Number of LLM responses observed during the run.
    pub llm_calls: u32,
}
