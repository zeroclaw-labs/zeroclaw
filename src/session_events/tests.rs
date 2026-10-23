use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use zeroclaw_api::attribution::{Attributable, Role};
use zeroclaw_api::model_provider::{ChatRequest, ChatResponse, ModelProvider, ToolCall};
use zeroclaw_api::session_events::*;
use zeroclaw_kernel::session::execution::UserTurnCommand;
use zeroclaw_kernel::session::query::{SessionAccess, SessionAccessPolicy};
use zeroclaw_kernel::session::{EffectIntent, PermissionResolver, SessionError};

struct Access;
impl SessionAccessPolicy for Access {
    fn authorize(
        &self,
        _: &zeroclaw_api::principal::PrincipalId,
        _: &SessionStreamId,
        _: SessionAccess,
    ) -> Result<(), SessionError> {
        Ok(())
    }
}
impl PermissionResolver for Access {
    fn child_authority(
        &self,
        _target: &str,
        _stream: &SessionStreamId,
    ) -> Result<Arc<dyn zeroclaw_kernel::session::engine::EngineSessionAuthority>, SessionError>
    {
        Ok(Arc::new(Access))
    }

    fn authorize_context(&self, _: &SessionStreamId, _: &BranchId) -> Result<(), SessionError> {
        Ok(())
    }
    fn authorize(&self, _: &EffectIntent) -> Result<(), SessionError> {
        Ok(())
    }
}
struct Provider {
    calls: Arc<AtomicUsize>,
}
impl Attributable for Provider {
    fn role(&self) -> Role {
        Role::System
    }
    fn alias(&self) -> &str {
        "fixture"
    }
}
#[async_trait::async_trait]
impl ModelProvider for Provider {
    async fn chat_with_system(
        &self,
        _: Option<&str>,
        _: &str,
        _: &str,
        _: Option<f64>,
    ) -> anyhow::Result<String> {
        anyhow::bail!("structured fixture only")
    }
    async fn chat(
        &self,
        _: ChatRequest<'_>,
        _: &str,
        _: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        let first = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
        Ok(ChatResponse {
            text: Some(
                if first {
                    "running fixture"
                } else {
                    "fixture complete"
                }
                .into(),
            ),
            tool_calls: if first {
                vec![ToolCall {
                    id: "fixture-call".into(),
                    name: "fixture_effect".into(),
                    arguments: "{}".into(),
                    extra_content: None,
                }]
            } else {
                vec![]
            },
            reasoning_content: Some("fixture signed reasoning".into()),
            usage: None,
            output_blocks: vec![],
        })
    }
    fn supports_native_tools(&self) -> bool {
        true
    }
}
struct Effect {
    calls: Arc<AtomicUsize>,
}

#[tokio::test]
async fn persistent_cli_import_restore_and_clear_use_events_and_fence_legacy_saves() {
    use zeroclaw_api::model_provider::ChatMessage;
    use zeroclaw_api::session_events::capture::{
        ContextSnapshot, ContextTransformIntent, EngineTurnInput, current_provider_admission,
    };
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::TempDir::new().expect("dir");
        let path = dir.path().join("interactive.json");
        let legacy = zeroclaw_runtime::agent::history::InteractiveSessionState {
            version: 2,
            history: vec![
                ChatMessage::system("fixture system"),
                ChatMessage::user("legacy input"),
                ChatMessage::assistant("legacy answer"),
            ],
            history_has_trim_breadcrumb: false,
        };
        let raw = serde_json::to_vec(&legacy).expect("legacy codec");
        std::fs::write(&path, &raw).expect("legacy file");
        let mut config = zeroclaw_config::schema::Config::default();
        config.agents.insert(
            "fixture".into(),
            zeroclaw_config::schema::AliasedAgentConfig::default(),
        );
        let authority = zeroclaw_runtime::live_config_authority::LiveConfigAuthority::new(config);
        let port = open_cli_session(
            dir.path(),
            backend,
            "fixture",
            authority.execution_capability(),
            Some(&path),
        )
        .await
        .expect("open");
        let restored = port
            .restore_context()
            .await
            .expect("restore")
            .expect("context");
        assert_eq!(
            canonical_json(&restored.messages).expect("restored messages"),
            canonical_json(
                &legacy
                    .history
                    .iter()
                    .cloned()
                    .map(zeroclaw_api::model_provider::ConversationMessage::Chat)
                    .collect::<Vec<_>>()
            )
            .expect("source messages")
        );
        assert!(
            zeroclaw_runtime::agent::history::save_interactive_session_history(&path, &[]).is_err(),
            "native lease must fail legacy writes without waiting"
        );
        assert!(!path.exists(), "legacy source is archived after upgrade");
        assert!(
            !dir.path().join("interactive.json.native-session").exists(),
            "completed imports keep no path locator"
        );
        assert_eq!(
            std::fs::read(path.with_extension("json.backup")).expect("source backup"),
            raw
        );
        let input = EngineTurnInput {
            execution_workspace: None,
            reply_route: None,
            turn_id: TurnId::new().to_string(),
            agent_alias: Some("fixture".into()),
            ingress: zeroclaw_api::ingress::IngressContext::interactive(),
            parts: zeroclaw_api::session_events::capture::EngineInputPart::text_and_files(
                "new input".into(),
                vec![],
            ),
            provider_ref: "fixture".into(),
            model: "fixture".into(),
        };
        let snapshot = ContextSnapshot {
            messages: (vec![
                ChatMessage::system("fixture system"),
                ChatMessage::user("new input"),
                ChatMessage::assistant("new answer"),
            ])
            .into_iter()
            .map(zeroclaw_api::model_provider::ConversationMessage::Chat)
            .collect(),
            history_has_trim_breadcrumb: Some(false),
        };
        let saved = snapshot.clone();
        port.execute(
            input,
            Box::pin(async move {
                let admission = current_provider_admission().expect("bound native provider scope");
                let transform = admission
                    .begin_context_transform(ContextTransformIntent {
                        reason_key: "fixture_projection".into(),
                        algorithm_version: "fixture/v1".into(),
                        budget: 0,
                    })
                    .await?
                    .expect("transform");
                transform.complete_snapshot(saved).await?;
                Ok("new answer".into())
            }),
        )
        .await
        .expect("turn");
        drop(port);
        let reopened = open_cli_session(
            dir.path(),
            backend,
            "fixture",
            authority.execution_capability(),
            Some(&path),
        )
        .await
        .expect("reopen");
        let restored = reopened
            .restore_context()
            .await
            .expect("restore committed")
            .expect("context");
        assert_eq!(
            canonical_json(&restored).expect("codec"),
            canonical_json(&snapshot).expect("codec")
        );
        let cleared = ContextSnapshot {
            messages: (vec![ChatMessage::system("fixture system")])
                .into_iter()
                .map(zeroclaw_api::model_provider::ConversationMessage::Chat)
                .collect(),
            history_has_trim_breadcrumb: Some(false),
        };
        reopened
            .replace_context(cleared.clone(), "operator_clear_context")
            .await
            .expect("clear");
        drop(reopened);
        let reopened = open_cli_session(
            dir.path(),
            backend,
            "fixture",
            authority.execution_capability(),
            Some(&path),
        )
        .await
        .expect("reopen cleared");
        assert_eq!(
            canonical_json(
                &reopened
                    .restore_context()
                    .await
                    .expect("restored")
                    .expect("context")
            )
            .expect("codec"),
            canonical_json(&cleared).expect("codec")
        );
        assert!(!path.exists());
        assert_eq!(
            std::fs::read(path.with_extension("json.backup")).expect("legacy backup unchanged"),
            raw
        );
    }
}

#[tokio::test]
async fn cli_native_admission_retries_without_polling_and_checks_current_authority() {
    use zeroclaw_api::session_events::capture::EngineTurnInput;
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::TempDir::new().expect("dir");
        let mut config = zeroclaw_config::schema::Config::default();
        config.agents.insert(
            "fixture".into(),
            zeroclaw_config::schema::AliasedAgentConfig::default(),
        );
        let authority = zeroclaw_runtime::live_config_authority::LiveConfigAuthority::new(config);
        let port = new_cli_session(
            dir.path(),
            backend,
            "fixture",
            authority.execution_capability(),
        )
        .await
        .expect("CLI session");
        let input = EngineTurnInput {
            execution_workspace: None,
            reply_route: None,
            turn_id: TurnId::new().to_string(),
            agent_alias: Some("fixture".into()),
            ingress: zeroclaw_api::ingress::IngressContext::interactive(),
            parts: zeroclaw_api::session_events::capture::EngineInputPart::text_and_files(
                "operator input".into(),
                vec![],
            ),
            provider_ref: "fixture".into(),
            model: "fixture".into(),
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&calls);
        assert_eq!(
            port.execute(
                input.clone(),
                Box::pin(async move {
                    observed.fetch_add(1, Ordering::SeqCst);
                    Ok("host answer".into())
                })
            )
            .await
            .expect("first"),
            "host answer"
        );
        assert_eq!(
            port.execute(
                input.clone(),
                Box::pin(async { panic!("a duplicate engine future must not be polled") })
            )
            .await
            .expect("retry"),
            "host answer"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let mut different = input.clone();
        different.parts = vec![
            zeroclaw_api::session_events::capture::EngineInputPart::Text("different input".into()),
        ];
        assert!(
            port.execute(
                different,
                Box::pin(async { panic!("conflicting input must not execute") })
            )
            .await
            .is_err()
        );
        authority.close_agent_lifecycle();
        assert!(
            port.execute(
                input,
                Box::pin(async { panic!("closed authority must not execute") })
            )
            .await
            .is_err()
        );
    }
}
impl Attributable for Effect {
    fn role(&self) -> Role {
        Role::System
    }
    fn alias(&self) -> &str {
        "fixture_effect"
    }
}
#[async_trait::async_trait]
impl zeroclaw_api::tool::Tool for Effect {
    fn name(&self) -> &str {
        "fixture_effect"
    }
    fn description(&self) -> &str {
        "fixture"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type":"object","properties":{}})
    }
    async fn execute(
        &self,
        _: serde_json::Value,
    ) -> anyhow::Result<zeroclaw_api::tool::ToolResult> {
        assert!(zeroclaw_api::session_events::capture::current_tool_observation().is_some());
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(zeroclaw_api::tool::ToolResult::ok("fixture effect done"))
    }
}
#[tokio::test]
async fn existing_agent_executes_through_coordinator_and_does_not_replay_effects() {
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::TempDir::new().expect("dir");
        let store = open_event_store(dir.path(), backend).expect("store");
        let stream = SessionStreamId::new();
        let branch = BranchId::new();
        let mut root = SessionEvent::typed(
            stream.clone(),
            EventData::SessionStarted {
                session_id: SessionId::new(),
                owning_agent: "fixture".into(),
                capture_mode: CaptureMode::Native,
            },
            EventActor::default(),
        )
        .expect("root");
        root.branch_id = Some(branch.clone());
        let ingress = PayloadRecord::json(
            "session.ingress/v1",
            &serde_json::json!({"source":"embedded_fixture"}),
        )
        .expect("ingress");
        let binding = PayloadRecord::json(
            "session.execution-binding/v1",
            &serde_json::json!({"fixture":true}),
        )
        .expect("binding");
        let context =
            PayloadRecord::json("session.context/v1", &serde_json::json!({"messages":[]}))
                .expect("context");
        let mut projection = SessionEvent::typed(
            stream.clone(),
            EventData::ContextProjectionCreated {
                source_ranges: vec![],
                summary_ref: None,
                budget: 4096,
                reason_key: "initial_context".into(),
                algorithm_version: "fixture/v1".into(),
                result_ref: StoredPayloadRef::from(&context),
                summary_source_refs: vec![],
                transform_started_event_id: None,
            },
            EventActor::default(),
        )
        .expect("projection");
        projection.branch_id = Some(branch.clone());
        let context_event_id = projection.event_id.clone();
        store
            .append_batch(AppendRequest {
                stream_id: stream.clone(),
                expected_head: EventCursor::beginning(stream.clone()),
                idempotency_key: "root".into(),
                events: vec![root, projection],
                payloads: vec![ingress.clone(), binding.clone(), context],
                erasure_reference_commitment: None,
            })
            .expect("initialize");
        let provider_calls = Arc::new(AtomicUsize::new(0));
        let tool_calls = Arc::new(AtomicUsize::new(0));
        let sources = Arc::new(Sources {
            provider_calls: Arc::clone(&provider_calls),
            tool_calls: Arc::clone(&tool_calls),
            resolutions: std::sync::Mutex::new(Vec::new()),
        });
        let capabilities = zeroclaw_kernel::composition::RuntimeCapabilities {
            providers: sources.clone(),
            memory: sources.clone(),
            tools: sources.clone(),
            channels: sources.clone(),
            observer: Arc::new(zeroclaw_runtime::observability::NoopObserver),
        };
        let mut config = zeroclaw_config::schema::Config::default();
        config.data_dir = dir.path().to_owned();
        let mut agent_config = zeroclaw_config::schema::AliasedAgentConfig::default();
        agent_config.model_provider = "openai.fixture".into();
        agent_config.risk_profile = "fixture".into();
        config.agents.insert("fixture".into(), agent_config);
        config.risk_profiles.insert(
            "fixture".into(),
            zeroclaw_config::schema::RiskProfileConfig {
                allowed_tools: vec!["fixture_effect".into()],
                auto_approve: vec!["fixture_effect".into()],
                approval_route: Some(zeroclaw_config::autonomy::ApprovalRoute {
                    approver_channel: "fixture-approval".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        config.providers.models.openai.insert(
            "fixture".into(),
            zeroclaw_config::schema::OpenAIModelProviderConfig {
                base: zeroclaw_config::schema::ModelProviderConfig {
                    model: Some("fixture-model".into()),
                    ..Default::default()
                },
            },
        );
        let config = Arc::new(config);
        let live = zeroclaw_config::live::LiveConfig::new((*config).clone());
        let runtime =
            Arc::from(zeroclaw_config::platform::create_runtime(&config.runtime).expect("runtime"));
        let mut agent = zeroclaw_runtime::agent::Agent::from_capabilities(
            &live.handle(),
            "fixture",
            &zeroclaw_api::principal::PrincipalId::shared_operator(),
            &["fixture_effect".into()],
            &capabilities,
            runtime,
        )
        .await
        .expect("supplied agent");
        assert_eq!(
            *sources.resolutions.lock().expect("resolutions"),
            ["provider", "memory", "tools", "channel"]
        );
        let prior_capacity = agent.context_limits().model_context_window;
        let mut updated = live.handle().snapshot();
        let capacity = prior_capacity.saturating_add(4096);
        updated
            .providers
            .models
            .openai
            .get_mut("fixture")
            .expect("profile")
            .base
            .context_window = Some(capacity);
        live.publish(live.next_revision().expect("revision"), updated)
            .expect("publication");
        assert_eq!(
            agent.context_limits().model_context_window,
            capacity,
            "injected sessions resolve current limits without rebuilding the provider"
        );
        let mut executor = EmbeddedTurnExecutor::new(&mut agent);
        let owner = Arc::new(SessionCoordinator::new(Arc::clone(&store)));
        let command = UserTurnCommand {
            stream_id: stream.clone(),
            branch_id: branch,
            turn_id: TurnId::new(),
            message_id: MessageId::new(),
            ingress_key: "fixture-turn".into(),
            parts: vec![
                zeroclaw_api::session_events::capture::EngineInputPart::Text("run fixture".into()),
            ],
            ingress_ref: StoredPayloadRef::from(&ingress),
            reply_route_ref: None,
            execution_binding_ref: StoredPayloadRef::from(&binding),
            context_event_id,
            input_payloads: vec![],
        };
        let receipt = owner
            .execute_user_turn(
                command.clone(),
                zeroclaw_api::principal::PrincipalId::shared_operator(),
                Arc::new(Access),
                Arc::new(Access),
                &mut executor,
            )
            .await
            .expect("turn");
        assert_eq!(
            receipt,
            owner
                .execute_user_turn(
                    command,
                    zeroclaw_api::principal::PrincipalId::shared_operator(),
                    Arc::new(Access),
                    Arc::new(Access),
                    &mut executor
                )
                .await
                .expect("retry")
        );
        assert_eq!(provider_calls.load(Ordering::SeqCst), 2);
        assert_eq!(tool_calls.load(Ordering::SeqCst), 1);
        let events = store
            .read_page(
                &stream,
                &EventCursor::beginning(stream.clone()),
                None,
                100,
                4 * 1024 * 1024,
            )
            .expect("events")
            .events;
        assert_eq!(
            events.last().expect("turn outcome").body.kind(),
            EventKind::TurnCommitted
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.body.kind() == EventKind::ToolEffectStarted)
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.body.kind() == EventKind::ToolEffectCompleted)
                .count(),
            1
        );
        for event in events {
            if let EventData::ProviderAttemptCompleted { response_ref, .. } = event.body.data {
                let PayloadRead::Present(payload) = store
                    .read_payload(&response_ref.payload_id)
                    .expect("response")
                else {
                    panic!("response");
                };
                let response: ProviderResponsePayload =
                    serde_json::from_slice(&payload.body).expect("codec");
                assert!(
                    response
                        .blocks
                        .iter()
                        .any(|block| matches!(block, ResponseBlock::Reasoning { .. }))
                );
            }
        }
    }
}

struct Sources {
    provider_calls: Arc<AtomicUsize>,
    tool_calls: Arc<AtomicUsize>,
    resolutions: std::sync::Mutex<Vec<&'static str>>,
}
impl zeroclaw_kernel::composition::ProviderSource for Sources {
    fn model_provider(
        &self,
        request: &zeroclaw_kernel::composition::ProviderRequest<'_>,
    ) -> anyhow::Result<Arc<dyn ModelProvider>> {
        assert_eq!(request.agent_alias, "fixture");
        assert_eq!(request.provider_ref, Some("openai.fixture"));
        assert_eq!(
            request.principal,
            Some(&zeroclaw_api::principal::PrincipalId::shared_operator())
        );
        self.resolutions
            .lock()
            .expect("resolutions")
            .push("provider");
        Ok(Arc::new(Provider {
            calls: Arc::clone(&self.provider_calls),
        }))
    }
}
impl zeroclaw_kernel::composition::MemorySource for Sources {
    fn memory(
        &self,
        _: &zeroclaw_kernel::composition::MemoryRequest<'_>,
    ) -> anyhow::Result<Arc<dyn zeroclaw_api::memory_traits::Memory>> {
        self.resolutions.lock().expect("resolutions").push("memory");
        Ok(Arc::new(zeroclaw_memory::NoneMemory::new("fixture")))
    }
}
impl zeroclaw_kernel::composition::ToolSource for Sources {
    fn tools(
        &self,
        request: &zeroclaw_kernel::composition::ToolRequest<'_>,
    ) -> anyhow::Result<Vec<Box<dyn zeroclaw_api::tool::Tool>>> {
        assert_eq!(request.agent_alias, "fixture");
        self.resolutions.lock().expect("resolutions").push("tools");
        Ok(vec![Box::new(Effect {
            calls: Arc::clone(&self.tool_calls),
        })])
    }
}
impl zeroclaw_kernel::composition::ChannelSource for Sources {
    fn channel(&self, _: &str) -> Option<Arc<dyn zeroclaw_api::channel::Channel>> {
        self.resolutions
            .lock()
            .expect("resolutions")
            .push("channel");
        None
    }
}

impl zeroclaw_kernel::session::engine::EngineSessionAuthority for Access {
    fn execution_binding(
        &self,
        _input: &zeroclaw_api::session_events::capture::EngineTurnInput,
    ) -> Result<PayloadRecord, SessionError> {
        Ok(PayloadRecord::json(
            "session.execution-binding/v1",
            &serde_json::json!({"fixture":true}),
        )
        .map_err(AppendError::from)?)
    }
    fn maintenance_binding(&self) -> Result<PayloadRecord, SessionError> {
        Ok(PayloadRecord::json(
            "session.execution-binding/v1",
            &serde_json::json!({"fixture":true}),
        )
        .map_err(AppendError::from)?)
    }
    fn diagnostic(&self, error: &anyhow::Error) -> Option<String> {
        Some(format!("{error:#}"))
    }
}

#[tokio::test]
async fn native_family_source_restores_context_fences_legacy_and_excludes_another_owner() {
    use zeroclaw_api::session_events::capture::{ContextSnapshot, EngineTurnInput};
    use zeroclaw_kernel::session::engine::EngineSessionSource;
    for backend in ["sqlite", "jsonl"] {
        let directory = tempfile::TempDir::new().expect("directory");
        let legacy = Arc::new(
            zeroclaw_infra::session_store::SessionStore::new(directory.path()).expect("legacy"),
        );
        legacy
            .append(
                "rpc_fixture",
                &zeroclaw_api::model_provider::ChatMessage::user("legacy text"),
            )
            .expect("legacy input");
        let mut config = zeroclaw_config::schema::Config::default();
        config.agents.entry("fixture".into()).or_default().enabled = true;
        config.data_dir = directory.path().to_owned();
        config.channels.session_backend = backend.into();
        let authority = zeroclaw_runtime::LiveConfigAuthority::new(config);
        let source = NativeSessionHub::new(authority.live_handle());
        let initial = ContextSnapshot {
            messages: vec![],
            history_has_trim_breadcrumb: Some(false),
        };
        let principal = zeroclaw_api::principal::PrincipalId::shared_operator();
        let session = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            source.open(
                "rpc_fixture".into(),
                "fixture".into(),
                principal.clone(),
                Arc::new(Access),
                initial.clone(),
            ),
        )
        .await
        .expect("publication does not hold locator during nested open")
        .expect("native family");
        assert!(
            source
                .open(
                    "rpc_fixture".into(),
                    "another-agent".into(),
                    principal.clone(),
                    Arc::new(Access),
                    initial.clone()
                )
                .await
                .is_err(),
            "reusing the cached execution owner cannot change the canonical owning agent"
        );
        assert_eq!(
            session
                .restore_context()
                .await
                .expect("restore")
                .expect("context")
                .chat_projection()[0]
                .content,
            "legacy text"
        );
        assert!(
            legacy
                .append(
                    "rpc_fixture",
                    &zeroclaw_api::model_provider::ChatMessage::user("fenced")
                )
                .is_err()
        );
        let other = NativeSessionHub::new(authority.live_handle());
        assert!(
            other
                .open(
                    "rpc_fixture".into(),
                    "fixture".into(),
                    principal.clone(),
                    Arc::new(Access),
                    initial.clone()
                )
                .await
                .is_err()
        );
        let input = EngineTurnInput {
            execution_workspace: None,
            reply_route: None,
            turn_id: TurnId::new().to_string(),
            agent_alias: Some("fixture".into()),
            ingress: zeroclaw_api::ingress::IngressContext::agent_direct(),
            parts: zeroclaw_api::session_events::capture::EngineInputPart::text_and_files(
                "run host".into(),
                vec![],
            ),
            provider_ref: "fixture".into(),
            model: "fixture".into(),
        };
        assert_eq!(
            session
                .execute(
                    input.clone(),
                    Box::pin(async { Ok("durable answer".into()) })
                )
                .await
                .expect("execute"),
            "durable answer"
        );
        drop(source);
        drop(session);
        let reopened = other
            .open(
                "rpc_fixture".into(),
                "fixture".into(),
                principal,
                Arc::new(Access),
                initial,
            )
            .await
            .expect("previous lease released");
        assert_eq!(
            reopened
                .execute(
                    input,
                    Box::pin(async { panic!("reopening cannot replay the effect") })
                )
                .await
                .expect("idempotent replay"),
            "durable answer"
        );
    }
}

#[tokio::test]
async fn child_capture_uses_a_distinct_stream_and_parent_receipt_never_replays() {
    use zeroclaw_api::session_events::capture::{
        ChildDispatchIntent, ContextSnapshot, EngineTurnInput, current_provider_admission,
    };
    use zeroclaw_kernel::session::engine::EngineSessionSource;
    for backend in ["sqlite", "jsonl"] {
        let directory = tempfile::TempDir::new().expect("directory");
        let mut config = zeroclaw_config::schema::Config::default();
        config.data_dir = directory.path().to_owned();
        config.channels.session_backend = backend.into();
        let authority = zeroclaw_runtime::LiveConfigAuthority::new(config);
        let source = NativeSessionHub::new(authority.live_handle());
        let principal = zeroclaw_api::principal::PrincipalId::shared_operator();
        let session = source
            .open(
                "child-fixture".into(),
                "fixture".into(),
                principal.clone(),
                Arc::new(Access),
                ContextSnapshot {
                    messages: vec![],
                    history_has_trim_breadcrumb: Some(false),
                },
            )
            .await
            .expect("open");
        let input = EngineTurnInput {
            execution_workspace: None,
            reply_route: None,
            turn_id: TurnId::new().to_string(),
            agent_alias: Some("fixture".into()),
            ingress: zeroclaw_api::ingress::IngressContext::agent_direct(),
            parts: zeroclaw_api::session_events::capture::EngineInputPart::text_and_files(
                "delegate fixture".into(),
                vec![],
            ),
            provider_ref: "fixture".into(),
            model: "fixture".into(),
        };
        let calls = Arc::new(AtomicUsize::new(1));
        let child_calls = calls.clone();
        session
            .execute(
                input.clone(),
                Box::pin(async move {
                    let parent = current_provider_admission().expect("parent");
                    let child = parent
                        .begin_child(ChildDispatchIntent {
                            relation: "delegate".into(),
                            target_agent: "fixture-child".into(),
                            prompt: "child input".into(),
                            execution_mode: ChildExecutionMode::Sync,
                            task_id: None,
                        })
                        .await?;
                    let result = child
                        .execute(Box::pin(async move {
                            let provider = Provider { calls: child_calls };
                            let messages = [zeroclaw_api::model_provider::ChatMessage::user(
                                "child input",
                            )];
                            let response =
                                zeroclaw_providers::ProviderDispatch::from_ref(&provider)
                                    .chat(
                                        ChatRequest {
                                            messages: &messages,
                                            tools: None,
                                            thinking: None,
                                        },
                                        "fixture",
                                        None,
                                    )
                                    .await?;
                            Ok(response.text.unwrap_or_default())
                        }))
                        .await?;
                    assert_eq!(
                        child
                            .execute(Box::pin(async {
                                panic!("child retry cannot execute provider")
                            }))
                            .await?,
                        result
                    );
                    Ok(result)
                }),
            )
            .await
            .expect("child and parent commit");
        session
            .execute(
                input,
                Box::pin(async { panic!("parent retry cannot recreate child") }),
            )
            .await
            .expect("parent retry");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let native = source
            .query("child-fixture".into(), principal, Arc::new(Access))
            .await
            .expect("query")
            .expect("native");
        let events = native
            .query
            .read_page(
                native.stream.clone(),
                EventCursor::beginning(native.stream.clone()),
                None,
                100,
                4 * 1024 * 1024,
            )
            .await
            .expect("parent events")
            .events;
        assert!(
            !events
                .iter()
                .any(|event| event.body.kind() == EventKind::ProviderAttemptStarted)
        );
        let completed = events
            .iter()
            .find_map(|event| {
                if let EventData::ChildInvocationCompleted {
                    child_terminal_cursor,
                    ..
                } = &event.body.data
                {
                    Some(child_terminal_cursor.clone())
                } else {
                    None
                }
            })
            .expect("parent child settlement");
        assert_ne!(completed.stream_id, native.stream);
        assert_eq!(
            events
                .iter()
                .filter(|event| event.body.kind() == EventKind::ChildInvocationRequested)
                .count(),
            1
        );
        let store = zeroclaw_infra::session_events::family::FamilyEventStore::open(
            directory.path(),
            "child-fixture",
        )
        .expect("family store");
        let child = store
            .read_page(
                &completed.stream_id,
                &EventCursor::beginning(completed.stream_id.clone()),
                Some(&completed),
                100,
                4 * 1024 * 1024,
            )
            .expect("child events")
            .events;
        assert_eq!(
            child.first().expect("origin").body.kind(),
            EventKind::DerivedSessionStarted
        );
        assert_eq!(
            child
                .iter()
                .filter(|event| event.body.kind() == EventKind::ProviderAttemptStarted)
                .count(),
            1
        );
        assert_eq!(
            child.last().expect("terminal").body.kind(),
            EventKind::TurnCommitted
        );
    }
}

#[tokio::test]
async fn reopening_family_recovers_interrupted_child_without_replaying_provider() {
    use zeroclaw_api::session_events::capture::{
        ChildDispatchIntent, ContextSnapshot, EngineTurnInput, ProviderDispatchIntent,
        current_provider_admission,
    };
    use zeroclaw_kernel::session::engine::EngineSessionSource;
    for backend in ["sqlite", "jsonl"] {
        let directory = tempfile::TempDir::new().expect("directory");
        let mut config = zeroclaw_config::schema::Config::default();
        config.data_dir = directory.path().to_owned();
        config.channels.session_backend = backend.into();
        let authority = zeroclaw_runtime::LiveConfigAuthority::new(config);
        let source = NativeSessionHub::new(authority.live_handle());
        let principal = zeroclaw_api::principal::PrincipalId::shared_operator();
        let initial = ContextSnapshot {
            messages: vec![],
            history_has_trim_breadcrumb: Some(false),
        };
        let session = source
            .open(
                "interrupted-family".into(),
                "fixture".into(),
                principal.clone(),
                Arc::new(Access),
                initial.clone(),
            )
            .await
            .expect("open");
        let input = EngineTurnInput {
            execution_workspace: None,
            reply_route: None,
            turn_id: TurnId::new().to_string(),
            agent_alias: Some("fixture".into()),
            ingress: zeroclaw_api::ingress::IngressContext::agent_direct(),
            parts: zeroclaw_api::session_events::capture::EngineInputPart::text_and_files(
                "child interruption".into(),
                vec![],
            ),
            provider_ref: "fixture".into(),
            model: "fixture".into(),
        };
        let ready = Arc::new(tokio::sync::Notify::new());
        let signal = ready.clone();
        let mut execution = Box::pin(session.execute(
            input.clone(),
            Box::pin(async move {
                let child = current_provider_admission()
                    .expect("parent")
                    .begin_child(ChildDispatchIntent {
                        relation: "delegate".into(),
                        target_agent: "fixture-child".into(),
                        prompt: "child".into(),
                        execution_mode: ChildExecutionMode::Sync,
                        task_id: None,
                    })
                    .await?;
                child
                    .execute(Box::pin(async move {
                        current_provider_admission()
                            .expect("child")
                            .admit(ProviderDispatchIntent {
                                requested_route: None,
                                previous_attempt_event_id: None,
                                thinking: None,
                                tool_round_policy:
                                    zeroclaw_api::model_provider::ToolRoundPolicy::default(),
                                provider_ref: "fixture".into(),
                                model: "fixture".into(),
                                messages: vec![],
                                tools: None,
                                temperature: None,
                            })
                            .await?;
                        signal.notify_one();
                        std::future::pending().await
                    }))
                    .await
            }),
        ));
        tokio::select! {
            ()=ready.notified()=>{},
            outcome=&mut execution=>panic!("fixture settled: {outcome:?}"),
        }
        // This drops the complete parent and child future trees. Exclusive
        // lease acquisition then proves that no escaped writer remains.
        drop(execution);
        drop(session);
        drop(source);
        let reopened = NativeSessionHub::new(authority.live_handle());
        let session = reopened
            .open(
                "interrupted-family".into(),
                "fixture".into(),
                principal,
                Arc::new(Access),
                initial,
            )
            .await
            .expect("recover family");
        assert!(
            session
                .execute(
                    input,
                    Box::pin(async { panic!("unknown parent cannot replay") })
                )
                .await
                .is_err()
        );
        let store = zeroclaw_infra::session_events::family::FamilyEventStore::open(
            directory.path(),
            "interrupted-family",
        )
        .expect("store");
        let root = NativeSessionHub::stream_id("interrupted-family").expect("root");
        let parent = store
            .read_page(
                &root,
                &EventCursor::beginning(root.clone()),
                None,
                256,
                4 * 1024 * 1024,
            )
            .expect("parent")
            .events;
        let invocation = parent
            .iter()
            .find(|event| event.body.kind() == EventKind::ChildInvocationRequested)
            .and_then(|event| event.invocation_id.as_ref())
            .expect("child invocation");
        let child = SessionStreamId::parse(format!("child-{invocation}")).expect("child stream");
        let events = store
            .read_page(
                &child,
                &EventCursor::beginning(child.clone()),
                None,
                256,
                4 * 1024 * 1024,
            )
            .expect("child")
            .events;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.body.kind() == EventKind::ProviderAttemptStarted)
                .count(),
            1
        );
        assert!(
            events
                .iter()
                .any(|event| event.body.kind() == EventKind::InvocationOutcomeUnknown)
        );
        assert_eq!(
            events.last().expect("child terminal").body.kind(),
            EventKind::TurnOutcomeUnknown
        );
        assert_eq!(
            parent.last().expect("parent terminal").body.kind(),
            EventKind::TurnOutcomeUnknown
        );
    }
}
