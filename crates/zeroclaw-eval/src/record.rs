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

/// The tools a run was actually able to call, recorded at three stages so the
/// receipt can neither under- nor over-report the run's capabilities.
///
/// Recording only the pre-registry request list lies in both directions: an empty
/// effective list still yields the built-in echo registry (under-report), and an
/// allowlisted name that matches no runtime tool is filtered out by the registry
/// yet would still be listed as available (over-report). Two runs whose surfaces
/// genuinely differed would then compare as identical, silently corrupting any
/// baseline built on these receipts.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ToolSurface {
    /// Names from the case ∩ config allowlist, verbatim — before any eval-side
    /// filtering. Sorted.
    pub requested: Vec<String>,
    /// After eval-side filtering, including the unconditional `shell` deny. Sorted.
    pub effective: Vec<String>,
    /// The names the constructed registry actually exposes to `Agent::builder` —
    /// including implicit built-ins such as `echo`, and excluding requested names
    /// that match no runtime tool. Sorted.
    pub registered: Vec<String>,
}

impl ToolSurface {
    /// Build a surface from the three stages, sorting each for stable comparison.
    pub fn new(requested: Vec<String>, effective: Vec<String>, registered: Vec<String>) -> Self {
        let mut s = Self {
            requested,
            effective,
            registered,
        };
        s.requested.sort();
        s.effective.sort();
        s.registered.sort();
        s
    }
}

/// Provenance for a case: everything knowable *before* the fallible work starts,
/// so it is always recorded — including when execution errors, times out, or the
/// provider never answers.
///
/// This is the half of the receipt that makes runs comparable. Attaching it only
/// to successful runs would drop the case hash, mode, provider, tool surface, and
/// sandbox stamp for exactly the cases a baseline most needs to classify.
#[derive(Debug, Clone, Serialize)]
pub struct CaseProvenance {
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
    /// The tools the run could actually call, at each filtering stage.
    pub tool_surface: ToolSurface,
    /// The sandbox posture the case ran under.
    pub sandbox: SandboxStamp,
}

/// The data that only exists once a run finishes: the transcript, the tool
/// trajectory, and the usage counters.
#[derive(Debug, Clone, Serialize)]
pub struct RunCompletion {
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

impl Default for RunCompletion {
    fn default() -> Self {
        Self {
            final_response: String::new(),
            history: Vec::new(),
            tools_called: Vec::new(),
            all_tools_succeeded: true,
            input_tokens: 0,
            output_tokens: 0,
            duration_ms: 0,
            llm_calls: 0,
        }
    }
}

/// Everything captured about a single case run: immutable provenance plus the
/// completion data, which is absent when the run errored before finishing.
#[derive(Debug, Clone, Serialize)]
pub struct RunRecord {
    /// Known before execution; never absent.
    #[serde(flatten)]
    pub provenance: CaseProvenance,
    /// Present only for a run that completed its turns loop.
    #[serde(flatten)]
    pub completion: Option<RunCompletion>,
}

impl RunRecord {
    /// A record for a run that never completed: provenance only.
    pub fn from_provenance(provenance: CaseProvenance) -> Self {
        Self {
            provenance,
            completion: None,
        }
    }

    /// The completion data, or an all-zero stand-in for an errored run. Graders
    /// read through this so an errored record grades as "nothing happened"
    /// rather than panicking or being skipped.
    pub fn completion_or_default(&self) -> std::borrow::Cow<'_, RunCompletion> {
        match &self.completion {
            Some(c) => std::borrow::Cow::Borrowed(c),
            None => std::borrow::Cow::Owned(RunCompletion::default()),
        }
    }

    /// Whether the run produced completion data.
    pub fn is_complete(&self) -> bool {
        self.completion.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provenance() -> CaseProvenance {
        CaseProvenance {
            schema: RECORD_SCHEMA.to_string(),
            mode: crate::Mode::Live,
            case_id: "c".to_string(),
            case_hash: "abc123".to_string(),
            provider_ref: "test.model:m".to_string(),
            tool_surface: ToolSurface::new(
                vec!["shell".into(), "echo".into()],
                vec!["echo".into()],
                vec!["echo".into()],
            ),
            sandbox: SandboxStamp {
                autonomy: "supervised".to_string(),
                workspace_only: true,
            },
        }
    }

    #[test]
    fn provenance_only_record_serializes_every_receipt_field() {
        let record = RunRecord::from_provenance(provenance());
        let v = serde_json::to_value(&record).unwrap();
        // The receipt fields survive without completion data.
        assert_eq!(v["case_hash"], "abc123");
        assert_eq!(v["mode"], "live");
        assert_eq!(v["provider_ref"], "test.model:m");
        assert_eq!(v["tool_surface"]["registered"][0], "echo");
        assert_eq!(v["sandbox"]["workspace_only"], true);
        // Completion fields are absent, not null-filled with fake values.
        assert!(v.get("final_response").is_none());
        assert!(!record.is_complete());
    }

    #[test]
    fn tool_surface_sorts_each_stage() {
        let s = ToolSurface::new(
            vec!["z".into(), "a".into()],
            vec!["m".into(), "b".into()],
            vec!["y".into(), "c".into()],
        );
        assert_eq!(s.requested, vec!["a".to_string(), "z".to_string()]);
        assert_eq!(s.effective, vec!["b".to_string(), "m".to_string()]);
        assert_eq!(s.registered, vec!["c".to_string(), "y".to_string()]);
    }

    #[test]
    fn completion_or_default_is_inert_for_an_errored_run() {
        let record = RunRecord::from_provenance(provenance());
        let c = record.completion_or_default();
        assert_eq!(c.llm_calls, 0);
        assert!(c.tools_called.is_empty());
        assert!(c.final_response.is_empty());
    }
}
