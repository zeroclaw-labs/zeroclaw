//! Pre-turn tool-elicitation prefilter: a deterministic scan of the
//! latest user message against activated tools' `invocation_triggers()`, and
//! the one-line ephemeral hint injected on a hit. Gated on the per-agent
//! `tool_elicitation` runtime-profile flag (default off) and on
//! `TurnOrigin::Channel` in v1. The model stays the decision-maker: the hint
//! nudges, it never forces a call, and the prefilter never executes a tool.

use crate::tools::Tool;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// Marker prefix identifying an injected elicitation hint to the model.
/// Informational only — idempotence is tracked in runtime-owned state (see
/// [`hinted_tool_for`]), never by scanning content: user text can contain
/// this literal, and a content guard would let it suppress a real hint and
/// contaminate the hit-rate telemetry.
pub(crate) const HINT_PREFIX: &str = "[tool-hint]";

/// Runtime-owned per-turn hint state: which tool was hinted, and whether
/// the invocation-correlation event has already fired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HintRecord {
    pub tool: String,
    pub call_recorded: bool,
}

/// Runtime-owned records of in-flight hinted turns, keyed by turn id. A
/// model-switch retry re-enters the engine with the same turn id and the
/// same (already-mutated) history; the record is what tells the re-entry
/// that the hint is already present and whether its call event already
/// fired. Entries live exactly as long as their turn: [`HintTurnGuard`]
/// removes them on every exit except the model-switch handoff.
fn hinted_turns() -> &'static Mutex<HashMap<String, HintRecord>> {
    static HINTED_TURNS: OnceLock<Mutex<HashMap<String, HintRecord>>> = OnceLock::new();
    HINTED_TURNS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn hinted_turns_lock() -> std::sync::MutexGuard<'static, HashMap<String, HintRecord>> {
    match hinted_turns().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// This turn's hint state, if the runtime injected a hint on a prior entry
/// (model-switch retry path).
pub(crate) fn hint_record_for(turn_id: &str) -> Option<HintRecord> {
    hinted_turns_lock().get(turn_id).cloned()
}

/// Record that the runtime injected a hint for `tool_name` on this turn.
pub(crate) fn record_hint(turn_id: &str, tool_name: &str) {
    hinted_turns_lock().insert(
        turn_id.to_string(),
        HintRecord {
            tool: tool_name.to_string(),
            call_recorded: false,
        },
    );
}

/// Record that this turn's hinted tool was called and the correlation
/// event fired, so a model-switch retry does not fire it again.
pub(crate) fn record_hint_call(turn_id: &str) {
    if let Some(record) = hinted_turns_lock().get_mut(turn_id) {
        record.call_recorded = true;
    }
}

/// Clears a turn's hint record on drop unless defused. Defused only on the
/// model-switch handoff, where the same turn re-enters the engine and must
/// still see the record; every other exit (completion, error, panic) ends
/// the turn and the record with it.
pub(crate) struct HintTurnGuard {
    turn_id: String,
    armed: bool,
}

impl HintTurnGuard {
    pub(crate) fn new(turn_id: &str) -> Self {
        Self {
            turn_id: turn_id.to_string(),
            armed: true,
        }
    }

    pub(crate) fn defuse(&mut self) {
        self.armed = false;
    }
}

impl Drop for HintTurnGuard {
    fn drop(&mut self) {
        if self.armed {
            hinted_turns_lock().remove(&self.turn_id);
        }
    }
}

/// Caller-owned backstop for the turn's hint record, held by the frame that
/// owns the turn id and any model-switch retries (the entry points and the
/// channel orchestrator). The engine's own guard is defused on the
/// model-switch handoff so the retry still sees the record — but the retry
/// owner can then fail to re-enter the loop at all (provider resolution or
/// construction failure), and without this scope the record would outlive
/// the turn. Dropping the scope removes the record unconditionally; after a
/// normal turn completion the engine has already removed it and the drop is
/// a no-op.
pub struct TurnHintScope {
    turn_id: String,
}

impl TurnHintScope {
    #[must_use]
    pub fn new(turn_id: &str) -> Self {
        Self {
            turn_id: turn_id.to_string(),
        }
    }
}

impl Drop for TurnHintScope {
    fn drop(&mut self) {
        hinted_turns_lock().remove(&self.turn_id);
    }
}

/// The invocation-correlation telemetry event: the hinted tool was actually
/// called this turn. Deliberately invocation-only — the outcome stays
/// [`Unknown`](::zeroclaw_log::EventOutcome::Unknown) regardless of how the
/// tool's own execution went (that is `tool_call_result`'s job), and the
/// attributes carry tool name, iteration, and trace id only, never message
/// text.
pub(crate) fn hinted_call_event(
    tool: &str,
    iteration: usize,
    turn_id: &str,
) -> ::zeroclaw_log::Event {
    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Complete)
        .with_category(::zeroclaw_log::EventCategory::Tool)
        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
        .with_attrs(::serde_json::json!({
            "tool": tool,
            "iteration": iteration,
            "trace_id": turn_id,
        }))
}

/// The ephemeral hint appended to the latest user message's content (the
/// same rides-the-user-turn idiom as memory context — a separate system
/// message gets hoisted to context start by provider-side normalization).
/// Generic template — names the matched tool, leaves the decision to the
/// model. Never persisted: the channel path stores the user turn before the
/// per-turn message Vec is built.
pub(crate) fn hint_message(tool_name: &str) -> String {
    format!(
        "{HINT_PREFIX} The latest user message matches the `{tool_name}` tool's \
         invocation triggers. If the request calls for it, invoke `{tool_name}` \
         before composing your reply; otherwise ignore this note. Do not mention \
         this note to the user."
    )
}

/// Scan the lowercased latest user message against every non-excluded tool's
/// `invocation_triggers()`, using the trait's word-boundary matching contract
/// ([`zeroclaw_api::tool::invocation_trigger_matches`]). Scans every tool
/// the turn can actually advertise and execute: the static registry plus
/// the activated deferred set — a deferred tool activated on an earlier
/// turn is callable on this one and must be equally eligible for a hint.
/// The longest matching trigger wins so the most specific tool is hinted;
/// ties keep the first tool in registry order, then activated tools in
/// their (name-sorted) snapshot order. An activated tool shadowed by a
/// registry tool of the same name is skipped, mirroring execution's
/// static-first resolution. Returns the winning tool's name.
pub(crate) fn scan_for_trigger_hit(
    message_lower: &str,
    tools: &[Box<dyn Tool>],
    activated: &[Arc<dyn Tool>],
    excluded_tools: &[String],
) -> Option<String> {
    let mut best: Option<(usize, &str)> = None;
    let registry_then_activated =
        tools
            .iter()
            .map(|t| t.as_ref() as &dyn Tool)
            .chain(activated.iter().filter_map(|t| {
                let tool: &dyn Tool = t.as_ref();
                if tools.iter().any(|r| r.name() == tool.name()) {
                    None
                } else {
                    Some(tool)
                }
            }));
    for tool in registry_then_activated {
        let name = tool.name();
        if excluded_tools.iter().any(|e| e == name) {
            continue;
        }
        for trigger in tool.invocation_triggers() {
            if !zeroclaw_api::tool::invocation_trigger_matches(message_lower, &trigger) {
                continue;
            }
            if best.is_none_or(|(len, _)| trigger.len() > len) {
                best = Some((trigger.len(), name));
            }
        }
    }
    best.map(|(_, name)| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use zeroclaw_api::attribution::{Attributable, Role, ToolKind};
    use zeroclaw_api::tool::ToolResult;

    struct TriggerTool {
        name: &'static str,
        triggers: Vec<String>,
        fail: bool,
    }

    impl Attributable for TriggerTool {
        fn role(&self) -> Role {
            Role::Tool(ToolKind::Plugin)
        }
        fn alias(&self) -> &str {
            self.name
        }
    }

    #[async_trait]
    impl Tool for TriggerTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "test tool"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        fn invocation_triggers(&self) -> Vec<String> {
            self.triggers.clone()
        }
        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            if self.fail {
                return Ok(ToolResult {
                    success: false,
                    output: "".into(),
                    error: Some("synthetic tool failure".to_string()),
                });
            }
            Ok(ToolResult::ok("ok"))
        }
    }

    pub(super) fn tool(name: &'static str, triggers: &[&str]) -> Box<dyn Tool> {
        Box::new(TriggerTool {
            name,
            triggers: triggers.iter().map(|t| (*t).to_string()).collect(),
            fail: false,
        })
    }

    pub(super) fn failing_tool(name: &'static str, triggers: &[&str]) -> Box<dyn Tool> {
        Box::new(TriggerTool {
            name,
            triggers: triggers.iter().map(|t| (*t).to_string()).collect(),
            fail: true,
        })
    }

    #[test]
    fn no_triggers_no_hit() {
        let tools = vec![tool("plain", &[])];
        assert_eq!(
            scan_for_trigger_hit("send this to my email", &tools, &[], &[]),
            None
        );
    }

    #[test]
    fn substring_match_hits() {
        let tools = vec![tool("send_via", &["send this to", "via voice"])];
        assert_eq!(
            scan_for_trigger_hit("please send this to marta", &tools, &[], &[]),
            Some("send_via".to_string())
        );
        assert_eq!(
            scan_for_trigger_hit("what's the weather", &tools, &[], &[]),
            None
        );
    }

    #[test]
    fn longest_trigger_wins_across_tools() {
        let tools = vec![
            tool("generic", &["send"]),
            tool("send_via", &["send this to my email"]),
        ];
        assert_eq!(
            scan_for_trigger_hit("send this to my email please", &tools, &[], &[]),
            Some("send_via".to_string())
        );
    }

    #[test]
    fn excluded_tools_are_skipped() {
        let tools = vec![tool("send_via", &["via voice"])];
        assert_eq!(
            scan_for_trigger_hit("reply via voice", &tools, &[], &["send_via".to_string()]),
            None
        );
    }

    #[test]
    fn word_boundary_prevents_substring_false_positives() {
        // A short dynamic-style trigger must not fire inside a longer word.
        let tools = vec![tool("send_via", &["dev"])];
        assert_eq!(
            scan_for_trigger_hit("check the device status", &tools, &[], &[]),
            None
        );
        assert_eq!(
            scan_for_trigger_hit("route this to dev", &tools, &[], &[]),
            Some("send_via".to_string())
        );
    }

    pub(super) fn activated_tool(name: &'static str, triggers: &[&str]) -> Arc<dyn Tool> {
        Arc::new(TriggerTool {
            name,
            triggers: triggers.iter().map(|t| (*t).to_string()).collect(),
            fail: false,
        })
    }

    #[test]
    fn activated_tools_are_scanned() {
        let tools = vec![tool("plain", &[])];
        let activated = vec![activated_tool("mcp__mail__send", &["send this to"])];
        assert_eq!(
            scan_for_trigger_hit("please send this to marta", &tools, &activated, &[]),
            Some("mcp__mail__send".to_string())
        );
    }

    #[test]
    fn longest_trigger_wins_across_registry_and_activated() {
        let tools = vec![tool("send_via", &["send"])];
        let activated = vec![activated_tool(
            "mcp__mail__send",
            &["send this to my email"],
        )];
        assert_eq!(
            scan_for_trigger_hit("send this to my email please", &tools, &activated, &[]),
            Some("mcp__mail__send".to_string())
        );
        // And the registry side wins when its trigger is the longer one.
        let tools = vec![tool("send_via", &["send this to my email"])];
        let activated = vec![activated_tool("mcp__mail__send", &["send"])];
        assert_eq!(
            scan_for_trigger_hit("send this to my email please", &tools, &activated, &[]),
            Some("send_via".to_string())
        );
    }

    #[test]
    fn excluded_activated_tools_are_skipped() {
        let tools: Vec<Box<dyn Tool>> = Vec::new();
        let activated = vec![activated_tool("mcp__mail__send", &["via voice"])];
        assert_eq!(
            scan_for_trigger_hit(
                "reply via voice",
                &tools,
                &activated,
                &["mcp__mail__send".to_string()]
            ),
            None
        );
    }

    #[test]
    fn activated_tool_shadowed_by_registry_name_is_skipped() {
        // Execution resolves static-first; a same-named activated tool must
        // not add triggers the registry tool does not advertise.
        let tools = vec![tool("send_via", &["send this to"])];
        let activated = vec![activated_tool("send_via", &["totally different trigger"])];
        assert_eq!(
            scan_for_trigger_hit("totally different trigger", &tools, &activated, &[]),
            None
        );
    }

    #[test]
    fn hint_record_round_trip_and_guard() {
        let turn = "test-hint-record-turn";
        assert!(hint_record_for(turn).is_none());
        record_hint(turn, "send_via");
        assert_eq!(
            hint_record_for(turn),
            Some(HintRecord {
                tool: "send_via".to_string(),
                call_recorded: false,
            })
        );
        record_hint_call(turn);
        assert_eq!(
            hint_record_for(turn),
            Some(HintRecord {
                tool: "send_via".to_string(),
                call_recorded: true,
            })
        );

        // A defused guard keeps the record (model-switch handoff)...
        let mut guard = HintTurnGuard::new(turn);
        guard.defuse();
        drop(guard);
        assert!(hint_record_for(turn).is_some());

        // ...an armed one ends it with the turn.
        drop(HintTurnGuard::new(turn));
        assert!(hint_record_for(turn).is_none());
    }

    #[test]
    fn hinted_call_event_is_invocation_only_by_construction() {
        let event = hinted_call_event("send_via", 2, "trace-1");
        assert_eq!(event.outcome, ::zeroclaw_log::EventOutcome::Unknown);
        let attrs = event.attrs.expect("event carries attributes");
        assert_eq!(attrs["tool"], "send_via");
        assert_eq!(attrs["iteration"], 2);
        assert_eq!(attrs["trace_id"], "trace-1");
        assert_eq!(
            attrs.as_object().map(serde_json::Map::len),
            Some(3),
            "tool name, iteration, and trace id only — never message text"
        );
    }

    #[test]
    fn turn_hint_scope_backstops_abandoned_records() {
        let turn = "test-hint-scope-turn";
        let scope = TurnHintScope::new(turn);
        record_hint(turn, "send_via");
        // The engine's defused guard left the record behind (switch
        // handoff); the owner's scope must reclaim it.
        drop(scope);
        assert!(hint_record_for(turn).is_none());
    }

    #[test]
    fn hint_message_names_tool_and_carries_marker() {
        let msg = hint_message("send_via");
        assert!(msg.starts_with(HINT_PREFIX));
        assert!(msg.contains("`send_via`"));
    }
}

/// Behavior tests for the flag/origin gate and injection in
/// `run_tool_call_loop` — the engine block this module backs.
#[cfg(test)]
mod loop_gate_tests {
    use super::tests::{failing_tool, tool};
    use super::*;
    use crate::observability;
    use zeroclaw_api::ingress::IngressContext;
    use zeroclaw_api::model_provider::{ChatRequest, ChatResponse};
    use zeroclaw_providers::{ChatMessage, ModelProvider};

    struct PlainProvider;

    #[async_trait::async_trait]
    impl ModelProvider for PlainProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("done".to_string())
        }
        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            Ok(ChatResponse {
                text: Some("done".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }
    }

    impl zeroclaw_api::attribution::Attributable for PlainProvider {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::Provider(
                zeroclaw_api::attribution::ProviderKind::Model(
                    zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "plain-provider"
        }
    }

    fn elicitation_config() -> zeroclaw_config::schema::Config {
        toml::from_str(
            r#"
[runtime_profiles.hinted]
tool_elicitation = true

[agents.default]
runtime_profile = "hinted"
"#,
        )
        .expect("test config parses")
    }

    struct RunSpec<'a> {
        config: Option<&'a zeroclaw_config::schema::Config>,
        ingress: IngressContext,
        tools_registry: &'a [Box<dyn Tool>],
        excluded_tools: &'a [String],
        activated_tools: Option<&'a Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>>,
        provider: &'a dyn zeroclaw_providers::ModelProvider,
        turn_id: &'a str,
        /// Prefilled model-switch state: makes the loop exit with
        /// `ModelSwitchRequested` on its first iteration, emulating the
        /// switch half of a retry.
        model_switch_to: Option<(&'a str, &'a str)>,
    }

    async fn run_spec(spec: RunSpec<'_>, history: &mut Vec<ChatMessage>) -> anyhow::Result<String> {
        let model_switch_callback = spec.model_switch_to.map(|(provider, model)| {
            Arc::new(std::sync::Mutex::new(Some((
                provider.to_string(),
                model.to_string(),
            ))))
        });
        crate::agent::loop_::run_tool_call_loop(crate::agent::loop_::ToolLoop {
            parent_agent_alias: None,
            sop_reassembly: None,
            exec: crate::agent::loop_::ResolvedAgentExecution {
                model_access: crate::agent::loop_::ResolvedModelAccess {
                    model_provider: spec.provider,
                    provider_name: "mock",
                    model: "mock-model",
                    temperature: None,
                },
                tools_registry: spec.tools_registry,
                observer: &observability::NoopObserver {},
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                config: spec.config,
                max_tool_iterations: 3,
                hooks: None,
                excluded_tools: spec.excluded_tools,
                dedup_exempt_tools: &[],
                activated_tools: spec.activated_tools,
                model_switch_callback,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 30_000,
                context_token_budget: 100_000,
                receipt_generator: None,
                knobs: &crate::agent::loop_::LoopKnobs::default(),
            },
            history,
            channel_name: "test-channel",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            memory: None,
            ingress: spec.ingress,
            agent_alias: Some("default"),
            turn_id: spec.turn_id,
        })
        .await
    }

    async fn run_once(
        config: Option<&zeroclaw_config::schema::Config>,
        ingress: IngressContext,
        history: &mut Vec<ChatMessage>,
        tools_registry: &[Box<dyn Tool>],
    ) {
        let provider = PlainProvider;
        let turn_id = uuid::Uuid::new_v4().to_string();
        run_spec(
            RunSpec {
                config,
                ingress,
                tools_registry,
                excluded_tools: &[],
                activated_tools: None,
                provider: &provider,
                turn_id: &turn_id,
                model_switch_to: None,
            },
            history,
        )
        .await
        .expect("loop should succeed");
    }

    /// Total occurrences of the hint marker anywhere in the history.
    fn hint_count(history: &[ChatMessage]) -> usize {
        history
            .iter()
            .map(|m| m.content.matches(HINT_PREFIX).count())
            .sum()
    }

    #[tokio::test]
    async fn channel_turn_with_flag_on_appends_hint_to_user_message() {
        let cfg = elicitation_config();
        let tools: Vec<Box<dyn Tool>> = vec![tool("send_via", &["send this to"])];
        let mut history = vec![ChatMessage::user("please send this to marta")];
        run_once(Some(&cfg), IngressContext::channel(), &mut history, &tools).await;

        assert_eq!(hint_count(&history), 1);
        let user = history
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .expect("user message survives the turn");
        assert!(
            user.content.starts_with("please send this to marta"),
            "original user text must stay first"
        );
        assert!(
            user.content.contains(HINT_PREFIX) && user.content.contains("`send_via`"),
            "hint must ride the user message content; history: {:?}",
            history
                .iter()
                .map(|m| (m.role.as_str(), &m.content[..m.content.len().min(40)]))
                .collect::<Vec<_>>()
        );
        assert!(
            !history.iter().any(|m| m.role == "system"),
            "the hint must not be a separate system message"
        );
    }

    #[tokio::test]
    async fn flag_off_never_injects() {
        // No config at all (fail closed) and a config without the profile flag.
        let tools: Vec<Box<dyn Tool>> = vec![tool("send_via", &["send this to"])];
        let mut history = vec![ChatMessage::user("please send this to marta")];
        run_once(None, IngressContext::channel(), &mut history, &tools).await;
        assert_eq!(hint_count(&history), 0);

        let cfg: zeroclaw_config::schema::Config =
            toml::from_str("[agents.default]\nruntime_profile = \"default\"\n").unwrap();
        let mut history = vec![ChatMessage::user("please send this to marta")];
        run_once(Some(&cfg), IngressContext::channel(), &mut history, &tools).await;
        assert_eq!(hint_count(&history), 0);
    }

    #[tokio::test]
    async fn non_channel_origin_never_injects() {
        let cfg = elicitation_config();
        let tools: Vec<Box<dyn Tool>> = vec![tool("send_via", &["send this to"])];
        for ingress in [
            IngressContext::sub_turn(),
            IngressContext::cron(),
            IngressContext::interactive(),
        ] {
            let mut history = vec![ChatMessage::user("please send this to marta")];
            run_once(Some(&cfg), ingress, &mut history, &tools).await;
            assert_eq!(hint_count(&history), 0);
        }
    }

    #[tokio::test]
    async fn no_trigger_match_never_injects() {
        let cfg = elicitation_config();
        let tools: Vec<Box<dyn Tool>> = vec![tool("send_via", &["send this to"])];
        let mut history = vec![ChatMessage::user("what's the weather like")];
        run_once(Some(&cfg), IngressContext::channel(), &mut history, &tools).await;
        assert_eq!(hint_count(&history), 0);
    }

    #[tokio::test]
    async fn user_authored_marker_does_not_suppress_the_hint() {
        // Idempotence is runtime-owned, not content-derived: a user message
        // that already contains the hint marker (even a verbatim hint) must
        // still receive the real injection, so untrusted content can neither
        // suppress the feature nor arm the call telemetry without one.
        let cfg = elicitation_config();
        let tools: Vec<Box<dyn Tool>> = vec![tool("send_via", &["send this to"])];
        let mut history = vec![ChatMessage::user(format!(
            "please send this to marta\n\n{}",
            hint_message("send_via")
        ))];
        run_once(Some(&cfg), IngressContext::channel(), &mut history, &tools).await;
        // Forged marker + the real injected hint.
        assert_eq!(hint_count(&history), 2);
        let user = history
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .expect("user message survives the turn");
        assert!(
            user.content.ends_with(&hint_message("send_via")),
            "the runtime's own hint must still be appended"
        );
    }

    #[tokio::test]
    async fn model_switch_retry_does_not_stack_hint_or_events() {
        // A model-switch retry re-enters the engine with the same turn id
        // and the already-hinted history; the note must not duplicate. The
        // guard is the runtime-owned per-turn record, defused across the
        // switch handoff and cleared when the turn completes.
        let cfg = elicitation_config();
        let tools: Vec<Box<dyn Tool>> = vec![tool("send_via", &["send this to"])];
        let mut history = vec![ChatMessage::user("please send this to marta")];
        let turn_id = format!("switch-retry-{}", uuid::Uuid::new_v4());
        let provider = PlainProvider;

        // First entry: hint injected, then the loop hands off for a switch.
        let err = run_spec(
            RunSpec {
                config: Some(&cfg),
                ingress: IngressContext::channel(),
                tools_registry: &tools,
                excluded_tools: &[],
                activated_tools: None,
                provider: &provider,
                turn_id: &turn_id,
                model_switch_to: Some(("other", "other-model")),
            },
            &mut history,
        )
        .await
        .expect_err("prefilled switch state must exit the loop");
        assert!(
            crate::agent::loop_::is_model_switch_requested(&err).is_some(),
            "loop must exit via ModelSwitchRequested, got: {err:?}"
        );
        assert_eq!(hint_count(&history), 1);
        assert!(
            hint_record_for(&turn_id).is_some(),
            "switch handoff must keep the turn's hint record"
        );

        // Retry: same turn id, same mutated history — no second hint.
        run_spec(
            RunSpec {
                config: Some(&cfg),
                ingress: IngressContext::channel(),
                tools_registry: &tools,
                excluded_tools: &[],
                activated_tools: None,
                provider: &provider,
                turn_id: &turn_id,
                model_switch_to: None,
            },
            &mut history,
        )
        .await
        .expect("retry completes");
        assert_eq!(hint_count(&history), 1);
        assert!(
            hint_record_for(&turn_id).is_none(),
            "completing the turn must clear its hint record"
        );
    }

    #[tokio::test]
    async fn abandoned_switch_handoff_does_not_leak_hint_record() {
        // The engine keeps the record alive across a model-switch handoff,
        // but the retry owner can fail provider resolution or construction
        // and exit without ever re-entering the loop. The owner's
        // TurnHintScope must reclaim the record on that path.
        let cfg = elicitation_config();
        let tools: Vec<Box<dyn Tool>> = vec![tool("send_via", &["send this to"])];
        let mut history = vec![ChatMessage::user("please send this to marta")];
        let turn_id = format!("abandoned-handoff-{}", uuid::Uuid::new_v4());
        {
            // What every production retry owner holds for the turn's lifetime.
            let _scope = crate::agent::loop_::TurnHintScope::new(&turn_id);
            let provider = PlainProvider;
            let err = run_spec(
                RunSpec {
                    config: Some(&cfg),
                    ingress: IngressContext::channel(),
                    tools_registry: &tools,
                    excluded_tools: &[],
                    activated_tools: None,
                    provider: &provider,
                    turn_id: &turn_id,
                    model_switch_to: Some(("other", "other-model")),
                },
                &mut history,
            )
            .await
            .expect_err("prefilled switch state must exit the loop");
            assert!(crate::agent::loop_::is_model_switch_requested(&err).is_some());
            assert!(
                hint_record_for(&turn_id).is_some(),
                "handoff must keep the record for a would-be retry"
            );
            // The owner now fails to build the new provider and exits
            // without re-entering the loop: the scope drops here.
        }
        assert!(
            hint_record_for(&turn_id).is_none(),
            "an abandoned handoff must not leak the turn's hint record"
        );
    }

    #[tokio::test]
    async fn activated_deferred_tool_is_hinted_and_excludable() {
        // A deferred tool activated on an earlier turn is advertised and
        // executable on this one; elicitation must scan it too, under the
        // same exclusion rules.
        let cfg = elicitation_config();
        let tools: Vec<Box<dyn Tool>> = vec![tool("plain", &[])];
        let mut set = crate::tools::ActivatedToolSet::new();
        set.activate(
            "mcp__mail__send".to_string(),
            super::tests::activated_tool("mcp__mail__send", &["send this to"]),
        );
        let activated = Arc::new(std::sync::Mutex::new(set));
        let provider = PlainProvider;

        let mut history = vec![ChatMessage::user("please send this to marta")];
        let turn_id = uuid::Uuid::new_v4().to_string();
        run_spec(
            RunSpec {
                config: Some(&cfg),
                ingress: IngressContext::channel(),
                tools_registry: &tools,
                excluded_tools: &[],
                activated_tools: Some(&activated),
                provider: &provider,
                turn_id: &turn_id,
                model_switch_to: None,
            },
            &mut history,
        )
        .await
        .expect("loop should succeed");
        assert_eq!(hint_count(&history), 1);
        assert!(
            history
                .iter()
                .rev()
                .find(|m| m.role == "user")
                .expect("user message survives")
                .content
                .contains("`mcp__mail__send`"),
            "the activated tool must be hinted"
        );

        // The same exclusion list that gates advertisement and execution
        // gates elicitation.
        let mut history = vec![ChatMessage::user("please send this to marta")];
        let turn_id = uuid::Uuid::new_v4().to_string();
        run_spec(
            RunSpec {
                config: Some(&cfg),
                ingress: IngressContext::channel(),
                tools_registry: &tools,
                excluded_tools: &["mcp__mail__send".to_string()],
                activated_tools: Some(&activated),
                provider: &provider,
                turn_id: &turn_id,
                model_switch_to: None,
            },
            &mut history,
        )
        .await
        .expect("loop should succeed");
        assert_eq!(hint_count(&history), 0);
    }

    /// Provider that requests one call of `tool` on its first iteration,
    /// then answers with plain text.
    struct CallOnceProvider {
        tool: &'static str,
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl ModelProvider for CallOnceProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("done".to_string())
        }
        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            let first = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
            Ok(ChatResponse {
                text: (!first).then(|| "done".to_string()),
                tool_calls: if first {
                    vec![zeroclaw_api::model_provider::ToolCall {
                        id: "call-1".to_string(),
                        name: self.tool.to_string(),
                        arguments: "{}".to_string(),
                        extra_content: None,
                    }]
                } else {
                    Vec::new()
                },
                usage: None,
                reasoning_content: None,
            })
        }
    }

    impl zeroclaw_api::attribution::Attributable for CallOnceProvider {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::Provider(
                zeroclaw_api::attribution::ProviderKind::Model(
                    zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "call-once-provider"
        }
    }

    async fn run_hinted_call(tools: Vec<Box<dyn Tool>>) -> usize {
        // Hold the process-global hook lock for the complete
        // subscribe → turn → sentinel → collection window, so no parallel
        // test can clear or replace the broadcast hook and detach this
        // receiver from the sender the turn's events go to — the same
        // discipline every other broadcast-capture test in the repository
        // follows.
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();

        let turn_id = format!("hinted-call-{}", uuid::Uuid::new_v4());
        let sentinel = format!("elicitation_test_sentinel_{turn_id}");

        // Collector runs CONCURRENTLY with the turn so the shared broadcast
        // receiver never falls a buffer's length behind under parallel test
        // load, and stops on a sentinel record emitted after the turn — the
        // channel is a single ordered stream, so once the sentinel arrives
        // every event the turn emitted has been observed. No timing guesses.
        let collector_turn_id = turn_id.clone();
        let collector_sentinel = sentinel.clone();
        let collector = zeroclaw_spawn::spawn!(async move {
            let mut seen = 0usize;
            loop {
                match rx.recv().await {
                    Ok(value) => {
                        let message = value.get("message").and_then(|v| v.as_str()).unwrap_or("");
                        if message.contains(&collector_sentinel) {
                            break;
                        }
                        let ours = value
                            .get("attributes")
                            .and_then(|a| a.get("trace_id"))
                            .and_then(|v| v.as_str())
                            == Some(collector_turn_id.as_str());
                        if ours && message.contains("tool_called_after_hint") {
                            seen += 1;
                            let outcome = value
                                .get("event")
                                .and_then(|e| e.get("outcome"))
                                .and_then(|v| v.as_str());
                            assert_ne!(
                                outcome,
                                Some("success"),
                                "invocation-only event must not claim tool success: {value}"
                            );
                        }
                    }
                    // Any capture loss is a test failure, never something to
                    // continue past: a lagged or prematurely closed receiver
                    // could swallow the hinted event or the sentinel, and the
                    // exactly-once assertions must not pass on lost evidence.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        panic!("broadcast capture lost {n} events — evidence incomplete");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        panic!("broadcast closed before the post-turn sentinel");
                    }
                }
            }
            seen
        });

        let cfg = elicitation_config();
        let provider = CallOnceProvider {
            tool: "send_via",
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let mut history = vec![ChatMessage::user("please send this to marta")];
        run_spec(
            RunSpec {
                config: Some(&cfg),
                ingress: IngressContext::channel(),
                tools_registry: &tools,
                excluded_tools: &[],
                activated_tools: None,
                provider: &provider,
                turn_id: &turn_id,
                model_switch_to: None,
            },
            &mut history,
        )
        .await
        .expect("loop should succeed");

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            &sentinel
        );
        tokio::time::timeout(std::time::Duration::from_secs(10), collector)
            .await
            .expect("collector must observe the post-turn sentinel")
            .expect("collector task must not panic")
    }

    #[tokio::test]
    async fn hinted_call_event_is_invocation_only_for_success_and_failure() {
        // The correlation event fires once whether the hinted tool's own
        // execution succeeds or fails, and never claims tool success —
        // execution results belong to `tool_call_result`.
        let succeeded = run_hinted_call(vec![tool("send_via", &["send this to"])]).await;
        assert_eq!(succeeded, 1, "successful hinted call must emit one event");

        let failed = run_hinted_call(vec![failing_tool("send_via", &["send this to"])]).await;
        assert_eq!(failed, 1, "failed hinted call must still emit one event");
    }
}
