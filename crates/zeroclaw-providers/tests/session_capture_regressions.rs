#[path = "session_capture_regressions/compatible_streaming.rs"]
mod compatible_streaming;

use std::sync::Arc;
use zeroclaw_api::attribution::{Attributable, Role};
use zeroclaw_api::model_provider::*;
use zeroclaw_api::session_events::capture::{
    ProviderRouteSource, scope_provider_capture, scope_provider_route_source,
};
use zeroclaw_api::session_events::*;
use zeroclaw_infra::session_events::open_event_store;
use zeroclaw_kernel::session::{
    EffectIntent, PermissionResolver, SessionCoordinator, SessionError,
};
use zeroclaw_providers::ProviderDispatch;

#[path = "session_capture_regressions/codex_text_done.rs"]
mod codex_text_done;

#[path = "session_capture_regressions/codex_item_parts.rs"]
mod codex_item_parts;

#[path = "session_capture_regressions/plain_reasoning.rs"]
mod plain_reasoning;

#[path = "session_capture_regressions/adapter_gaps.rs"]
mod adapter_gaps;

#[cfg(unix)]
#[path = "session_capture_regressions/cli_transport.rs"]
mod cli_transport;

struct Allow;
impl PermissionResolver for Allow {
    fn authorize(&self, _intent: &EffectIntent) -> Result<(), SessionError> {
        Ok(())
    }
}
struct Harness {
    _dir: tempfile::TempDir,
    store: Arc<dyn SessionEventStore>,
    stream: SessionStreamId,
    admission: Arc<dyn zeroclaw_api::session_events::capture::ProviderAdmissionPort>,
}

/// Validate supplemental evidence separately from assertions about semantic
/// content. Unknown semantic blocks still reach the caller's exhaustive match.
fn semantic_blocks<'a>(
    store: &'a dyn SessionEventStore,
    blocks: &'a [ResponseBlock],
) -> impl Iterator<Item = &'a ResponseBlock> {
    blocks.iter().filter(move |block| {
        let ResponseBlock::Other {
            type_name,
            classified_body_ref,
        } = block
        else {
            return true;
        };
        if type_name != "provider.content_metadata" {
            return true;
        }
        let PayloadRead::Present(payload) = store
            .read_payload(&classified_body_ref.payload_id)
            .expect("supplemental evidence")
        else {
            panic!("supplemental evidence must remain available");
        };
        let value: serde_json::Value =
            serde_json::from_slice(&payload.body).expect("metadata JSON");
        assert!(matches!(
            value["kind"].as_str(),
            Some("provider_item" | "content_metadata")
        ));
        assert!(value["type_name"].is_string());
        assert!(value["fields"].is_object());
        false
    })
}

impl Harness {
    fn new(backend: &str) -> Self {
        let dir = tempfile::TempDir::new().expect("dir");
        let store = open_event_store(dir.path(), backend).expect("store");
        let stream = SessionStreamId::new();
        let branch = BranchId::new();
        let turn = TurnId::new();
        let ingress = PayloadRecord::json(
            "session.ingress/v1",
            &serde_json::json!({"source":"embedded_test"}),
        )
        .expect("ingress");
        let binding = PayloadRecord::json(
            "session.execution-binding/v1",
            &serde_json::json!({"execution":"fixture"}),
        )
        .expect("binding");
        let content = PayloadRecord::json(
            "session.content/v1",
            &ContentPayload {
                message_id: MessageId::new(),
                parts: vec![],
            },
        )
        .expect("content");
        let context = PayloadRecord::json(
            "session.context/v1",
            &serde_json::json!({"message_refs":[],"input_ref":StoredPayloadRef::from(&content)}),
        )
        .expect("context");
        let make = |data: EventData, is_turn: bool| {
            let mut event =
                SessionEvent::typed(stream.clone(), data, EventActor::default()).expect("event");
            event.branch_id = Some(branch.clone());
            if is_turn {
                event.turn_id = Some(turn.clone());
            }
            event
        };
        let root = make(
            EventData::SessionStarted {
                session_id: SessionId::new(),
                owning_agent: "fixture".into(),
                capture_mode: CaptureMode::Native,
            },
            false,
        );
        let accepted = make(
            EventData::TurnAccepted {
                input_ref: StoredPayloadRef::from(&content),
                ingress_ref: StoredPayloadRef::from(&ingress),
                reply_route_ref: None,
            },
            true,
        );
        let started = make(
            EventData::TurnStarted {
                accepted_event_id: accepted.event_id.clone(),
                execution_binding_ref: StoredPayloadRef::from(&binding),
            },
            true,
        );
        let projection = make(
            EventData::ContextProjectionCreated {
                source_ranges: vec![],
                summary_ref: None,
                budget: 4096,
                reason_key: "fixture_context".into(),
                algorithm_version: "fixture/v1".into(),
                result_ref: StoredPayloadRef::from(&context),
                summary_source_refs: vec![],
                transform_started_event_id: None,
            },
            true,
        );
        let context_id = projection.event_id.clone();
        store
            .append_batch(AppendRequest {
                stream_id: stream.clone(),
                expected_head: EventCursor::beginning(stream.clone()),
                idempotency_key: "fixture".into(),
                events: vec![root, accepted, started, projection],
                payloads: vec![ingress, binding, content, context],
                erasure_reference_commitment: None,
            })
            .expect("fixture commit");
        let coordinator = Arc::new(SessionCoordinator::new(Arc::clone(&store)));
        let admission =
            coordinator.provider_capture(stream.clone(), branch, turn, context_id, Arc::new(Allow));
        Self {
            _dir: dir,
            store,
            stream,
            admission,
        }
    }
    fn events(&self) -> Vec<SessionEvent> {
        self.store
            .read_page(
                &self.stream,
                &EventCursor::beginning(self.stream.clone()),
                None,
                1000,
                4 * 1024 * 1024,
            )
            .expect("events")
            .events
    }
    fn payload<T: serde::de::DeserializeOwned>(&self, reference: &StoredPayloadRef) -> T {
        let PayloadRead::Present(record) = self
            .store
            .read_payload(&reference.payload_id)
            .expect("payload")
        else {
            panic!("present")
        };
        serde_json::from_slice(&record.body).expect("decode")
    }
}

struct Mixed;
impl Attributable for Mixed {
    fn role(&self) -> Role {
        Role::System
    }
    fn alias(&self) -> &str {
        "audit-fixture"
    }
}
#[async_trait::async_trait]
impl ModelProvider for Mixed {
    async fn chat_with_system(
        &self,
        _: Option<&str>,
        _: &str,
        _: &str,
        _: Option<f64>,
    ) -> anyhow::Result<String> {
        Ok("answer".into())
    }
    async fn chat(
        &self,
        _: ChatRequest<'_>,
        _: &str,
        _: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        Ok(ChatResponse {
            text: Some("answer".into()),
            tool_calls: vec![ToolCall {
                id: "call-1".into(),
                name: "shell".into(),
                arguments: "{}".into(),
                extra_content: None,
            }],
            reasoning_content: Some("reasoning fixture".into()),
            usage: Some(TokenUsage {
                input_tokens: Some(7),
                output_tokens: Some(3),
                ..TokenUsage::default()
            }),
            output_blocks: vec![ProviderOutputBlock::Text {
                text: "answer".into(),
            }],
        })
    }
}
#[tokio::test]
async fn captured_session_regressions_both_backends() {
    for backend in ["sqlite", "jsonl"] {
        audit_live(backend).await;
        audit_wire(backend).await;
        audit_replay_disclosure(backend).await;
        audit_request_reasoning_disclosure(backend).await;
        audit_late_child_failure(backend).await;
        audit_summary(backend).await;
        let h = Harness::new(backend);
        let messages = [ChatMessage::user("fixture")];
        scope_provider_capture(
            h.admission.clone(),
            ProviderDispatch::from_ref(&Mixed).chat(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "fixture",
                None,
            ),
        )
        .await
        .unwrap();
        let events = h.events();
        let EventData::ProviderAttemptCompleted { response_ref, .. } =
            &events.last().unwrap().body.data
        else {
            panic!()
        };
        let response: ProviderResponsePayload = h.payload(response_ref);
        assert_eq!(response.blocks.len(), 3);
        assert!(response.reasoning_absence.is_none());
    }
}
struct RevokeAfterAccepted {
    store: Arc<dyn SessionEventStore>,
    stream: SessionStreamId,
    turn: TurnId,
}
impl zeroclaw_kernel::session::query::SessionAccessPolicy for RevokeAfterAccepted {
    fn authorize(
        &self,
        _: &zeroclaw_api::principal::PrincipalId,
        _: &SessionStreamId,
        access: zeroclaw_kernel::session::query::SessionAccess,
    ) -> Result<(), SessionError> {
        let events = self
            .store
            .read_page(
                &self.stream,
                &EventCursor::beginning(self.stream.clone()),
                None,
                1000,
                4 * 1024 * 1024,
            )?
            .events;
        if matches!(
            access,
            zeroclaw_kernel::session::query::SessionAccess::Write
        ) && events.iter().any(|e| {
            e.turn_id.as_ref() == Some(&self.turn) && e.body.kind() == EventKind::TurnAccepted
        }) {
            return Err(SessionError::PermissionDenied(
                "audit_revoked_before_start".into(),
            ));
        }
        Ok(())
    }
}
struct ReadAllow;
impl zeroclaw_kernel::session::query::SessionAccessPolicy for ReadAllow {
    fn authorize(
        &self,
        _: &zeroclaw_api::principal::PrincipalId,
        _: &SessionStreamId,
        _: zeroclaw_kernel::session::query::SessionAccess,
    ) -> Result<(), SessionError> {
        Ok(())
    }
}
struct NoEffect;
#[async_trait::async_trait]
impl zeroclaw_kernel::session::execution::TurnExecutorPort for NoEffect {
    async fn execute(
        &mut self,
        _: &str,
        _: Arc<dyn zeroclaw_api::session_events::capture::ProviderAdmissionPort>,
    ) -> anyhow::Result<String> {
        panic!("must not execute")
    }
}
async fn audit_live(backend: &str) {
    use zeroclaw_api::principal::PrincipalId;
    use zeroclaw_api::session_events::capture::EngineInputPart;
    let h = Harness::new(backend);
    let events = h.events();
    let EventData::TurnAccepted { ingress_ref, .. } = &events[1].body.data else {
        panic!()
    };
    let EventData::TurnStarted {
        execution_binding_ref,
        ..
    } = &events[2].body.data
    else {
        panic!()
    };
    let turn = TurnId::new();
    let command = zeroclaw_kernel::session::execution::UserTurnCommand {
        stream_id: h.stream.clone(),
        branch_id: events[0].branch_id.clone().unwrap(),
        turn_id: turn.clone(),
        message_id: MessageId::new(),
        ingress_key: "audit-revoke".into(),
        parts: vec![EngineInputPart::Text("fixture".into())],
        ingress_ref: ingress_ref.clone(),
        reply_route_ref: None,
        execution_binding_ref: execution_binding_ref.clone(),
        context_event_id: events[3].event_id.clone(),
        input_payloads: vec![],
    };
    let owner = Arc::new(SessionCoordinator::new(h.store.clone()));
    let policy = Arc::new(RevokeAfterAccepted {
        store: h.store.clone(),
        stream: h.stream.clone(),
        turn: turn.clone(),
    });
    let err = owner
        .execute_user_turn(
            command,
            PrincipalId::shared_operator(),
            policy,
            Arc::new(Allow),
            &mut NoEffect,
        )
        .await;
    let projection = owner
        .query(PrincipalId::shared_operator(), Arc::new(ReadAllow))
        .project_history(h.stream.clone())
        .await
        .unwrap();
    println!(
        "REVOKED {backend} result={err:?} status={:?}",
        projection.turns[&turn].status
    );
    assert!(matches!(
        projection.turns[&turn].status,
        zeroclaw_api::session_events::projection::TurnStatus::Cancelled
    ));
    // A future content part must not make the remaining known history unreadable.
    let text = PayloadRecord::new(
        PayloadId::new(),
        "session.text/v1".into(),
        b"known text after unknown".to_vec(),
    )
    .unwrap();
    let content=PayloadRecord::new(PayloadId::new(),"session.content/v1".into(),serde_json::to_vec(&serde_json::json!({"message_id":MessageId::new(),"parts":[{"kind":"future_part","coordinate":7},{"kind":"text","text_ref":StoredPayloadRef::from(&text)}]})).unwrap()).unwrap();
    let mut e = SessionEvent::typed(
        h.stream.clone(),
        EventData::AssistantContentRecorded {
            content_ref: StoredPayloadRef::from(&content),
            provenance_refs: vec![],
        },
        EventActor::default(),
    )
    .unwrap();
    e.branch_id = events[0].branch_id.clone();
    e.turn_id = events[1].turn_id.clone();
    h.store
        .append_batch(AppendRequest {
            stream_id: h.stream.clone(),
            expected_head: h.store.head(&h.stream).unwrap(),
            idempotency_key: "audit-future-part".into(),
            events: vec![e],
            payloads: vec![text, content],
            erasure_reference_commitment: None,
        })
        .unwrap();
    let messages = owner
        .query(PrincipalId::shared_operator(), Arc::new(ReadAllow))
        .messages(h.stream.clone())
        .await;
    println!("UNKNOWN_PART {backend} {messages:?}");
    let messages = messages.unwrap();
    assert_eq!(messages.last().unwrap().text, "known text after unknown");
    assert_eq!(messages.last().unwrap().unresolved_parts, vec![0]);
}
async fn audit_wire(backend: &str) {
    use zeroclaw_providers::compatible::{AuthStyle, OpenAiCompatibleModelProvider};
    let h = Harness::new(backend);
    let app=axum::Router::new().route("/chat/completions",axum::routing::post(||async {axum::Json(serde_json::json!({
 "model":"actual-served-model","choices":[{"message":{"role":"assistant","content":"answer","reasoning_content":"wire thinking", "audio":{"data":"AAEC","format":"wav"}},"finish_reason":"length"}],"usage":{"prompt_tokens":7,"completion_tokens":9}
 }))}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = zeroclaw_spawn::spawn!(async move { axum::serve(listener, app).await.unwrap() });
    let provider = OpenAiCompatibleModelProvider::builder("audit")
        .display_name("audit")
        .base_url(&format!("http://{address}"))
        .auth_style(AuthStyle::Bearer)
        .build();
    let text = scope_provider_capture(
        h.admission.clone(),
        ProviderDispatch::from_ref(&provider).chat_with_system(
            None,
            "fixture",
            "requested-model",
            None,
        ),
    )
    .await
    .unwrap();
    let events = h.events();
    let EventData::ProviderAttemptCompleted {
        response_ref,
        served_attribution_ref,
        ..
    } = &events.last().unwrap().body.data
    else {
        panic!()
    };
    let response: ProviderResponsePayload = h.payload(response_ref);
    let finish: serde_json::Value = h.payload(response.finish_evidence_ref.as_ref().unwrap());
    println!(
        "HTTP_STRING {backend} text={text} blocks={} reasoning_absence={:?} usage={:?} finish={finish} served={served_attribution_ref:?}",
        response.blocks.len(),
        response.reasoning_absence,
        response.usage_ref
    );
    assert_eq!(response.blocks.len(), 3);
    assert!(response.reasoning_absence.is_none());
    assert!(response.usage_ref.is_some());
    assert!(
        response
            .blocks
            .iter()
            .any(|b| matches!(b, ResponseBlock::Media { .. }))
    );
    // Both API projections preserve actual wire attribution.
    let h2 = Harness::new(backend);
    let messages = [ChatMessage::user("fixture")];
    scope_provider_capture(
        h2.admission.clone(),
        ProviderDispatch::from_ref(&provider).chat(
            ChatRequest {
                messages: &messages,
                tools: None,
                thinking: None,
            },
            "requested-model",
            None,
        ),
    )
    .await
    .unwrap();
    let events = h2.events();
    let EventData::ProviderAttemptCompleted {
        response_ref,
        served_attribution_ref,
        ..
    } = &events.last().unwrap().body.data
    else {
        panic!()
    };
    let response: ProviderResponsePayload = h2.payload(response_ref);
    println!(
        "HTTP_STRUCTURED {backend} blocks={} usage={} served={served_attribution_ref:?}",
        response.blocks.len(),
        response.usage_ref.is_some()
    );
    assert_eq!(
        h2.payload::<serde_json::Value>(served_attribution_ref.as_ref().unwrap())["model"],
        "actual-served-model"
    );
    assert!(
        response
            .blocks
            .iter()
            .any(|b| matches!(b, ResponseBlock::Media { .. }))
    );
    server.abort();
}
struct ReplayLeaf;
impl Attributable for ReplayLeaf {
    fn role(&self) -> Role {
        Role::System
    }
    fn alias(&self) -> &str {
        "replay-fixture"
    }
}
#[async_trait::async_trait]
impl ModelProvider for ReplayLeaf {
    async fn chat_with_system(
        &self,
        _: Option<&str>,
        _: &str,
        _: &str,
        _: Option<f64>,
    ) -> anyhow::Result<String> {
        Ok("answer".into())
    }
    async fn chat(
        &self,
        _: ChatRequest<'_>,
        _: &str,
        _: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        Ok(ChatResponse {
            text: Some("answer".into()),
            tool_calls: vec![],
            reasoning_content: None,
            usage: None,
            output_blocks: vec![ProviderOutputBlock::Other {
                type_name: "gemini.part_replay".into(),
                content: serde_json::json!({"text":"visible answer","thoughtSignature":"opaque-signature-fixture"}),
            }],
        })
    }
}
async fn audit_replay_disclosure(backend: &str) {
    use zeroclaw_api::principal::PrincipalId;
    use zeroclaw_kernel::session::query::PayloadSelector;
    let h = Harness::new(backend);
    let messages = [ChatMessage::user("fixture")];
    scope_provider_capture(
        h.admission.clone(),
        ProviderDispatch::from_ref(&ReplayLeaf).chat(
            ChatRequest {
                messages: &messages,
                tools: None,
                thinking: None,
            },
            "fixture",
            None,
        ),
    )
    .await
    .unwrap();
    let events = h.events();
    let completed = events.last().unwrap();
    let owner = Arc::new(SessionCoordinator::new(h.store.clone()));
    let query = owner.query(PrincipalId::shared_operator(), Arc::new(ReadAllow));
    let read = query
        .read_payload(
            h.stream.clone(),
            PayloadSelector {
                event_sequence: completed.sequence,
                reference_path: vec![0, 0],
            },
            1024 * 1024,
        )
        .await;
    assert!(matches!(read, Err(SessionError::PermissionDenied(_))));
}
struct ChildAuthority;
impl zeroclaw_kernel::session::query::SessionAccessPolicy for ChildAuthority {
    fn authorize(
        &self,
        _: &zeroclaw_api::principal::PrincipalId,
        _: &SessionStreamId,
        _: zeroclaw_kernel::session::query::SessionAccess,
    ) -> Result<(), SessionError> {
        Ok(())
    }
}
impl PermissionResolver for ChildAuthority {
    fn authorize(&self, _: &EffectIntent) -> Result<(), SessionError> {
        Ok(())
    }
    fn authorize_context(&self, _: &SessionStreamId, _: &BranchId) -> Result<(), SessionError> {
        Ok(())
    }
    fn child_authority(
        &self,
        _: &str,
        _: &SessionStreamId,
    ) -> Result<Arc<dyn zeroclaw_kernel::session::engine::EngineSessionAuthority>, SessionError>
    {
        Ok(Arc::new(ChildAuthority))
    }
}
impl zeroclaw_kernel::session::engine::EngineSessionAuthority for ChildAuthority {
    fn execution_binding(
        &self,
        _: &zeroclaw_api::session_events::capture::EngineTurnInput,
    ) -> Result<PayloadRecord, SessionError> {
        self.maintenance_binding()
    }
    fn maintenance_binding(&self) -> Result<PayloadRecord, SessionError> {
        Ok(PayloadRecord::json(
            "session.execution-binding/v1",
            &serde_json::json!({"source":"fixture"}),
        )
        .unwrap())
    }
    fn diagnostic(&self, e: &anyhow::Error) -> Option<String> {
        Some(e.to_string())
    }
}

#[tokio::test]
async fn cancelled_provider_attempts_retain_durable_partial_and_never_retry() {
    use futures_util::StreamExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use zeroclaw_api::session_events::capture::*;
    use zeroclaw_providers::compatible::{AuthStyle, OpenAiCompatibleModelProvider};

    for backend in ["sqlite", "jsonl"] {
        for (partial, explicit_request, summary) in [
            (false, false, false),
            (true, true, false),
            (true, false, true),
        ] {
            let h = Harness::new(backend);
            let owner = Arc::new(SessionCoordinator::new(h.store.clone()));
            let session = zeroclaw_kernel::session::engine::EngineSession::new(
                owner,
                h.stream.clone(),
                h.events()[0].branch_id.clone().unwrap(),
                zeroclaw_api::principal::PrincipalId::shared_operator(),
                Arc::new(ChildAuthority),
            );
            let calls = Arc::new(AtomicUsize::new(0));
            let ready = Arc::new(tokio::sync::Notify::new());
            let server_calls = calls.clone();
            let server_ready = ready.clone();
            let app = axum::Router::new().route(
                "/chat/completions",
                axum::routing::post(move || {
                    let calls = server_calls.clone();
                    let ready = server_ready.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        if !partial {
                            ready.notify_one();
                        }
                        let frames = if partial {
                            vec![Ok::<_, std::convert::Infallible>(
                                "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"durable cancellation thought\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"durable cancellation answer\"}}]}\n\n",
                            )]
                        } else {
                            Vec::new()
                        };
                        let body = axum::body::Body::from_stream(
                            futures_util::stream::iter(frames).chain(futures_util::stream::pending()),
                        );
                        axum::response::Response::builder()
                            .header("content-type", "text/event-stream")
                            .body(body)
                            .unwrap()
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = zeroclaw_spawn::spawn!(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let provider = OpenAiCompatibleModelProvider::builder("cancel-fixture")
                .display_name("cancel-fixture")
                .base_url(&format!("http://{address}"))
                .auth_style(AuthStyle::Bearer)
                .build();
            let input = EngineTurnInput {
                execution_workspace: None,
                turn_id: TurnId::new().to_string(),
                agent_alias: Some("fixture".into()),
                ingress: zeroclaw_api::ingress::IngressContext::agent_direct(),
                reply_route: None,
                parts: vec![EngineInputPart::Text("cancellation input".into())],
                provider_ref: "fixture".into(),
                model: "fixture".into(),
            };
            let work = async {
                let messages = [ChatMessage::user("fixture")];
                if explicit_request {
                    ProviderDispatch::from_ref(&Mixed)
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
                }
                let call = async {
                    let mut stream = ProviderDispatch::from_ref(&provider).stream_chat(
                        ChatRequest {
                            messages: &messages,
                            tools: None,
                            thinking: None,
                        },
                        "fixture",
                        None,
                        StreamOptions::new(true),
                    );
                    while let Some(event) = stream.next().await {
                        // The earlier reasoning frame is durable before this text is
                        // published; its UI representation need not be ThinkingDelta.
                        if partial
                            && matches!(event?, StreamEvent::TextDelta(chunk) if !chunk.delta.is_empty())
                        {
                            ready.notify_one();
                        }
                    }
                    anyhow::bail!("fixture provider must remain pending until cancellation")
                };
                tokio::select! {
                    result = call => result,
                    () = ready.notified() => {
                        if explicit_request {
                            session.request_cancellation(Some(&input.turn_id), "fixture-live-owner", "fixture_operator_cancel").await?;
                        }
                        Err::<String, anyhow::Error>(zeroclaw_api::turn_stop::ToolLoopCancelled.into())
                    }
                }
            };
            let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                if summary {
                    session
                        .summarize_context(ProviderPurpose::Summary, Box::new(move |_source| Box::pin(work)))
                        .await.map(|_| String::new())
                } else {
                    session.execute(input.clone(), Box::pin(work)).await
                }
            })
            .await
            .unwrap_or_else(|error| {
                panic!("bounded provider cancellation backend={backend} partial={partial} explicit_request={explicit_request} summary={summary}: {error}; durable events={:?}", h.events().iter().map(|event| event.body.kind()).collect::<Vec<_>>())
            });
            assert!(zeroclaw_api::turn_stop::is_tool_loop_cancelled(
                &result.expect_err("cancelled")
            ));
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            let events = h.events();
            let started: Vec<_> = events
                .iter()
                .filter(|event| event.body.kind() == EventKind::ProviderAttemptStarted)
                .collect();
            assert_eq!(started.len(), if explicit_request { 2 } else { 1 });
            let invocation = &started.last().unwrap().invocation_id;
            if explicit_request {
                let finished = &started[0].invocation_id;
                assert_eq!(
                    events
                        .iter()
                        .filter(|event| &event.invocation_id == finished
                            && zeroclaw_api::session_events::rules::terminal(&event.body.kind()))
                        .count(),
                    1
                );
                assert!(events.iter().any(|event| &event.invocation_id == finished
                    && event.body.kind() == EventKind::ProviderAttemptCompleted));
            }
            let observations: Vec<_> = events
                .iter()
                .filter_map(|event| {
                    let EventData::ProviderResponseProgressObserved { observation_ref } =
                        &event.body.data
                    else {
                        return None;
                    };
                    let payload: ProviderObservationPayload = h.payload(observation_ref);
                    payload
                        .operations
                        .into_iter()
                        .find_map(|operation| match operation {
                            ProviderObservation::CancellationObserved {
                                request_event_id,
                                stage,
                            } => Some((event, request_event_id, stage)),
                            _ => None,
                        })
                })
                .collect();
            assert_eq!(observations.len(), 1);
            let (observation, request_id, stage) = &observations[0];
            assert_eq!(&observation.invocation_id, invocation);
            assert_eq!(
                *stage,
                if partial {
                    DiagnosticStage::ConsumeStream
                } else {
                    DiagnosticStage::AwaitResponse
                }
            );
            let request = events
                .iter()
                .find(|event| &event.event_id == request_id)
                .expect("real cancellation request");
            assert_eq!(
                events
                    .iter()
                    .filter(|event| event.body.kind() == EventKind::CancellationRequested)
                    .count(),
                1
            );
            let EventData::CancellationRequested { reason_key, .. } = &request.body.data else {
                panic!("cancellation target must be a request")
            };
            assert_eq!(
                reason_key,
                if explicit_request {
                    "fixture_operator_cancel"
                } else {
                    "executor_cooperative_stop_observed"
                }
            );
            if !explicit_request {
                assert!(
                    matches!(&request.actor, EventActor::System { component } if component == "execution_settlement")
                );
            }
            assert!(request.sequence < observation.sequence);
            let terminals: Vec<_> = events
                .iter()
                .filter(|event| {
                    &event.invocation_id == invocation
                        && zeroclaw_api::session_events::rules::terminal(&event.body.kind())
                })
                .collect();
            assert_eq!(terminals.len(), 1);
            let EventData::InvocationOutcomeUnknown {
                last_observed_event_id,
                reason_key,
                observed_partial_ref,
                ..
            } = &terminals[0].body.data
            else {
                panic!("remote outcome remains unknown")
            };
            assert_eq!(last_observed_event_id, &observation.event_id);
            assert_eq!(reason_key, "cancelled_remote_outcome_unconfirmed");
            assert_eq!(observed_partial_ref.is_some(), partial);
            assert!(observation.sequence < terminals[0].sequence);
            assert!(terminals[0].sequence < events.last().unwrap().sequence);
            assert_eq!(
                events.last().unwrap().body.kind(),
                if summary {
                    EventKind::ContextTransformCancelled
                } else {
                    EventKind::TurnCancelled
                }
            );
            if partial {
                let mut durable = String::new();
                for event in &events {
                    if let EventData::ProviderResponseProgressObserved { observation_ref } =
                        &event.body.data
                    {
                        let payload: ProviderObservationPayload = h.payload(observation_ref);
                        for operation in payload.operations {
                            let bytes_ref = match operation {
                                ProviderObservation::BytesAppended { bytes_ref, .. } => {
                                    Some(bytes_ref)
                                }
                                ProviderObservation::ProtocolSegmentObserved {
                                    segment_ref,
                                    ..
                                } => {
                                    let block: ResponseBlock = h.payload(&segment_ref);
                                    match block {
                                        ResponseBlock::Text { text_ref } => Some(text_ref),
                                        ResponseBlock::Reasoning { payload_ref, .. } => {
                                            Some(payload_ref)
                                        }
                                        _ => None,
                                    }
                                }
                                _ => None,
                            };
                            if let Some(bytes_ref) = bytes_ref {
                                let PayloadRead::Present(record) =
                                    h.store.read_payload(&bytes_ref.payload_id).unwrap()
                                else {
                                    panic!("durable partial missing")
                                };
                                durable.push_str(std::str::from_utf8(&record.body).unwrap());
                            }
                        }
                    }
                }
                assert!(durable.contains("durable cancellation thought"));
                assert!(durable.contains("durable cancellation answer"));
            }
            if !summary {
                assert!(
                    session
                        .execute(
                            input.clone(),
                            Box::pin(async { panic!("cancelled input must not dispatch again") })
                        )
                        .await
                        .is_err()
                );
                assert_eq!(h.events().len(), events.len());
            }
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            server.abort();
            assert!(server.await.expect_err("server aborted").is_cancelled());
            let reopened =
                open_event_store(h._dir.path(), backend).expect("reopen committed cancellation");
            let replay = reopened
                .read_page(
                    &h.stream,
                    &EventCursor::beginning(h.stream.clone()),
                    None,
                    1000,
                    4 * 1024 * 1024,
                )
                .expect("replay");
            assert_eq!(
                replay
                    .events
                    .iter()
                    .map(|event| &event.event_id)
                    .collect::<Vec<_>>(),
                events
                    .iter()
                    .map(|event| &event.event_id)
                    .collect::<Vec<_>>()
            );
            assert_eq!(calls.load(Ordering::SeqCst), 1, "replay cannot dispatch");
        }
    }
}

async fn audit_late_child_failure(backend: &str) {
    use zeroclaw_api::principal::PrincipalId;
    use zeroclaw_api::session_events::capture::*;
    let h = Harness::new(backend);
    let events = h.events();
    let owner = Arc::new(SessionCoordinator::new(h.store.clone()));
    let session = zeroclaw_kernel::session::engine::EngineSession::new(
        owner.clone(),
        h.stream.clone(),
        events[0].branch_id.clone().unwrap(),
        PrincipalId::shared_operator(),
        Arc::new(ChildAuthority),
    );
    let input = EngineTurnInput {
        execution_workspace: None,
        reply_route: None,
        turn_id: TurnId::new().to_string(),
        agent_alias: Some("fixture".into()),
        ingress: zeroclaw_api::ingress::IngressContext::sub_turn(),
        parts: vec![EngineInputPart::Text("fixture".into())],
        provider_ref: "fixture".into(),
        model: "fixture".into(),
    };
    let (send, receive) = tokio::sync::oneshot::channel();
    session
        .execute(
            input,
            Box::pin(async move {
                let child = current_provider_admission()
                    .unwrap()
                    .begin_child(ChildDispatchIntent {
                        relation: "delegate".into(),
                        target_agent: "child".into(),
                        prompt: "child fixture".into(),
                        execution_mode: ChildExecutionMode::Sync,
                        task_id: None,
                    })
                    .await?;
                send.send(child)
                    .map_err(|_| anyhow::Error::msg("receiver lost"))?;
                Err(anyhow::Error::msg("parent stopped before child"))
            }),
        )
        .await
        .unwrap_err();
    let child = receive.await.unwrap();
    let result = child
        .execute(Box::pin(async {
            Err(anyhow::Error::msg("child definite failure"))
        }))
        .await;
    assert!(result.is_err());
    let events = h.events();
    for e in events
        .iter()
        .filter(|e| e.body.kind() == EventKind::OutcomeReconciled)
    {
        println!(
            "LATE_CHILD_FAILURE {backend} result={result:?} recorded={:?}",
            e.body.data
        );
        assert!(matches!(
            &e.body.data,
            EventData::OutcomeReconciled {
                resolution: Reconciliation::Failed { .. },
                ..
            }
        ));
    }
    assert!(
        events
            .iter()
            .any(|e| e.body.kind() == EventKind::OutcomeReconciled)
    );
}
async fn audit_request_reasoning_disclosure(backend: &str) {
    use zeroclaw_api::principal::PrincipalId;
    use zeroclaw_kernel::session::query::PayloadSelector;
    let h = Harness::new(backend);
    let messages = [
        ChatMessage::assistant(
            r#"{"tool_calls":[],"reasoning_content":"previous signed reasoning fixture"}"#,
        ),
        ChatMessage::user("fixture"),
    ];
    scope_provider_capture(
        h.admission.clone(),
        ProviderDispatch::from_ref(&Mixed).chat(
            ChatRequest {
                messages: &messages,
                tools: None,
                thinking: None,
            },
            "fixture",
            None,
        ),
    )
    .await
    .unwrap();
    let events = h.events();
    let prepared = events
        .iter()
        .find(|e| e.body.kind() == EventKind::ProviderRequestPrepared)
        .unwrap();
    let owner = Arc::new(SessionCoordinator::new(h.store.clone()));
    let read = owner
        .query(PrincipalId::shared_operator(), Arc::new(ReadAllow))
        .read_payload(
            h.stream.clone(),
            PayloadSelector {
                event_sequence: prepared.sequence,
                reference_path: vec![0, 0],
            },
            1024 * 1024,
        )
        .await;
    assert!(matches!(read, Err(SessionError::PermissionDenied(_))));
}

#[tokio::test]
async fn summary_policy_retains_system_context_and_captures_the_assembled_request() {
    use zeroclaw_api::principal::PrincipalId;
    use zeroclaw_api::session_events::capture::{ContextSnapshot, SessionExecutionPort};
    for backend in ["sqlite", "jsonl"] {
        let h = Harness::new(backend);
        let events = h.events();
        let session = zeroclaw_kernel::session::engine::EngineSession::new(
            Arc::new(SessionCoordinator::new(h.store.clone())),
            h.stream.clone(),
            events[0].branch_id.clone().unwrap(),
            PrincipalId::shared_operator(),
            Arc::new(ChildAuthority),
        );
        session
            .replace_context(
                ContextSnapshot {
                    messages: vec![
                        ConversationMessage::Chat(ChatMessage::system("keep these instructions")),
                        ConversationMessage::Chat(ChatMessage::user("summarize this request")),
                        ConversationMessage::Chat(ChatMessage::assistant("original response")),
                    ],
                    history_has_trim_breadcrumb: Some(false),
                },
                "fixture_summary_seed",
            )
            .await
            .expect("seed context");
        let installed = session
            .summarize_context(
                ProviderPurpose::Summary,
                Box::new(|conversation| {
                    Box::pin(async move {
                        let messages: Vec<_> = conversation
                            .into_iter()
                            .map(|message| match message {
                                ConversationMessage::Chat(chat) => chat,
                                _ => panic!("fixture only has chat messages"),
                            })
                            .collect();
                        assert_eq!(messages.len(), 4);
                        assert_eq!(messages[0].content, "keep these instructions");
                        assert_eq!(messages[1].content, "summarize this request");
                        assert_eq!(messages[2].content, "original response");
                        assert_eq!(messages[3].role, "user");
                        assert!(
                            messages[3]
                                .content
                                .starts_with("Summarize the conversation for continuation.")
                        );
                        let response = ProviderDispatch::from_ref(&Mixed)
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
                        Ok(response.text_or_empty().to_owned())
                    })
                }),
            )
            .await
            .expect("summary committed");
        let messages = installed.chat_projection();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "keep these instructions");
        assert_eq!(messages[1].content, "[Context summary]\nanswer");
        assert_eq!(installed.history_has_trim_breadcrumb, Some(true));
        let events = h.events();
        let request = events
            .iter()
            .find_map(|event| match &event.body.data {
                EventData::ProviderRequestPrepared { request_ref, .. } => {
                    Some(h.payload::<serde_json::Value>(request_ref))
                }
                _ => None,
            })
            .expect("captured request");
        let messages_ref: StoredPayloadRef =
            serde_json::from_value(request["messages_ref"].clone()).expect("messages reference");
        let messages: Vec<ChatMessage> = h.payload(&messages_ref);
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].content, "keep these instructions");
        assert!(
            messages[3]
                .content
                .starts_with("Summarize the conversation for continuation.")
        );
    }
}

async fn audit_summary(backend: &str) {
    use zeroclaw_api::principal::PrincipalId;
    use zeroclaw_api::session_events::capture::*;
    let h = Harness::new(backend);
    let events = h.events();
    let owner = Arc::new(SessionCoordinator::new(h.store.clone()));
    let session = zeroclaw_kernel::session::engine::EngineSession::new(
        owner.clone(),
        h.stream.clone(),
        events[0].branch_id.clone().unwrap(),
        PrincipalId::shared_operator(),
        Arc::new(ChildAuthority),
    );
    session.append_assistant("old context").await.unwrap();
    let generate = async {
        let messages = [ChatMessage::user("summarize")];
        let response = ProviderDispatch::from_ref(&Mixed)
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
        Ok::<String, anyhow::Error>(response.text_or_empty().to_owned())
    };
    assert_eq!(
        session
            .summarize_context(
                ProviderPurpose::Summary,
                Box::new(move |source| Box::pin(async move {
                    assert_eq!(
                        match &source[source.len() - 2] {
                            zeroclaw_api::model_provider::ConversationMessage::Chat(chat) =>
                                &chat.content,
                            _ => panic!("chat input"),
                        },
                        "old context"
                    );
                    generate.await
                }))
            )
            .await
            .unwrap()
            .chat_projection()[0]
            .content,
        "[Context summary]\nanswer"
    );
    let restored = session.restore_context().await.unwrap().unwrap();
    assert_eq!(restored.messages.len(), 1);
    assert_eq!(
        restored.chat_projection()[0].content,
        "[Context summary]\nanswer"
    );
    let events = h.events();
    let projection = events.last().unwrap();
    let EventData::ContextProjectionCreated {
        summary_ref,
        summary_source_refs,
        transform_started_event_id,
        source_ranges,
        ..
    } = &projection.body.data
    else {
        panic!("summary projection");
    };
    assert!(summary_ref.is_some());
    assert_eq!(summary_source_refs.len(), 1);
    assert!(
        source_ranges
            .iter()
            .any(|r| r.disposition == RangeDisposition::Summarized)
    );
    let prepared = events
        .iter()
        .rev()
        .find(|e| {
            matches!(
                &e.body.data,
                EventData::ProviderRequestPrepared {
                    purpose: ProviderPurpose::Summary,
                    ..
                }
            )
        })
        .unwrap();
    assert_eq!(prepared.causation_id, *transform_started_event_id);
    assert_eq!(
        events
            .iter()
            .filter(|e| e.body.kind() == EventKind::ProviderAttemptCompleted)
            .count(),
        1
    );
    let completed = events
        .iter()
        .find(|e| e.body.kind() == EventKind::ProviderAttemptCompleted)
        .expect("summary result");
    let EventData::ProviderAttemptCompleted { response_ref, .. } = &completed.body.data else {
        unreachable!()
    };
    let response: ProviderResponsePayload = h.payload(response_ref);
    assert!(response.usage_ref.is_some());
    assert!(
        response
            .blocks
            .iter()
            .any(|block| matches!(block, ResponseBlock::Reasoning { .. }))
    );

    let first_refs: serde_json::Value = match &projection.body.data {
        EventData::ContextProjectionCreated { result_ref, .. } => h.payload(result_ref),
        _ => unreachable!(),
    };
    session
        .replace_context(restored.clone(), "summary_preserving_noop")
        .await
        .expect("preserve summary provenance");
    let generated = async {
        let messages = [ChatMessage::user("summarize again")];
        let response = ProviderDispatch::from_ref(&Mixed)
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
        Ok::<String, anyhow::Error>(response.text_or_empty().to_owned())
    };
    session
        .summarize_context(
            ProviderPurpose::Summary,
            Box::new(move |_source| Box::pin(generated)),
        )
        .await
        .expect("second summary");
    let restored_again = session
        .restore_context()
        .await
        .expect("restore")
        .expect("context");
    assert_eq!(
        restored_again.messages.len(),
        1,
        "previous summary is replaced, not accumulated"
    );
    let second_projection = h.events().last().expect("projection").clone();
    let second_refs: serde_json::Value = match &second_projection.body.data {
        EventData::ContextProjectionCreated { result_ref, .. } => h.payload(result_ref),
        _ => unreachable!(),
    };
    assert_ne!(
        first_refs["message_refs"][0]["message_id"], second_refs["message_refs"][0]["message_id"],
        "even identical regenerated summaries create a new identity"
    );

    // A failed generation leaves the installed context unchanged and settles
    // the transform independently of its captured uncertain provider attempt.
    let failure = session
        .summarize_context(
            ProviderPurpose::ContextRepair,
            Box::new(move |_source| {
                Box::pin(async move { anyhow::bail!("fixture summary rejected") })
            }),
        )
        .await;
    assert!(failure.is_err());
    assert_eq!(
        session
            .restore_context()
            .await
            .unwrap()
            .unwrap()
            .chat_projection()[0]
            .content,
        "[Context summary]\nanswer"
    );
    assert_eq!(
        h.events().last().unwrap().body.kind(),
        EventKind::ContextTransformFailed
    );
    let cancellation = session
        .summarize_context(
            ProviderPurpose::Summary,
            Box::new(move |_source| {
                Box::pin(async move {
                    Err(anyhow::Error::new(
                        zeroclaw_api::turn_stop::ToolLoopCancelled,
                    ))
                })
            }),
        )
        .await;
    assert!(cancellation.is_err());
    assert_eq!(
        h.events().last().expect("cancelled transform").body.kind(),
        EventKind::ContextTransformCancelled
    );
    assert_eq!(
        session
            .restore_context()
            .await
            .expect("unchanged context")
            .expect("context")
            .chat_projection()[0]
            .content,
        "[Context summary]\nanswer"
    );
}

struct RetryOnce(std::sync::atomic::AtomicUsize);
impl Attributable for RetryOnce {
    fn role(&self) -> Role {
        Role::System
    }
    fn alias(&self) -> &str {
        "retry-fixture"
    }
}
#[async_trait::async_trait]
impl ModelProvider for RetryOnce {
    async fn chat_with_system(
        &self,
        _: Option<&str>,
        _: &str,
        _: &str,
        _: Option<f64>,
    ) -> anyhow::Result<String> {
        if self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            return Err(StreamError::HttpStatus {
                status: 503,
                message: "fixture unavailable".into(),
            }
            .into());
        }
        Ok("retried answer".into())
    }
}
#[tokio::test]
async fn retry_capture_preserves_requested_route_and_terminal_causality_both_backends() {
    for backend in ["sqlite", "jsonl"] {
        let h = Harness::new(backend);
        let provider = zeroclaw_providers::reliable::ReliableModelProvider::new(
            "requested-route",
            vec![(
                "physical-route".into(),
                Box::new(RetryOnce(std::sync::atomic::AtomicUsize::new(0))),
            )],
            1,
            50,
        );
        for _ in 0..2 {
            let messages = [ChatMessage::user("fixture")];
            let scope = zeroclaw_providers::dispatch::AccountedChatScope::new();
            let response = scope_provider_route_source(
                ProviderRouteSource::Routing,
                scope_provider_capture(
                    h.admission.clone(),
                    scope.scope(ProviderDispatch::from_ref(&provider).chat(
                        ChatRequest {
                            messages: &messages,
                            tools: None,
                            thinking: None,
                        },
                        "requested-model",
                        None,
                    )),
                ),
            )
            .await
            .expect("retry succeeds");
            assert_eq!(response.text_or_empty(), "retried answer");
        }
        let events = h.events();
        let starts: Vec<_> = events
            .iter()
            .filter(|e| e.body.kind() == EventKind::ProviderAttemptStarted)
            .collect();
        assert_eq!(starts.len(), 3);
        assert!(matches!(
            &starts[0].body.data,
            EventData::ProviderAttemptStarted {
                previous_attempt_event_id: None,
                ..
            }
        ));
        let failure = events
            .iter()
            .find(|e| e.body.kind() == EventKind::ProviderAttemptFailed)
            .expect("first failure retained");
        assert!(
            matches!(&starts[1].body.data,EventData::ProviderAttemptStarted {previous_attempt_event_id:Some(id),..} if id==&failure.event_id)
        );
        assert!(
            matches!(
                &starts[2].body.data,
                EventData::ProviderAttemptStarted {
                    previous_attempt_event_id: None,
                    ..
                }
            ),
            "independent calls do not inherit retry causes"
        );
        for event in events
            .iter()
            .filter(|e| e.body.kind() == EventKind::ProviderRequestPrepared)
        {
            let EventData::ProviderRequestPrepared {
                route_attribution_ref,
                ..
            } = &event.body.data
            else {
                unreachable!()
            };
            let route: serde_json::Value = h.payload(route_attribution_ref);
            assert_eq!(route["requested"]["provider_ref"], "requested-route");
            assert_eq!(route["dispatch_source"], "routing");
            assert!(route["selection_event_id"].is_null());
            assert_eq!(route["actual_dispatch"]["provider_ref"], "physical-route");
        }
    }
}

#[tokio::test]
async fn codex_done_only_and_mixed_parts_preserve_order_both_backends() {
    use futures_util::StreamExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    for backend in ["sqlite", "jsonl"] {
        for mode in [
            "item_done",
            "terminal_only",
            "mixed_items",
            "mixed_parts",
            "text_done",
            "interleaved_delta",
            "split_delta",
            "credential_delta",
            "unknown_parts",
            "duplicate_done",
            "unidentified_items",
            "json",
        ] {
            for streaming in [false, true] {
                if mode == "json" && streaming {
                    continue;
                }
                let h = Harness::new(backend);
                let calls = Arc::new(AtomicUsize::new(0));
                let seen = calls.clone();
                let app = axum::Router::new().route("/responses", axum::routing::post(move || {
                    let seen = seen.clone();
                    async move {
                        seen.fetch_add(1, Ordering::SeqCst);
                        let mut message = serde_json::json!({"type":"message","id":"message-main","content":[
                            {"type":"output_text","text":"A"},
                            {"type":"output_audio","data":"AQID","format":"wav"},
                            {"type":"output_text","text":"B"},
                            {"type":"output_image","result":"BAUG","format":"png"},
                            {"type":"output_text","text":"C"}
                        ]});
                        if mode == "split_delta" { message["content"][0]["text"] = serde_json::json!("Aa"); }
                        if mode == "credential_delta" { message["content"][0]["text"] = serde_json::json!("token=fixture-secretA tail"); }
                        if mode == "unknown_parts" {message["content"].as_array_mut().expect("parts").insert(1,serde_json::json!({"type":"future_part","future":"preserved"}));}
                        if mode == "unidentified_items" {message.as_object_mut().expect("message").remove("id");}
                        let prior = serde_json::json!({"type":"message","id":"message-prior","content":[{"type":"output_text","text":"P"}]});
                        let reasoning = serde_json::json!({"type":"reasoning","id":"reasoning-original","summary":[{"type":"summary_text","text":"inspect"}],"encrypted_content":"opaque-synthetic"});
                        let tool = serde_json::json!({"type":"function_call","id":"item-tool","call_id":"call-fixture","name":"fixture_tool","arguments":"{}"});
                        let output = match mode {
                            "mixed_items" => vec![prior.clone(),message.clone()],
                            "duplicate_done" => {
                                let mut final_reasoning = reasoning.clone();
                                final_reasoning["id"] = serde_json::json!("reasoning-final");
                                vec![final_reasoning,message.clone(),tool.clone()]
                            },
                            "unidentified_items" => {
                                let mut final_message = message.clone();
                                final_message["id"] = serde_json::json!("terminal-message-id");
                                vec![message.clone(),final_message]
                            },
                            _ => vec![message.clone()],
                        };
                        let response = serde_json::json!({"model":"served-fixture","status":"completed","output":output,"usage":{"input_tokens":7,"output_tokens":9}});
                        let (content_type, body) = if mode == "json" {
                            ("application/json", response.to_string())
                        } else {
                            let mut frames = Vec::new();
                            if mode == "mixed_items" {
                                frames.push(serde_json::json!({"type":"response.output_text.delta","item_id":"message-prior","output_index":0,"content_index":0,"delta":"P"}));
                                frames.push(serde_json::json!({"type":"response.output_item.done","output_index":0,"item":prior}));
                            }
                            if mode == "duplicate_done" {
                                for _ in 0..2 { frames.push(serde_json::json!({"type":"response.output_item.done","output_index":2,"item":tool})); }
                                frames.push(serde_json::json!({"type":"response.reasoning_summary_text.delta","item_id":"reasoning-original","output_index":0,"summary_index":0,"delta":"inspect"}));
                                for _ in 0..2 { frames.push(serde_json::json!({"type":"response.output_item.done","output_index":0,"item":reasoning})); }
                            }
                            if mode == "unidentified_items" {
                                frames.push(serde_json::json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"A"}));
                            }
                            if matches!(mode,"mixed_parts"|"interleaved_delta"|"split_delta") {
                                frames.push(serde_json::json!({"type":"response.output_text.delta","item_id":"message-main","output_index":0,"content_index":0,"delta":"A"}));
                            }
                            if mode == "interleaved_delta" {
                                for (content_index, delta) in [(2,"B"),(4,"C")] {
                                    frames.push(serde_json::json!({"type":"response.output_text.delta","item_id":"message-main","output_index":0,"content_index":content_index,"delta":delta}));
                                }
                            }
                            if mode == "split_delta" {frames.push(serde_json::json!({"type":"response.output_text.delta","item_id":"message-main","output_index":0,"content_index":0,"delta":"a"}));}
                            if mode == "credential_delta" {
                                for delta in ["token=fixture-secretA"," tail"] {
                                    frames.push(serde_json::json!({"type":"response.output_text.delta","item_id":"message-main","output_index":0,"content_index":0,"delta":delta}));
                                }
                            }
                            if mode == "text_done" {
                                frames.push(serde_json::json!({"type":"response.output_text.done","item_id":"message-main","output_index":0,"content_index":0,"text":"A"}));
                            }
                            if mode != "terminal_only" {
                                frames.push(serde_json::json!({"type":"response.output_item.done","output_index":if matches!(mode,"mixed_items"|"duplicate_done") {1} else {0},"item":message}));
                            }
                            if mode == "duplicate_done" {
                                frames.push(serde_json::json!({"type":"response.output_item.done","output_index":1,"item":message}));
                            }
                            if mode == "unidentified_items" {
                                for output_index in [0,1] { frames.push(serde_json::json!({"type":"response.output_item.done","output_index":output_index,"item":message})); }
                            }
                            frames.push(serde_json::json!({"type":"response.completed","response":response}));
                            ("text/event-stream", frames.into_iter().map(|frame| format!("data: {frame}\n\n")).collect::<String>())
                        };
                        axum::response::Response::builder().header("content-type", content_type).body(axum::body::Body::from(body)).expect("response")
                    }
                }));
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("listener");
                let address = listener.local_addr().expect("address");
                let server = zeroclaw_spawn::spawn!(async move {
                    axum::serve(listener, app).await.expect("server");
                });
                let provider = zeroclaw_providers::openai_codex::OpenAiCodexModelProvider::new(
                    "fixture",
                    &zeroclaw_providers::ModelProviderRuntimeOptions {
                        provider_api_url: Some(format!("http://{address}")),
                        zeroclaw_dir: Some(h._dir.path().to_path_buf()),
                        secrets_encrypt: false,
                        ..Default::default()
                    },
                    Some("fixture-key"),
                )
                .expect("provider");
                let messages = [ChatMessage::user("fixture")];
                let text = scope_provider_capture(h.admission.clone(), async {
                    let dispatch = ProviderDispatch::from_ref(&provider);
                    let request = ChatRequest {
                        messages: &messages,
                        tools: None,
                        thinking: None,
                    };
                    if streaming {
                        let mut stream = dispatch.stream_chat(
                            request,
                            "fixture",
                            None,
                            StreamOptions::new(true),
                        );
                        let mut text = String::new();
                        while let Some(event) = stream.next().await {
                            if let StreamEvent::TextDelta(chunk) = event.expect("stream event") {
                                text.push_str(&chunk.delta);
                            }
                        }
                        text
                    } else {
                        dispatch
                            .chat(request, "fixture", None)
                            .await
                            .expect("chat")
                            .text
                            .expect("text")
                    }
                })
                .await;
                assert_eq!(
                    text,
                    match mode {
                        "mixed_items" => "PABC",
                        "split_delta" => "AaBC",
                        "credential_delta" => "token=fixture-secretA tailBC",
                        "unidentified_items" => "ABCABC",
                        _ => "ABC",
                    },
                    "{backend} {mode} streaming={streaming}"
                );
                assert_eq!(calls.load(Ordering::SeqCst), 1);
                let events = h.events();
                let completed: Vec<_> = events
                    .iter()
                    .filter_map(|event| match &event.body.data {
                        EventData::ProviderAttemptCompleted { response_ref, .. } => {
                            Some(response_ref)
                        }
                        _ => None,
                    })
                    .collect();
                assert_eq!(completed.len(), 1, "single provider terminal");
                let response: ProviderResponsePayload = h.payload(completed[0]);
                let bytes = |reference: &StoredPayloadRef| {
                    let PayloadRead::Present(record) = h
                        .store
                        .read_payload(&reference.payload_id)
                        .expect("payload")
                    else {
                        panic!("present");
                    };
                    record.body
                };
                let metadata: Vec<serde_json::Value> = response
                    .blocks
                    .iter()
                    .filter_map(|block| match block {
                        ResponseBlock::Other {
                            type_name,
                            classified_body_ref,
                        } if type_name == "provider.content_metadata" => {
                            Some(h.payload(classified_body_ref))
                        }
                        _ => None,
                    })
                    .collect();
                let expected_message_id = if mode == "unidentified_items" {
                    "terminal-message-id"
                } else {
                    "message-main"
                };
                assert!(
                    metadata.iter().any(|value| {
                        value["kind"] == "provider_item"
                            && value["type_name"] == "message"
                            && value["fields"]["id"] == expected_message_id
                    }),
                    "message identity survives capture: {backend} {mode} streaming={streaming}"
                );
                if mode == "mixed_items" {
                    assert!(
                        metadata
                            .iter()
                            .any(|value| { value["fields"]["id"] == "message-prior" })
                    );
                }
                if mode == "duplicate_done" {
                    assert!(metadata.iter().any(|value| {
                        value["kind"] == "content_metadata"
                            && value["fields"]["id"] == "item-tool"
                            && value["fields"]["call_id"] == "call-fixture"
                    }));
                }
                let projected: Vec<_> = semantic_blocks(h.store.as_ref(), &response.blocks)
                    .map(|block| match block {
                        ResponseBlock::Text { text_ref } => {
                            String::from_utf8(bytes(text_ref)).expect("text bytes")
                        }
                        ResponseBlock::Media {
                            received: MediaReceived::Captured { representation_ref },
                            ..
                        } => match bytes(representation_ref).as_slice() {
                            [1, 2, 3] => "audio".into(),
                            [4, 5, 6] => "image".into(),
                            other => panic!("unexpected media {other:?}"),
                        },
                        ResponseBlock::Other {
                            type_name,
                            classified_body_ref,
                        } if type_name == "future_part" => {
                            assert!(
                                String::from_utf8(bytes(classified_body_ref))
                                    .expect("unknown bytes")
                                    .contains("preserved")
                            );
                            "unknown".into()
                        }
                        ResponseBlock::Reasoning {
                            payload_ref,
                            completion,
                            ..
                        } => {
                            if *completion == ContentCompletion::Partial {
                                assert_eq!(bytes(payload_ref), b"inspect");
                                "thinking".into()
                            } else {
                                let full: serde_json::Value = h.payload(payload_ref);
                                assert_eq!(full["encrypted_content"], "opaque-synthetic");
                                assert_eq!(full["summary"][0]["text"], "inspect");
                                "reasoning".into()
                            }
                        }
                        ResponseBlock::ToolCall { tool_call_ref } => {
                            let call: serde_json::Value = h.payload(tool_call_ref);
                            assert_eq!(call["name"], "fixture_tool");
                            assert_eq!(call["provider_call_id"], "call-fixture");
                            "tool".into()
                        }
                        other => panic!("unexpected block {other:?}"),
                    })
                    .collect();
                let expected = match mode {
                    "mixed_items" => vec!["P", "A", "audio", "B", "image", "C"],
                    // Item metadata and part coordinates coexist, so the
                    // canonical capture retains actual observation order.
                    "interleaved_delta" => vec!["A", "B", "C", "audio", "image"],
                    "split_delta" => vec!["A", "a", "audio", "B", "image", "C"],
                    "credential_delta" => {
                        vec!["token=fixture-secretA", " tail", "audio", "B", "image", "C"]
                    }
                    "unknown_parts" => vec!["A", "unknown", "audio", "B", "image", "C"],
                    "duplicate_done" => vec![
                        "tool",
                        "thinking",
                        "reasoning",
                        "A",
                        "audio",
                        "B",
                        "image",
                        "C",
                    ],
                    "unidentified_items" => vec![
                        "A", "audio", "B", "image", "C", "A", "audio", "B", "image", "C",
                    ],
                    _ => vec!["A", "audio", "B", "image", "C"],
                };
                assert_eq!(
                    projected, expected,
                    "{backend} {mode} streaming={streaming}"
                );
                let mut offsets = std::collections::HashMap::new();
                let mut protocol_segments = 0;
                for event in &events {
                    if let EventData::ProviderResponseProgressObserved { observation_ref } =
                        &event.body.data
                    {
                        let observed: ProviderObservationPayload = h.payload(observation_ref);
                        for operation in observed.operations {
                            if let ProviderObservation::ProtocolSegmentObserved {
                                position,
                                component,
                                byte_offset,
                                segment_ref,
                            } = operation
                            {
                                let block: ResponseBlock = h.payload(&segment_ref);
                                let expected = offsets.entry((position, component)).or_insert(0);
                                assert_eq!(
                                    byte_offset, *expected,
                                    "canonical component offsets remain continuous after classification"
                                );
                                *expected += block.component().1.byte_len;
                                protocol_segments += 1;
                            }
                        }
                    }
                }
                if mode != "json" {
                    assert_eq!(
                        protocol_segments,
                        response.blocks.len(),
                        "every canonical segment retains its original coordinate"
                    );
                }
                let reopened = open_event_store(h._dir.path(), backend).expect("reopen capture");
                assert_eq!(
                    reopened.head(&h.stream).expect("reopened head"),
                    h.store.head(&h.stream).expect("original head")
                );
                let replay = reopened
                    .read_page(
                        &h.stream,
                        &EventCursor::beginning(h.stream.clone()),
                        None,
                        1000,
                        4 * 1024 * 1024,
                    )
                    .expect("reopened capture history");
                assert_eq!(replay.events.len(), events.len());
                let PayloadRead::Present(record) = reopened
                    .read_payload(&completed[0].payload_id)
                    .expect("reopened response")
                else {
                    panic!("response preserved");
                };
                let restored: ProviderResponsePayload =
                    serde_json::from_slice(&record.body).expect("reopened response structure");
                assert_eq!(restored, response);
                server.abort();
                assert!(server.await.expect_err("server aborted").is_cancelled());
            }
        }
    }
}

#[tokio::test]
async fn provider_observation_append_rejects_gaps_overlap_and_reordering_both_backends() {
    use zeroclaw_api::session_events::capture::{ProviderDispatchIntent, ProviderObservationPort};
    async fn observe(
        port: &dyn ProviderObservationPort,
        operations: Vec<ProviderObservation>,
        mut payloads: Vec<PayloadRecord>,
    ) -> anyhow::Result<AppendReceipt> {
        let record = PayloadRecord::json(
            "session.provider-observation/v1",
            &ProviderObservationPayload { operations },
        )
        .expect("observation");
        let reference = StoredPayloadRef::from(&record);
        payloads.push(record);
        port.observe(
            EventData::ProviderResponseProgressObserved {
                observation_ref: reference,
            },
            payloads,
        )
        .await
    }
    fn segment(text: &str) -> (ResponseBlock, StoredPayloadRef, Vec<PayloadRecord>) {
        let bytes = PayloadRecord::new(
            PayloadId::new(),
            "session.text/v1".into(),
            text.as_bytes().to_vec(),
        )
        .expect("text");
        let block = ResponseBlock::Text {
            text_ref: StoredPayloadRef::from(&bytes),
        };
        let descriptor = PayloadRecord::json("session.evidence/v1", &block).expect("descriptor");
        (
            block,
            StoredPayloadRef::from(&descriptor),
            vec![bytes, descriptor],
        )
    }
    for backend in ["sqlite", "jsonl"] {
        for coordinates in ["legacy", "indexed", "mixed"] {
            let positioned = coordinates != "legacy";
            let h = Harness::new(backend);
            let port = h
                .admission
                .admit(ProviderDispatchIntent {
                    requested_route: None,
                    previous_attempt_event_id: None,
                    provider_ref: "fixture".into(),
                    model: "fixture".into(),
                    messages: vec![],
                    tools: None,
                    temperature: None,
                    thinking: None,
                    tool_round_policy: ToolRoundPolicy::Batch,
                })
                .await
                .expect("admit");
            let (first, first_ref, first_payloads) = segment("abc");
            let (second, second_ref, second_payloads) = segment("defg");
            let position = ProviderContentPosition {
                output_index: 0,
                content_index: Some(0),
            };
            let invalid = if positioned {
                vec![ProviderObservation::ProtocolSegmentObserved {
                    position: position.clone(),
                    component: "text".into(),
                    byte_offset: 1,
                    segment_ref: first_ref.clone(),
                }]
            } else {
                vec![ProviderObservation::BytesAppended {
                    block_index: 0,
                    component: "text".into(),
                    byte_offset: 0,
                    bytes_ref: first.component().1.clone(),
                }]
            };
            let before = h.store.head(&h.stream).expect("head");
            assert!(
                observe(port.as_ref(), invalid, first_payloads.clone())
                    .await
                    .is_err()
            );
            assert_eq!(h.store.head(&h.stream).expect("unchanged head"), before);
            if !positioned {
                assert!(
                    observe(
                        port.as_ref(),
                        vec![ProviderObservation::BlockFinalized {
                            block_index: 0,
                            finalized_block_ref: first_ref.clone()
                        }],
                        first_payloads.clone()
                    )
                    .await
                    .is_err()
                );
            }
            let first_ops = if positioned {
                vec![ProviderObservation::ProtocolSegmentObserved {
                    position: position.clone(),
                    component: "text".into(),
                    byte_offset: 0,
                    segment_ref: first_ref.clone(),
                }]
            } else {
                vec![
                    ProviderObservation::BlockOpened {
                        block_index: 0,
                        descriptor_ref: first_ref.clone(),
                    },
                    ProviderObservation::BytesAppended {
                        block_index: 0,
                        component: "text".into(),
                        byte_offset: 0,
                        bytes_ref: first.component().1.clone(),
                    },
                    ProviderObservation::BlockFinalized {
                        block_index: 0,
                        finalized_block_ref: first_ref.clone(),
                    },
                ]
            };
            observe(port.as_ref(), first_ops, first_payloads)
                .await
                .expect("first segment");
            if !positioned {
                for invalid in [
                    ProviderObservation::BlockOpened {
                        block_index: 0,
                        descriptor_ref: first_ref.clone(),
                    },
                    ProviderObservation::BlockFinalized {
                        block_index: 0,
                        finalized_block_ref: first_ref.clone(),
                    },
                    ProviderObservation::BytesAppended {
                        block_index: 0,
                        component: "text".into(),
                        byte_offset: 3,
                        bytes_ref: second.component().1.clone(),
                    },
                ] {
                    assert!(
                        observe(port.as_ref(), vec![invalid], second_payloads.clone())
                            .await
                            .is_err()
                    );
                }
            }
            for offset in if positioned { vec![0, 4] } else { vec![1, 2] } {
                let invalid = if positioned {
                    vec![ProviderObservation::ProtocolSegmentObserved {
                        position: position.clone(),
                        component: "text".into(),
                        byte_offset: offset,
                        segment_ref: second_ref.clone(),
                    }]
                } else {
                    vec![
                        ProviderObservation::BlockOpened {
                            block_index: 1,
                            descriptor_ref: second_ref.clone(),
                        },
                        ProviderObservation::BytesAppended {
                            block_index: 1,
                            component: "text".into(),
                            byte_offset: offset,
                            bytes_ref: second.component().1.clone(),
                        },
                    ]
                };
                let before = h.store.head(&h.stream).expect("head");
                assert!(
                    observe(port.as_ref(), invalid, second_payloads.clone())
                        .await
                        .is_err()
                );
                assert_eq!(h.store.head(&h.stream).expect("unchanged head"), before);
            }
            if positioned {
                let before = h.store.head(&h.stream).expect("head");
                assert!(
                    observe(
                        port.as_ref(),
                        vec![ProviderObservation::ProtocolSegmentObserved {
                            position: position.clone(),
                            component: "media".into(),
                            byte_offset: 0,
                            segment_ref: second_ref.clone(),
                        }],
                        second_payloads.clone(),
                    )
                    .await
                    .is_err()
                );
                assert_eq!(h.store.head(&h.stream).expect("unchanged head"), before);
            }
            let second_ops = if positioned {
                vec![ProviderObservation::ProtocolSegmentObserved {
                    position: if coordinates == "mixed" {
                        ProviderContentPosition {
                            output_index: position.output_index,
                            content_index: None,
                        }
                    } else {
                        position
                    },
                    component: "text".into(),
                    byte_offset: if coordinates == "mixed" { 0 } else { 3 },
                    segment_ref: second_ref,
                }]
            } else {
                vec![
                    ProviderObservation::BlockOpened {
                        block_index: 1,
                        descriptor_ref: second_ref.clone(),
                    },
                    ProviderObservation::BytesAppended {
                        block_index: 1,
                        component: "text".into(),
                        byte_offset: 0,
                        bytes_ref: second.component().1.clone(),
                    },
                    ProviderObservation::BlockFinalized {
                        block_index: 1,
                        finalized_block_ref: second_ref,
                    },
                ]
            };
            let last = observe(port.as_ref(), second_ops, second_payloads)
                .await
                .expect("second segment")
                .event_ids
                .last()
                .cloned()
                .expect("last progress");
            for reversed in [true, false] {
                let before = h.store.head(&h.stream).expect("head before terminal");
                let response = PayloadRecord::json(
                    "session.response/v1",
                    &ProviderResponsePayload {
                        blocks: if reversed {
                            vec![second.clone(), first.clone()]
                        } else {
                            vec![first.clone(), second.clone()]
                        },
                        completion: ContentCompletion::Complete,
                        finish_evidence_ref: None,
                        usage_ref: None,
                        reasoning_absence: Some("not_returned".into()),
                    },
                )
                .expect("response");
                let result = port
                    .observe(
                        EventData::ProviderAttemptCompleted {
                            response_ref: StoredPayloadRef::from(&response),
                            served_attribution_ref: None,
                            last_progress_event_id: Some(last.clone()),
                        },
                        vec![response],
                    )
                    .await;
                assert_eq!(
                    result.is_err(),
                    reversed,
                    "final order must consume exact durable segment references"
                );
                if reversed {
                    assert_eq!(h.store.head(&h.stream).expect("unchanged head"), before);
                }
            }
        }
    }
}
