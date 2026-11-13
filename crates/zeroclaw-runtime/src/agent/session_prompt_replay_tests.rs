//! Private result provenance must survive the Agent's typed-history boundary.

use super::*;
use crate::agent::prompt::{
    SESSION_PROMPT_HISTORY_RESULT_KEY, redact_session_prompt_history_for_export,
};

#[test]
fn session_prompt_result_replay_preserves_private_and_ordinary_content() {
    let private_body = "synthetic-private-session-reminder";
    let ordinary_body = r#"business data: {"session_prompt_tool_result":true}"#;
    for array_envelope in [false, true] {
        let assistant = ChatMessage::assistant(
            serde_json::json!({
                "tool_calls": [
                    {"id": "rewritten", "name": "ordinary_tool", "arguments": "{}"},
                    {"id": "ordinary", "name": "business_tool", "arguments": "{}"},
                ],
            })
            .to_string(),
        );
        // A before-tool hook may execute a prompt tool under an ordinary
        // provider-selected name. Only the host result marker proves privacy.
        let private = serde_json::json!({
            "tool_call_id": "rewritten",
            "content": private_body,
            "attachments": [],
            SESSION_PROMPT_HISTORY_RESULT_KEY: true,
        });
        let ordinary = serde_json::json!({
            "tool_call_id": "ordinary",
            "content": ordinary_body,
            "attachments": [],
        });
        let mut history = vec![assistant];
        if array_envelope {
            history.push(ChatMessage::tool(
                serde_json::json!([private, ordinary]).to_string(),
            ));
        } else {
            history.push(ChatMessage::tool(private.to_string()));
            history.push(ChatMessage::tool(ordinary.to_string()));
        }
        let typed = Agent::replay_loop_messages(&history, None);
        let typed: Vec<ConversationMessage> =
            serde_json::from_str(&serde_json::to_string(&typed).unwrap()).unwrap();
        let ConversationMessage::ToolResults(results) = &typed[1] else {
            panic!("native results must remain typed results");
        };
        assert_eq!(results[0].tool_name, "ordinary_tool");
        assert_eq!(results[0].content, private_body);
        assert!(results[0].session_prompt_tool_result);
        assert_eq!(results[1].tool_name, "business_tool");
        assert_eq!(results[1].content, ordinary_body);
        assert!(!results[1].session_prompt_tool_result);

        for dispatcher in [
            &NativeToolDispatcher as &dyn ToolDispatcher,
            &XmlToolDispatcher as &dyn ToolDispatcher,
        ] {
            let request = dispatcher.to_provider_messages(&typed);
            assert!(request.iter().any(|msg| msg.content.contains(private_body)));
            let exported = redact_session_prompt_history_for_export(&request, false);
            assert!(
                exported
                    .iter()
                    .all(|msg| !msg.content.contains(private_body)),
                "replayed host-marked results stay private even with exposure disabled"
            );
            assert!(request.iter().any(|msg| msg.content.contains(private_body)));
            let ordinary_only = dispatcher.to_provider_messages(&[
                ConversationMessage::ToolResults(vec![results[1].clone()]),
            ]);
            let exported = redact_session_prompt_history_for_export(&ordinary_only, false);
            assert_eq!(exported[0].content, ordinary_only[0].content);
            if exported[0].role == "tool" {
                let envelope: serde_json::Value =
                    serde_json::from_str(&exported[0].content).unwrap();
                assert_eq!(envelope["content"].as_str(), Some(ordinary_body));
            } else {
                assert!(exported[0].content.contains(ordinary_body));
            }
        }
    }
}

type CapturedInputs = Arc<parking_lot::Mutex<Vec<Vec<ChatMessage>>>>;

#[test]
fn session_prompt_result_format_and_replay_use_executed_result_identity() {
    use crate::agent::dispatcher::ToolExecutionResult;

    for name in ["session_prompt_list", "ordinary_tool"] {
        for dispatcher in [
            &NativeToolDispatcher as &dyn ToolDispatcher,
            &XmlToolDispatcher as &dyn ToolDispatcher,
        ] {
            let result = dispatcher.format_results(&[ToolExecutionResult {
                name: name.into(),
                output: "synthetic-result-control".into(),
                success: true,
                tool_call_id: Some("result-control".into()),
            }]);
            let request = dispatcher.to_provider_messages(&[result]);
            assert!(request[0].content.contains("synthetic-result-control"));
            let exported = redact_session_prompt_history_for_export(&request, false);
            if name == "session_prompt_list" {
                assert!(!exported[0].content.contains("synthetic-result-control"));
            } else {
                assert_eq!(exported[0].content, request[0].content);
            }
        }
    }
}

struct OrdinaryReplayTool;
zeroclaw_api::mock_tool_attribution!(OrdinaryReplayTool);

#[async_trait::async_trait]
impl Tool for OrdinaryReplayTool {
    fn name(&self) -> &str {
        "ordinary_tool"
    }

    fn description(&self) -> &str {
        "Ordinary replay fixture"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<crate::tools::ToolResult> {
        Ok(crate::tools::ToolResult {
            success: true,
            output: "ordinary-output".into(),
            error: None,
        })
    }
}

struct ReplayProvider(CapturedInputs);

impl zeroclaw_api::attribution::Attributable for ReplayProvider {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        zeroclaw_api::attribution::Role::Provider(zeroclaw_api::attribution::ProviderKind::Model(
            zeroclaw_api::attribution::ModelProviderKind::Custom,
        ))
    }

    fn alias(&self) -> &str {
        "privacy-replay-provider"
    }
}

#[async_trait::async_trait]
impl ModelProvider for ReplayProvider {
    async fn chat_with_system(
        &self,
        _system: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        Ok("done".into())
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<zeroclaw_providers::ChatResponse> {
        self.0.lock().push(request.messages.to_vec());
        Ok(zeroclaw_providers::ChatResponse {
            text: Some("done".into()),
            tool_calls: Vec::new(),
            usage: None,
            reasoning_content: None,
        })
    }

    fn supports_native_tools(&self) -> bool {
        true
    }
}

struct ReplayInputHook {
    inputs: CapturedInputs,
    modifying_inputs: CapturedInputs,
}

#[async_trait::async_trait]
impl crate::hooks::HookHandler for ReplayInputHook {
    fn name(&self) -> &str {
        "privacy-replay-input"
    }

    async fn on_llm_input(&self, messages: &[ChatMessage], _model: &str) {
        self.inputs.lock().push(messages.to_vec());
    }

    async fn before_llm_call(
        &self,
        messages: &mut Vec<ChatMessage>,
        _model: &mut String,
    ) -> crate::hooks::HookResult<()> {
        self.modifying_inputs.lock().push(messages.clone());
        crate::hooks::HookResult::Continue(())
    }
}

#[tokio::test]
async fn later_agent_turns_keep_replayed_private_results_out_of_input_hooks() {
    let private_body = "synthetic-private-result-on-later-turn";
    for keep_turns in [0, 1] {
        let workspace = tempfile::tempdir().unwrap();
        let requests = CapturedInputs::default();
        let inputs = CapturedInputs::default();
        let modifying_inputs = CapturedInputs::default();
        let mut hooks = crate::hooks::HookRunner::new();
        hooks.register(Box::new(ReplayInputHook {
            inputs: inputs.clone(),
            modifying_inputs: modifying_inputs.clone(),
        }));
        let mut agent = Agent::builder()
            .model_provider(Box::new(ReplayProvider(requests.clone())))
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(OrdinaryReplayTool)],
            ))
            .memory(Arc::new(zeroclaw_memory::NoneMemory::new("privacy-replay")))
            .observer(Arc::new(crate::observability::NoopObserver {}))
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .hook_runner(Some(Arc::new(hooks)))
            .workspace_dir(workspace.path().to_path_buf())
            .auto_save(false)
            .build()
            .unwrap();
        agent.config.resolved.keep_tool_context_turns = keep_turns;
        let previous_turn = vec![
            ChatMessage::user("previous request"),
            ChatMessage::assistant(
                serde_json::json!({
                    "tool_calls": [{
                        "id": "rewritten", "name": "ordinary_tool", "arguments": "{}"
                    }],
                })
                .to_string(),
            ),
            ChatMessage::tool(
                serde_json::json!({
                    "tool_call_id": "rewritten", "content": private_body,
                    "attachments": [], SESSION_PROMPT_HISTORY_RESULT_KEY: true,
                })
                .to_string(),
            ),
            ChatMessage::assistant("previous completion"),
        ];
        agent.seed_conversation_history(Agent::replay_loop_messages(&previous_turn, None));
        for _ in 0..2 {
            assert_eq!(agent.turn("continue").await.unwrap(), "done");
        }

        let requests = requests.lock().clone();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0]
                .iter()
                .any(|msg| msg.content.contains(private_body)),
            keep_turns == 1,
            "zero collapses prior tool turns; one retains the latest prior turn"
        );
        assert!(
            requests[1]
                .iter()
                .all(|msg| !msg.content.contains(private_body))
        );
        for captured in [&inputs, &modifying_inputs] {
            assert!(
                captured
                    .lock()
                    .iter()
                    .flatten()
                    .all(|msg| { !msg.content.contains(private_body) })
            );
        }
        assert_eq!(inputs.lock().len(), 2);
        assert!(agent.history.iter().any(|message| matches!(
            message,
            ConversationMessage::ToolResults(results)
                if results.iter().any(|result| result.content == private_body
                    && result.session_prompt_tool_result)
        )));
    }
}
