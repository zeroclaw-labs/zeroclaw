use super::*;

const FOREIGN: &str = "private-steering-history-marker";
const APPEND: &str = "steering-must-not-append-this";
const TOOLS: &[&str] = &[
    "sessions_list",
    "sessions_history",
    "sessions_send",
    "sessions_current",
    "calculator",
    "delegate",
    "peek__history",
    "peek__send",
];

struct SessionToolsAfterSteering {
    first: RecordingTurnProvider,
    calls: std::sync::atomic::AtomicUsize,
    results: Arc<std::sync::Mutex<Vec<String>>>,
}

impl zeroclaw_api::attribution::Attributable for SessionToolsAfterSteering {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        self.first.role()
    }
    fn alias(&self) -> &str {
        "session-tools-after-steering"
    }
}

#[async_trait]
impl zeroclaw_api::model_provider::ModelProvider for SessionToolsAfterSteering {
    fn supports_native_tools(&self) -> bool {
        true
    }
    async fn chat_with_system(
        &self,
        _: Option<&str>,
        _: &str,
        _: &str,
        _: Option<f64>,
    ) -> anyhow::Result<String> {
        unreachable!("the fixture uses native tool requests")
    }
    async fn chat(
        &self,
        request: zeroclaw_providers::ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<zeroclaw_providers::ChatResponse> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if call == 0 {
            return self.first.chat(request, model, temperature).await;
        }
        assert!(
            request
                .messages
                .iter()
                .any(|m| m.content.contains(STEER_TEXT))
        );
        if call > 1 {
            *self.results.lock().unwrap() = request
                .messages
                .iter()
                .filter(|m| m.role == "tool")
                .map(|m| m.content.clone())
                .collect();
        }
        Ok(zeroclaw_providers::ChatResponse {
            text: (call > 1).then(|| "steering probe complete".into()),
            // Ask even when the tool was not offered: execution must refuse it too.
            tool_calls: if call == 1 {
                [
                    ("sessions_history", json!({"session_id":"rpc_victim"})),
                    ("peek__history", json!({"session_id":"rpc_victim"})),
                    (
                        "sessions_send",
                        json!({"session_id":"rpc_victim", "message":APPEND}),
                    ),
                    (
                        "peek__send",
                        json!({"session_id":"rpc_victim", "message":APPEND}),
                    ),
                    ("calculator", json!({"function":"add", "values":[2,2]})),
                    ("sessions_current", json!({})),
                ]
                .into_iter()
                .enumerate()
                .map(|(i, (name, args))| zeroclaw_providers::ToolCall {
                    id: format!("steering-{i}"),
                    name: name.into(),
                    arguments: args.to_string(),
                    extra_content: None,
                })
                .collect()
            } else {
                Vec::new()
            },
            usage: None,
            reasoning_content: None,
        })
    }
}

#[derive(Clone, Copy, Debug)]
enum Demotion {
    Never,
    BeforeConsumption,
    DuringMemoryWrite,
}

async fn session_tools_after_steering(demotion: Demotion, wildcard: bool) {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut config = two_user_config_with_administrator_alice(&tmp);
    let member = config.permission_profiles.get_mut("member").unwrap();
    member.grants.insert(
        zeroclaw_api::grants::Resource::Tools,
        vec![zeroclaw_api::grants::Verb::Execute],
    );
    if !wildcard {
        member.allowed_tools = TOOLS.iter().map(|name| (*name).into()).collect();
    }
    let risk = config.risk_profiles.get_mut("test-profile").unwrap();
    risk.level = zeroclaw_config::autonomy::AutonomyLevel::Full;
    risk.allowed_tools
        .extend(TOOLS.iter().map(|name| (*name).to_owned()));
    let bundle = tmp.path().join("skills");
    std::fs::create_dir_all(bundle.join("peek")).unwrap();
    std::fs::write(bundle.join("peek/SKILL.toml"), "[skill]\nname='peek'\ndescription='Steering boundary fixture'\n[[tools]]\nname='history'\ndescription='Read history'\nkind='builtin'\ncommand=''\ntarget='sessions_history'\n[[tools]]\nname='send'\ndescription='Append message'\nkind='builtin'\ncommand=''\ntarget='sessions_send'\n").unwrap();
    config.skill_bundles.insert(
        "probe".into(),
        zeroclaw_config::schema::SkillBundleConfig {
            directory: Some(bundle.to_string_lossy().into_owned()),
            ..Default::default()
        },
    );
    config.agents.get_mut("test-agent").unwrap().skill_bundles = vec!["probe".into()];
    let data_dir = config.data_dir.clone();
    let (fixture, sessions, backend, _) = make_persistence_test_dispatcher(config, &data_dir);
    let ctx = Arc::clone(&fixture.ctx);
    scoped_dispatcher(&ctx, 4343)
        .await
        .handle_session_new_for_test(
            &json!({"agent_alias":"test-agent", "session_id":"victim", "chat_mode":"chat"}),
        )
        .await
        .unwrap();
    backend
        .append("rpc_victim", &ChatMessage::assistant(FOREIGN))
        .unwrap();
    let before = serde_json::to_value(backend.load("rpc_victim")).unwrap();
    let (mut alice, mut alice_rx) = roster_peer(&ctx, 4242).await;
    let (mut steerer, mut steerer_rx) = roster_peer(&ctx, 4242).await;
    let created = rpc(
        &mut alice,
        &mut alice_rx,
        10,
        "session/new",
        json!({"agent_alias":"test-agent", "session_id":"actor", "chat_mode":"chat"}),
    )
    .await;
    assert!(created.get("error").is_none(), "{created}");
    let agent = sessions.get_agent("actor").await.unwrap();
    let (first, mut handles) = recording_provider(true);
    let results = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (write_started, mut write_started_rx) = tokio::sync::mpsc::unbounded_channel();
    let (release_write, write_gate) = tokio::sync::oneshot::channel();
    {
        let mut guard = agent.lock().await;
        for name in TOOLS {
            assert!(
                guard.tool_names().contains(name),
                "missing real tool {name}"
            );
        }
        guard.set_tool_dispatcher(Box::new(crate::agent::dispatcher::NativeToolDispatcher));
        guard.set_model_provider(Box::new(SessionToolsAfterSteering {
            first,
            calls: std::sync::atomic::AtomicUsize::new(0),
            results: Arc::clone(&results),
        }));
        if matches!(demotion, Demotion::DuringMemoryWrite) {
            guard.set_auto_save_memory_for_test(Arc::new(GatedMemory {
                stored: Arc::new(std::sync::Mutex::new(Vec::new())),
                gate_on: STEER_TEXT.into(),
                started: write_started,
                release: tokio::sync::Mutex::new(Some(write_gate)),
            }));
        }
    }
    send_prompt(&mut alice, 1, "actor", 1).await;
    handles.await_start().await;
    let steered = rpc(
        &mut steerer,
        &mut steerer_rx,
        2,
        "session/steer",
        json!({"session_id":"actor", "content":STEER_TEXT}),
    )
    .await;
    assert_eq!(steered["result"]["accepted"], true, "{steered}");
    if matches!(demotion, Demotion::BeforeConsumption) {
        set_alice_administrator(&ctx, false);
    }
    handles.release();
    if matches!(demotion, Demotion::DuringMemoryWrite) {
        tokio::time::timeout(std::time::Duration::from_secs(10), write_started_rx.recv())
            .await
            .unwrap()
            .unwrap();
        set_alice_administrator(&ctx, false);
        release_write.send(()).unwrap();
    }
    let (response, _) = response_and_notifications(&mut alice_rx, 1).await;
    assert!(response.get("error").is_none(), "{response}");
    let output = results.lock().unwrap().clone();
    let denied = !matches!(demotion, Demotion::Never);
    assert_eq!(output.len(), 6, "{demotion:?}/{wildcard}: {output:?}");
    assert_eq!(
        output.iter().any(|text| text.contains(FOREIGN)),
        !denied,
        "{demotion:?}/{wildcard}: foreign history must follow current authority: {output:?}"
    );
    assert!(
        output.iter().any(|text| {
            let result: Value = serde_json::from_str(text).unwrap();
            result["tool_call_id"] == "steering-4" && result["content"] == "4"
        }),
        "ordinary calculator must still execute: {output:?}"
    );
    assert!(
        output.iter().any(|text| text.contains("actor")),
        "sessions_current remains: {output:?}"
    );
    if denied {
        assert_eq!(
            serde_json::to_value(backend.load("rpc_victim")).unwrap(),
            before,
            "foreign transcript must remain byte-for-byte equivalent"
        );
        for id in 0..4 {
            assert!(
                output.iter().any(|text| {
                    let result: Value = serde_json::from_str(text).unwrap();
                    result["tool_call_id"] == format!("steering-{id}")
                        && text.contains("Unknown tool")
                }),
                "canonical and alias calls must be refused: {output:?}"
            );
        }
        let guard = agent.lock().await;
        assert!(
            guard
                .delegate_tool
                .as_ref()
                .unwrap()
                .withholds_session_data_tools()
        );
        if wildcard {
            assert!(guard.tool_names().contains(&"delegate"));
        }
    } else {
        assert!(
            backend
                .load("rpc_victim")
                .iter()
                .any(|m| m.content.contains(APPEND)),
            "admin control proves the append probe reaches storage"
        );
        assert!(
            !agent
                .lock()
                .await
                .delegate_tool
                .as_ref()
                .unwrap()
                .withholds_session_data_tools()
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steering_withholds_session_tools_before_consumption() {
    for wildcard in [true, false] {
        session_tools_after_steering(Demotion::BeforeConsumption, wildcard).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steering_withholds_session_tools_after_memory_wait() {
    for wildcard in [true, false] {
        session_tools_after_steering(Demotion::DuringMemoryWrite, wildcard).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steering_keeps_session_tools_for_unchanged_admin() {
    for wildcard in [true, false] {
        session_tools_after_steering(Demotion::Never, wildcard).await;
    }
}
