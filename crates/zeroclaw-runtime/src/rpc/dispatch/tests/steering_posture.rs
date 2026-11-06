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
                .map(|m| json!({"tool_call_id": m.tool_result.as_ref().unwrap().tool_call_id, "content": m.content}).to_string())
                .collect();
        }
        Ok(zeroclaw_providers::ChatResponse {
            output_blocks: Vec::new(),
            text: (call > 1).then(|| "steering probe complete".into()),
            // Ask even when the tool was not offered: execution must refuse it too.
            tool_calls: if call == 1 {
                [
                    ("sessions_history", json!({"session_id":"victim"})),
                    ("peek__history", json!({"session_id":"victim"})),
                    (
                        "sessions_send",
                        json!({"session_id":"victim", "message":APPEND}),
                    ),
                    (
                        "peek__send",
                        json!({"session_id":"victim", "message":APPEND}),
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
    let (fixture, sessions) = make_native_test_dispatcher(config);
    let ctx = Arc::clone(&fixture.ctx);
    scoped_dispatcher(&ctx, 4343)
        .await
        .handle_session_new_for_test(
            &json!({"agent_alias":"test-agent", "session_id":"victim", "chat_mode":"chat"}),
        )
        .await
        .unwrap();
    seed_native_assistant(&fixture, "victim", FOREIGN).await;
    let before = native_test_transcript(&ctx, "victim");
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
            native_test_transcript(&ctx, "victim"),
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
                .withholds_unrestricted_tools()
        );
        if wildcard {
            assert!(guard.tool_names().contains(&"delegate"));
        }
    } else {
        assert_eq!(
            native_test_transcript(&ctx, "victim"),
            before,
            "native history mutation tools refuse the write even for an administrator"
        );
        assert!(
            output
                .iter()
                .any(|text| text.contains("cannot mutate native event history")),
            "admin control reaches the native mutation refusal: {output:?}"
        );
        assert!(
            !agent
                .lock()
                .await
                .delegate_tool
                .as_ref()
                .unwrap()
                .withholds_unrestricted_tools()
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

const STEER_TEXT: &str = "please also check the second file";
struct RecordingTurnProvider {
    started: tokio::sync::mpsc::UnboundedSender<()>,
    release: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    calls: Arc<tokio::sync::Mutex<Vec<(String, Vec<String>)>>>,
}

#[async_trait]
impl zeroclaw_api::model_provider::ModelProvider for RecordingTurnProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        Ok("ok".to_string())
    }

    fn supports_native_tools(&self) -> bool {
        true
    }

    async fn chat(
        &self,
        request: zeroclaw_providers::ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<zeroclaw_providers::ChatResponse> {
        let text = request
            .messages
            .iter()
            .map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join("\n");
        let tools = request
            .tools
            .unwrap_or_default()
            .iter()
            .map(|spec| spec.name.clone())
            .collect();
        self.calls.lock().await.push((text, tools));
        let _ = self.started.send(());
        let gate = self.release.lock().await.take();
        if let Some(gate) = gate {
            let _ = gate.await;
        }
        Ok(zeroclaw_providers::ChatResponse {
            output_blocks: Vec::new(),
            text: Some("steered reply".to_string()),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None,
        })
    }
}

impl zeroclaw_api::attribution::Attributable for RecordingTurnProvider {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        zeroclaw_api::attribution::Role::Provider(zeroclaw_api::attribution::ProviderKind::Model(
            zeroclaw_api::attribution::ModelProviderKind::Custom,
        ))
    }
    fn alias(&self) -> &str {
        "recording-provider"
    }
}

struct RecordingHandles {
    started: tokio::sync::mpsc::UnboundedReceiver<()>,
    release: Option<tokio::sync::oneshot::Sender<()>>,
}

impl RecordingHandles {
    async fn await_start(&mut self) {
        tokio::time::timeout(std::time::Duration::from_secs(10), self.started.recv())
            .await
            .expect("the turn must reach the provider")
            .expect("the provider stays alive");
    }

    fn release(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }
}

fn recording_provider(gated: bool) -> (RecordingTurnProvider, RecordingHandles) {
    let (started_tx, started) = tokio::sync::mpsc::unbounded_channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let calls = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    (
        RecordingTurnProvider {
            started: started_tx,
            release: tokio::sync::Mutex::new(gated.then_some(release_rx)),
            calls: Arc::clone(&calls),
        },
        RecordingHandles {
            started,
            release: gated.then_some(release_tx),
        },
    )
}

struct GatedMemory {
    stored: Arc<std::sync::Mutex<Vec<String>>>,
    gate_on: String,
    started: tokio::sync::mpsc::UnboundedSender<()>,
    release: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}

impl GatedMemory {
    async fn write(&self, content: &str) {
        self.stored.lock().unwrap().push(content.to_string());
        if content.contains(&self.gate_on) {
            let gate = self.release.lock().await.take();
            if let Some(gate) = gate {
                let _ = self.started.send(());
                let _ = gate.await;
            }
        }
    }
}

impl zeroclaw_api::attribution::Attributable for GatedMemory {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        zeroclaw_api::attribution::Role::Memory(zeroclaw_api::attribution::MemoryKind::None)
    }
    fn alias(&self) -> &str {
        "gated"
    }
}

#[async_trait]
impl zeroclaw_api::memory_traits::Memory for GatedMemory {
    fn name(&self) -> &str {
        "gated"
    }

    async fn store(
        &self,
        _key: &str,
        content: &str,
        _category: zeroclaw_api::memory_traits::MemoryCategory,
        _session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        self.write(content).await;
        Ok(())
    }

    async fn recall(
        &self,
        _query: &str,
        _limit: usize,
        _session_id: Option<&str>,
        _since: Option<&str>,
        _until: Option<&str>,
    ) -> anyhow::Result<Vec<zeroclaw_api::memory_traits::MemoryEntry>> {
        Ok(Vec::new())
    }

    async fn get(
        &self,
        _key: &str,
    ) -> anyhow::Result<Option<zeroclaw_api::memory_traits::MemoryEntry>> {
        Ok(None)
    }

    async fn list(
        &self,
        _category: Option<&zeroclaw_api::memory_traits::MemoryCategory>,
        _session_id: Option<&str>,
    ) -> anyhow::Result<Vec<zeroclaw_api::memory_traits::MemoryEntry>> {
        Ok(Vec::new())
    }

    async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
        Ok(false)
    }

    async fn forget_for_agent(&self, _key: &str, _agent_id: &str) -> anyhow::Result<bool> {
        Ok(false)
    }

    async fn purge_session_for_agent(
        &self,
        _session_id: &str,
        _agent_id: &str,
    ) -> anyhow::Result<usize> {
        Ok(0)
    }

    async fn count(&self) -> anyhow::Result<usize> {
        Ok(0)
    }

    async fn health_check(&self) -> bool {
        true
    }

    async fn store_with_agent(
        &self,
        _key: &str,
        content: &str,
        _category: zeroclaw_api::memory_traits::MemoryCategory,
        _session_id: Option<&str>,
        _namespace: Option<&str>,
        _importance: Option<f64>,
        _agent_id: Option<&str>,
    ) -> anyhow::Result<()> {
        self.write(content).await;
        Ok(())
    }

    async fn recall_for_agents(
        &self,
        _allowed_agent_ids: &[&str],
        _query: &str,
        _limit: usize,
        _session_id: Option<&str>,
        _since: Option<&str>,
        _until: Option<&str>,
    ) -> anyhow::Result<Vec<zeroclaw_api::memory_traits::MemoryEntry>> {
        Ok(Vec::new())
    }
}

/// Read frames until the response with `id`, returning it and every
/// notification that preceded it.
async fn response_and_notifications(
    rx: &mut tokio::sync::mpsc::Receiver<String>,
    id: u64,
) -> (Value, Vec<Value>) {
    let mut notifications = Vec::new();
    loop {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
            .await
            .expect("a response within 10s")
            .expect("writer channel open");
        let value: Value = serde_json::from_str(&frame).expect("valid JSON-RPC frame");
        if value.get("id") == Some(&json!(id)) {
            return (value, notifications);
        }
        notifications.push(value);
    }
}

/// Send `session/prompt` through `process_line` with a client turn
/// generation.
async fn send_prompt(
    dispatcher: &mut RpcDispatcher,
    id: u64,
    session_id: &str,
    client_turn_generation: u64,
) {
    let line = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "session/prompt",
        "params": {
            "session_id": session_id,
            "prompt": "hello",
            "client_turn_generation": client_turn_generation,
        },
    })
    .to_string();
    dispatcher.process_line(&line).await;
}
