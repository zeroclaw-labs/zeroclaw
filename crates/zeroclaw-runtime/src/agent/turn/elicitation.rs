//! Pre-turn tool-elicitation prefilter: a deterministic scan of the
//! latest user message against activated tools' `invocation_triggers()`, and
//! the one-line ephemeral hint injected on a hit. Gated on the per-agent
//! `tool_elicitation` runtime-profile flag (default off) and on
//! `TurnOrigin::Channel` in v1. The model stays the decision-maker: the hint
//! nudges, it never forces a call, and the prefilter never executes a tool.

use crate::tools::Tool;

/// Marker prefix identifying an injected elicitation hint. Doubles as the
/// idempotence guard: a model-switch retry re-enters the engine with the same
/// history, and the hint must not stack.
pub(crate) const HINT_PREFIX: &str = "[tool-hint]";

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
/// ([`zeroclaw_api::tool::invocation_trigger_matches`]). The longest matching
/// trigger wins so the most specific tool is hinted; ties keep the first tool
/// in registry order. Returns the winning tool's name.
pub(crate) fn scan_for_trigger_hit(
    message_lower: &str,
    tools: &[Box<dyn Tool>],
    excluded_tools: &[String],
) -> Option<String> {
    let mut best: Option<(usize, &str)> = None;
    for tool in tools {
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
            Ok(ToolResult::ok("ok"))
        }
    }

    pub(super) fn tool(name: &'static str, triggers: &[&str]) -> Box<dyn Tool> {
        Box::new(TriggerTool {
            name,
            triggers: triggers.iter().map(|t| (*t).to_string()).collect(),
        })
    }

    #[test]
    fn no_triggers_no_hit() {
        let tools = vec![tool("plain", &[])];
        assert_eq!(
            scan_for_trigger_hit("send this to my email", &tools, &[]),
            None
        );
    }

    #[test]
    fn substring_match_hits() {
        let tools = vec![tool("send_via", &["send this to", "via voice"])];
        assert_eq!(
            scan_for_trigger_hit("please send this to marta", &tools, &[]),
            Some("send_via".to_string())
        );
        assert_eq!(
            scan_for_trigger_hit("what's the weather", &tools, &[]),
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
            scan_for_trigger_hit("send this to my email please", &tools, &[]),
            Some("send_via".to_string())
        );
    }

    #[test]
    fn excluded_tools_are_skipped() {
        let tools = vec![tool("send_via", &["via voice"])];
        assert_eq!(
            scan_for_trigger_hit("reply via voice", &tools, &["send_via".to_string()]),
            None
        );
    }

    #[test]
    fn word_boundary_prevents_substring_false_positives() {
        // A short dynamic-style trigger must not fire inside a longer word.
        let tools = vec![tool("send_via", &["dev"])];
        assert_eq!(
            scan_for_trigger_hit("check the device status", &tools, &[]),
            None
        );
        assert_eq!(
            scan_for_trigger_hit("route this to dev", &tools, &[]),
            Some("send_via".to_string())
        );
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
    use super::tests::tool;
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

    async fn run_once(
        config: Option<&zeroclaw_config::schema::Config>,
        ingress: IngressContext,
        history: &mut Vec<ChatMessage>,
        tools_registry: &[Box<dyn Tool>],
    ) {
        let provider = PlainProvider;
        let turn_id = uuid::Uuid::new_v4().to_string();
        crate::agent::loop_::run_tool_call_loop(crate::agent::loop_::ToolLoop {
            parent_agent_alias: None,
            sop_reassembly: None,
            exec: crate::agent::loop_::ResolvedAgentExecution {
                model_access: crate::agent::loop_::ResolvedModelAccess {
                    model_provider: &provider,
                    provider_name: "mock",
                    model: "mock-model",
                    temperature: None,
                },
                tools_registry,
                observer: &observability::NoopObserver {},
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                config,
                max_tool_iterations: 3,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
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
            ingress,
            agent_alias: Some("default"),
            turn_id: &turn_id,
        })
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
    async fn existing_hint_does_not_stack() {
        // A model-switch retry re-enters the engine with the already-hinted
        // history; the note must not duplicate.
        let cfg = elicitation_config();
        let tools: Vec<Box<dyn Tool>> = vec![tool("send_via", &["send this to"])];
        let mut history = vec![ChatMessage::user(format!(
            "please send this to marta\n\n{}",
            hint_message("send_via")
        ))];
        run_once(Some(&cfg), IngressContext::channel(), &mut history, &tools).await;
        assert_eq!(hint_count(&history), 1);
    }
}
