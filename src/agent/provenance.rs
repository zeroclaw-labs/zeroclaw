//! ACP Provenance — audit trail metadata injected into agent prompts.
//!
//! Adapted from OpenClaw v2026.3.8 ACP/Provenance feature.
//! Injects `[PROVENANCE: trace=uuid, origin=channel, user=id, ts=iso, receipt=hash]`
//! into system prompts so every agent action has a verifiable lineage.
//!
//! Per-turn model attribution tracks which provider/model produced each piece of
//! output (text, tool calls, thinking), giving full audit accountability.

use crate::config::schema::ProvenanceMode;
use chrono::Utc;
use sha2::{Digest, Sha256};
use uuid::Uuid;

// ── Per-Turn Model Attribution ──────────────────────────────────

/// What kind of action a model performed in a turn.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnAction {
    /// Direct LLM inference (text generation).
    Inference,
    /// LLM produced tool call(s) that were executed.
    ToolDispatch,
    /// Sub-agent delegation via the delegate tool.
    Delegation,
}

/// A single turn's model attribution — who did what, when.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ModelTurn {
    /// Sequential turn number within the session (1-based).
    pub turn: u32,
    /// Provider that served the model (e.g. "openrouter", "copilot", "lifebook").
    pub provider: String,
    /// Model identifier (e.g. "grok-4.1-fast", "claude-opus-4.6").
    pub model: String,
    /// What the model did this turn.
    pub action: TurnAction,
    /// Number of tool calls produced (0 for pure inference).
    pub tool_calls: u32,
    /// ISO-8601 timestamp of the turn.
    pub timestamp: String,
    /// SHA-256 hash (truncated) of the generated content for tamper detection.
    pub content_hash: Option<String>,
}

impl ModelTurn {
    /// Create a new attribution record for a turn.
    pub fn new(
        turn: u32,
        provider: &str,
        model: &str,
        action: TurnAction,
        tool_calls: u32,
        content: Option<&str>,
    ) -> Self {
        let content_hash = content.map(|c| {
            let mut h = Sha256::new();
            h.update(c.as_bytes());
            hex::encode(&h.finalize()[..16])
        });
        Self {
            turn,
            provider: provider.to_string(),
            model: model.to_string(),
            action,
            tool_calls,
            timestamp: Utc::now().to_rfc3339(),
            content_hash,
        }
    }
}

/// Per-session provenance context. Created once per agent run.
#[derive(Clone, Debug)]
pub struct Provenance {
    pub trace_id: Uuid,
    pub origin: String,
    pub user_id: String,
    pub timestamp: String,
    pub receipt: Option<String>,
    /// Per-turn model attribution log.
    pub turns: Vec<ModelTurn>,
}

impl Provenance {
    /// Create a new provenance record for this session.
    pub fn new(origin: &str, user_id: &str) -> Self {
        Self {
            trace_id: Uuid::new_v4(),
            origin: origin.to_string(),
            user_id: user_id.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            receipt: None,
            turns: Vec::new(),
        }
    }

    /// Record a model turn into the attribution log.
    pub fn add_turn(&mut self, turn: ModelTurn) {
        self.turns.push(turn);
    }

    /// Record all turns from a tool-loop outcome.
    pub fn add_turns(&mut self, turns: Vec<ModelTurn>) {
        self.turns.extend(turns);
    }

    /// Seal the provenance with a SHA-256 receipt of the prompt + tool names.
    /// This makes the provenance tamper-evident — if the prompt or tools change
    /// after sealing, the receipt won't match.
    pub fn seal(&mut self, prompt: &str, tool_names: &[&str]) {
        let mut hasher = Sha256::new();
        hasher.update(self.trace_id.as_bytes());
        hasher.update(prompt.as_bytes());
        for name in tool_names {
            hasher.update(name.as_bytes());
        }
        hasher.update(self.timestamp.as_bytes());
        let hash = hasher.finalize();
        self.receipt = Some(hex::encode(&hash[..16])); // 128-bit truncated for readability
    }

    /// Format the provenance block for system prompt injection.
    pub fn to_prompt_block(&self) -> String {
        let mut parts = vec![
            format!("trace={}", self.trace_id),
            format!("origin={}", self.origin),
            format!("user={}", self.user_id),
            format!("ts={}", self.timestamp),
        ];
        if let Some(ref receipt) = self.receipt {
            parts.push(format!("receipt={}", receipt));
        }
        format!("[PROVENANCE: {}]", parts.join(", "))
    }

    /// Serialize to JSON for memory storage.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "trace_id": self.trace_id.to_string(),
            "origin": self.origin,
            "user_id": self.user_id,
            "timestamp": self.timestamp,
            "receipt": self.receipt,
            "turns": self.turns,
        })
    }
}

/// Build provenance for a session if the mode requires it.
pub fn build_provenance(
    mode: &ProvenanceMode,
    origin: &str,
    user_id: &str,
) -> Option<Provenance> {
    match mode {
        ProvenanceMode::Off => None,
        ProvenanceMode::Meta | ProvenanceMode::MetaReceipt => {
            Some(Provenance::new(origin, user_id))
        }
    }
}

/// Inject provenance into the system prompt if active.
pub fn inject_provenance(
    system_prompt: &mut String,
    provenance: &mut Option<Provenance>,
    mode: &ProvenanceMode,
    prompt_text: &str,
    tool_names: &[&str],
) {
    if let Some(ref mut prov) = provenance {
        if *mode == ProvenanceMode::MetaReceipt {
            prov.seal(prompt_text, tool_names);
        }
        system_prompt.push_str("\n\n");
        system_prompt.push_str(&prov.to_prompt_block());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provenance_round_trip() {
        let mut prov = Provenance::new("telegram:123", "mike");
        assert!(!prov.trace_id.is_nil());
        assert!(prov.receipt.is_none());

        prov.seal("Hello world", &["shell", "file_read"]);
        assert!(prov.receipt.is_some());

        let block = prov.to_prompt_block();
        assert!(block.contains("PROVENANCE"));
        assert!(block.contains("trace="));
        assert!(block.contains("receipt="));
    }

    #[test]
    fn meta_mode_no_receipt() {
        let mode = ProvenanceMode::Meta;
        let mut prov = build_provenance(&mode, "cli", "user1");
        assert!(prov.is_some());

        let mut prompt = "You are an agent.".to_string();
        let snap = prompt.clone();
        inject_provenance(&mut prompt, &mut prov, &mode, &snap, &[]);
        assert!(prompt.contains("PROVENANCE"));
        assert!(!prompt.contains("receipt="));
    }

    #[test]
    fn off_mode_no_injection() {
        let mode = ProvenanceMode::Off;
        let prov = build_provenance(&mode, "cli", "user1");
        assert!(prov.is_none());
    }

    #[test]
    fn model_turn_attribution() {
        let mut prov = Provenance::new("cli", "mike");
        assert!(prov.turns.is_empty());

        prov.add_turn(ModelTurn::new(
            1,
            "openrouter",
            "grok-4.1-fast",
            TurnAction::Inference,
            0,
            Some("Hello world"),
        ));
        prov.add_turn(ModelTurn::new(
            2,
            "copilot",
            "claude-opus-4.6",
            TurnAction::ToolDispatch,
            3,
            Some("Running shell commands..."),
        ));

        assert_eq!(prov.turns.len(), 2);
        assert_eq!(prov.turns[0].provider, "openrouter");
        assert_eq!(prov.turns[1].model, "claude-opus-4.6");
        assert_eq!(prov.turns[1].tool_calls, 3);

        let json = prov.to_json();
        let turns = json["turns"].as_array().unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0]["provider"], "openrouter");
        assert_eq!(turns[1]["action"], "tool_dispatch");
        assert!(turns[0]["content_hash"].is_string());
    }
}
