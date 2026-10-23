use super::*;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use zeroclaw_api::session_events::capture::ProviderDispatchIntent;
use zeroclaw_infra::session_events::open_event_store;

mod approval_replacement;

struct Authority(AtomicBool);
impl PermissionResolver for Authority {
    fn authorize_context(&self, _: &SessionStreamId, _: &BranchId) -> Result<(), SessionError> {
        if self.0.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(SessionError::PermissionDenied("fixture live deny".into()))
        }
    }
    fn authorize(&self, _: &EffectIntent) -> Result<(), SessionError> {
        if self.0.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(SessionError::PermissionDenied("fixture live deny".into()))
        }
    }
}

struct RejectionFailureStore {
    inner: Arc<dyn SessionEventStore>,
    fail_rejection: Arc<AtomicBool>,
    append_time: Arc<AtomicI64>,
}
impl SessionEventStore for RejectionFailureStore {
    fn append_batch(&self, mut request: AppendRequest) -> Result<AppendReceipt, AppendError> {
        if self.fail_rejection.load(Ordering::SeqCst)
            && request
                .events
                .iter()
                .any(|event| event.body.kind() == EventKind::ApprovalReplyRejected)
        {
            return Err(AppendError::StorageUnavailable(
                "fixture append failure".into(),
            ));
        }
        let append_time = self.append_time.load(Ordering::SeqCst);
        if append_time > 0 {
            for event in &mut request.events {
                event.occurred_at = append_time;
            }
        }
        self.inner.append_batch(request)
    }
    fn read_event(&self, id: &EventId) -> Result<Option<SessionEvent>, ReadError> {
        self.inner.read_event(id)
    }
    fn read_page(
        &self,
        stream: &SessionStreamId,
        after: &EventCursor,
        through: Option<&EventCursor>,
        max_events: usize,
        max_bytes: usize,
    ) -> Result<EventPage, ReadError> {
        self.inner
            .read_page(stream, after, through, max_events, max_bytes)
    }
    fn head(&self, stream: &SessionStreamId) -> Result<EventCursor, ReadError> {
        self.inner.head(stream)
    }
    fn lookup_receipt(
        &self,
        stream: &SessionStreamId,
        key: &str,
    ) -> Result<Option<AppendReceipt>, ReadError> {
        self.inner.lookup_receipt(stream, key)
    }
    fn read_payload_bounded(
        &self,
        id: &PayloadId,
        max_bytes: usize,
    ) -> Result<PayloadRead, ReadError> {
        self.inner.read_payload_bounded(id, max_bytes)
    }
    fn erasure_scope(
        &self,
        targets: &[StoredPayloadRef],
    ) -> Result<(Vec<SessionStreamId>, Digest), AppendError> {
        self.inner.erasure_scope(targets)
    }
}

struct Harness {
    _dir: tempfile::TempDir,
    store: Arc<dyn SessionEventStore>,
    owner: Arc<SessionCoordinator>,
    stream: SessionStreamId,
    branch: BranchId,
    turn: TurnId,
    context: EventId,
    authority: Arc<Authority>,
    fail_rejection: Arc<AtomicBool>,
    append_time: Arc<AtomicI64>,
}
impl Harness {
    fn new(backend: &str) -> Self {
        let dir = tempfile::TempDir::new().expect("dir");
        let fail_rejection = Arc::new(AtomicBool::new(false));
        let append_time = Arc::new(AtomicI64::new(0));
        let store: Arc<dyn SessionEventStore> = Arc::new(RejectionFailureStore {
            inner: open_event_store(dir.path(), backend).expect("store"),
            fail_rejection: Arc::clone(&fail_rejection),
            append_time: Arc::clone(&append_time),
        });
        let stream = SessionStreamId::new();
        let branch = BranchId::new();
        let turn = TurnId::new();
        let content = PayloadRecord::json(
            "session.content/v1",
            &ContentPayload {
                message_id: MessageId::new(),
                parts: vec![],
            },
        )
        .expect("content");
        let evidence =
            PayloadRecord::json("session.evidence/v1", &serde_json::json!({"fixture":true}))
                .expect("evidence");
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
        let make = |data| {
            let mut event =
                SessionEvent::typed(stream.clone(), data, EventActor::default()).expect("event");
            event.branch_id = Some(branch.clone());
            event.turn_id = Some(turn.clone());
            event
        };
        let accepted = make(EventData::TurnAccepted {
            input_ref: StoredPayloadRef::from(&content),
            ingress_ref: StoredPayloadRef::from(&evidence),
            reply_route_ref: None,
        });
        let started = make(EventData::TurnStarted {
            accepted_event_id: accepted.event_id.clone(),
            execution_binding_ref: StoredPayloadRef::from(&evidence),
        });
        let context_record = PayloadRecord::json(
            "session.context/v1",
            &serde_json::json!({"message_refs":[]}),
        )
        .expect("context");
        let projection = make(EventData::ContextProjectionCreated {
            source_ranges: vec![],
            summary_ref: None,
            budget: 4096,
            reason_key: "fixture".into(),
            algorithm_version: "fixture/v1".into(),
            result_ref: StoredPayloadRef::from(&context_record),
            summary_source_refs: vec![],
            transform_started_event_id: None,
        });
        let context = projection.event_id.clone();
        store
            .append_batch(AppendRequest {
                stream_id: stream.clone(),
                expected_head: EventCursor::beginning(stream.clone()),
                idempotency_key: "fixture".into(),
                events: vec![root, accepted, started, projection],
                payloads: vec![content, evidence, context_record],
                erasure_reference_commitment: None,
            })
            .expect("initialize");
        Self {
            _dir: dir,
            owner: Arc::new(SessionCoordinator::new(Arc::clone(&store))),
            store,
            stream,
            branch,
            turn,
            context,
            authority: Arc::new(Authority(AtomicBool::new(true))),
            fail_rejection,
            append_time,
        }
    }
    fn events(&self) -> Vec<SessionEvent> {
        self.store
            .read_page(
                &self.stream,
                &EventCursor::beginning(self.stream.clone()),
                None,
                100,
                4 * 1024 * 1024,
            )
            .expect("events")
            .events
    }
    async fn prepare(&self, text_protocol: bool) -> Arc<dyn PreparedToolPort> {
        self.prepare_with_arguments(
            text_protocol,
            serde_json::json!({"command":"true"}),
            serde_json::json!({"command":"true"}),
        )
        .await
    }
    async fn prepare_with_arguments(
        &self,
        text_protocol: bool,
        provider_arguments: serde_json::Value,
        execution_arguments: serde_json::Value,
    ) -> Arc<dyn PreparedToolPort> {
        let provider = self.owner.provider_capture(
            self.stream.clone(),
            self.branch.clone(),
            self.turn.clone(),
            self.context.clone(),
            self.authority.clone(),
        );
        let observer = provider
            .admit(ProviderDispatchIntent {
                requested_route: None,
                previous_attempt_event_id: None,
                tool_round_policy: zeroclaw_api::model_provider::ToolRoundPolicy::Batch,
                thinking: None,
                provider_ref: "fixture".into(),
                model: "model".into(),
                messages: vec![],
                tools: None,
                temperature: None,
            })
            .await
            .expect("provider admit");
        let args =
            PayloadRecord::json("session.tool-arguments/v1", &provider_arguments).expect("args");
        let call=PayloadRecord::json("session.tool-call/v1",&serde_json::json!({"name":"shell","provider_call_id":"call-1","arguments_ref":StoredPayloadRef::from(&args)})).expect("call");
        let text = PayloadRecord::new(
            PayloadId::new(),
            "session.text/v1".into(),
            format!(
                "<tool_call>{}</tool_call>",
                serde_json::json!({"name":"shell","arguments":provider_arguments,"tool_call_id":"call-1"})
            )
            .into_bytes(),
        )
        .expect("text");
        let response = PayloadRecord::json(
            "session.response/v1",
            &ProviderResponsePayload {
                blocks: vec![if text_protocol {
                    ResponseBlock::Text {
                        text_ref: StoredPayloadRef::from(&text),
                    }
                } else {
                    ResponseBlock::ToolCall {
                        tool_call_ref: StoredPayloadRef::from(&call),
                    }
                }],
                completion: ContentCompletion::Complete,
                finish_evidence_ref: None,
                usage_ref: None,
                reasoning_absence: Some("not_returned".into()),
            },
        )
        .expect("response");
        observer
            .observe(
                EventData::ProviderAttemptCompleted {
                    response_ref: StoredPayloadRef::from(&response),
                    served_attribution_ref: None,
                    last_progress_event_id: None,
                },
                vec![args, call, text, response],
            )
            .await
            .expect("provider completed");
        let tools = self.owner.tool_capture(
            self.stream.clone(),
            self.branch.clone(),
            self.turn.clone(),
            self.authority.clone(),
        );
        tools
            .prepare(ToolPreparationIntent {
                proposed_name: "shell".into(),
                provider_call_id: (!text_protocol).then(|| "call-1".into()),
                name: "shell".into(),
                arguments: execution_arguments,
                approval_requirement: "prompt".into(),
                trusted_route: serde_json::json!({"channel":"fixture"}),
            })
            .await
            .expect("prepare")
    }
}

#[tokio::test]
async fn recovered_arguments_preserve_origin_and_execute_after_approval() {
    for backend in ["sqlite", "jsonl"] {
        for text_protocol in [false, true] {
            for recovered in [
                serde_json::json!({"maxResults":3}),
                serde_json::json!(["first", "second"]),
            ] {
                let h = Harness::new(backend);
                let provider_arguments = serde_json::json!({"params":recovered.to_string()});
                let execution_arguments = serde_json::json!({"params":recovered});
                let port = h
                    .prepare_with_arguments(
                        text_protocol,
                        provider_arguments.clone(),
                        execution_arguments.clone(),
                    )
                    .await;
                port.verify_action("shell", &execution_arguments)
                    .await
                    .expect("recovered arguments are the execution input");
                assert!(
                    port.verify_action("shell", &provider_arguments)
                        .await
                        .is_err()
                );
                assert!(port.start().await.is_err(), "approval is still required");
                port.request_approval(Default::default())
                    .await
                    .expect("request");
                port.resolve_approval(
                    ChannelApprovalResponse::Approve,
                    "approved",
                    None,
                    serde_json::json!({"fixture_operator_reply":true}),
                )
                .await
                .expect("approve");
                let _observer = port.start().await.expect("start recovered call");
                let events = h.events();
                let proposal = events
                    .iter()
                    .find_map(|event| match &event.body.data {
                        EventData::ToolCallProposed {
                            provider_result_event_id,
                            block_index,
                        } => Some((provider_result_event_id, *block_index)),
                        _ => None,
                    })
                    .expect("proposal origin");
                let provider = events
                    .iter()
                    .find(|event| event.body.kind() == EventKind::ProviderAttemptCompleted)
                    .expect("provider response");
                assert_eq!(proposal, (&provider.event_id, 0));
                assert!(
                    events
                        .iter()
                        .any(|event| event.body.kind() == EventKind::ToolEffectStarted)
                );
            }
        }
    }
}

#[tokio::test]
async fn proposal_still_requires_current_provider_call_identity() {
    for backend in ["sqlite", "jsonl"] {
        for text_protocol in [false, true] {
            let h = Harness::new(backend);
            let _prepared = h.prepare(text_protocol).await;
            let tools = h.owner.tool_capture(
                h.stream.clone(),
                h.branch.clone(),
                h.turn.clone(),
                h.authority.clone(),
            );
            tools
                .prepare(ToolPreparationIntent {
                    proposed_name: "shell".into(),
                    provider_call_id: Some("call-1".into()),
                    name: "shell".into(),
                    arguments: serde_json::json!({"command":"true"}),
                    approval_requirement: "prompt".into(),
                    trusted_route: serde_json::json!({"channel":"fixture"}),
                })
                .await
                .expect("provider identity matches without comparing arguments");
            for (name, id) in [("different", "call-1"), ("shell", "different")] {
                assert!(
                    tools
                        .prepare(ToolPreparationIntent {
                            proposed_name: name.into(),
                            provider_call_id: Some(id.into()),
                            name: "shell".into(),
                            arguments: serde_json::json!({"command":"true"}),
                            approval_requirement: "prompt".into(),
                            trusted_route: serde_json::json!({"channel":"fixture"}),
                        })
                        .await
                        .is_err()
                );
            }
        }
    }
}

#[tokio::test]
async fn approval_deadlines_follow_owner_intent_and_preserve_legacy_values() {
    for backend in ["sqlite", "jsonl"] {
        for (expiry_offset, start_offset, may_start) in [
            (None, 301_000, true),
            (Some(600_000), 301_000, true),
            (Some(300_000), 299_999, true),
            (Some(300_000), 300_000, false),
            (Some(300_000), 301_000, false),
        ] {
            let h = Harness::new(backend);
            let requested_at = h.events().last().expect("initialized events").occurred_at;
            h.append_time.store(requested_at, Ordering::SeqCst);
            let port = h.prepare(false).await;
            let expires_at = expiry_offset.map(|offset| requested_at + offset);
            port.request_approval(ApprovalRequestIntent { expires_at })
                .await
                .expect("owner intent");
            assert!(
                port.request_approval(ApprovalRequestIntent {
                    expires_at: Some(requested_at + 900_000),
                })
                .await
                .is_err(),
                "an existing request cannot extend its deadline"
            );
            let requested = h
                .events()
                .into_iter()
                .find(|event| event.body.kind() == EventKind::ApprovalRequested)
                .expect("requested");
            assert!(matches!(
                &requested.body.data,
                EventData::ApprovalRequested { expires_at: actual, .. } if *actual == expires_at
            ));

            let mut legacy = serde_json::to_value(&requested.body.data).expect("event value");
            legacy["fields"]["expires_at"] = serde_json::json!(requested_at + 600_000);
            assert!(matches!(
                serde_json::from_value::<EventData>(legacy.clone()).expect("legacy integer"),
                EventData::ApprovalRequested { expires_at: Some(value), .. }
                    if value == requested_at + 600_000
            ));
            legacy["fields"]
                .as_object_mut()
                .expect("fields")
                .remove("expires_at");
            assert!(matches!(
                serde_json::from_value::<EventData>(legacy).expect("unspecified deadline"),
                EventData::ApprovalRequested {
                    expires_at: None,
                    ..
                }
            ));

            h.append_time
                .store(requested_at + start_offset, Ordering::SeqCst);
            port.resolve_approval(
                ChannelApprovalResponse::Approve,
                "operator_approved",
                None,
                serde_json::json!({"source":"operator"}),
            )
            .await
            .expect("reply");
            let resolved_head = h.store.head(&h.stream).expect("resolved head");
            assert_eq!(port.start().await.is_ok(), may_start);
            let events = h.events();
            let started = events
                .iter()
                .find(|event| event.body.kind() == EventKind::ToolEffectStarted);
            if may_start {
                assert_eq!(
                    started.expect("started").occurred_at,
                    requested_at + start_offset
                );
                assert!(port.start().await.is_err(), "one execution only");
            } else {
                assert!(started.is_none());
                assert_eq!(h.store.head(&h.stream).expect("head"), resolved_head);
                assert!(
                    !events
                        .iter()
                        .any(|event| event.body.kind() == EventKind::ApprovalConsumed)
                );
            }
        }
    }
}

struct ApprovalOwnerChannel {
    store: Arc<dyn SessionEventStore>,
    stream: SessionStreamId,
    append_time: Arc<AtomicI64>,
    response_time: Timestamp,
    source: zeroclaw_api::channel::ApprovalSource,
}

impl zeroclaw_api::attribution::Attributable for ApprovalOwnerChannel {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        zeroclaw_api::attribution::Role::Channel(zeroclaw_api::attribution::ChannelKind::Cli)
    }
    fn alias(&self) -> &str {
        "approval-owner-test"
    }
}

#[async_trait::async_trait]
impl zeroclaw_api::channel::Channel for ApprovalOwnerChannel {
    fn name(&self) -> &str {
        "approval-owner-test"
    }
    async fn send(&self, _: &zeroclaw_api::channel::SendMessage) -> anyhow::Result<()> {
        unreachable!("approval uses its native surface")
    }
    async fn listen(
        &self,
        _: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
    ) -> anyhow::Result<()> {
        unreachable!("approval reply is controlled by the fixture")
    }
    async fn request_approval_attributed(
        &self,
        _: &str,
        _: &zeroclaw_api::channel::ChannelApprovalRequest,
    ) -> anyhow::Result<Option<zeroclaw_api::channel::AttributedApprovalResponse>> {
        let events = self.store.read_page(
            &self.stream,
            &EventCursor::beginning(self.stream.clone()),
            None,
            100,
            4 * 1024 * 1024,
        )?;
        assert!(
            events.events.iter().any(|event| matches!(
                event.body.data,
                EventData::ApprovalRequested {
                    expires_at: None,
                    ..
                }
            )),
            "request must be durable before presenting the prompt"
        );
        self.append_time.store(self.response_time, Ordering::SeqCst);
        let response = if self.source.is_runtime_fail_closed() {
            zeroclaw_api::channel::ChannelApprovalResponse::Deny
        } else {
            zeroclaw_api::channel::ChannelApprovalResponse::Approve
        };
        Ok(Some(
            zeroclaw_api::channel::AttributedApprovalResponse::from_runtime(response, self.source),
        ))
    }
}

#[tokio::test]
async fn approval_gate_preserves_owner_outcome_and_long_wait_without_invented_expiry() {
    use crate::approval::{ApprovalManager, ApprovalResponse};
    use crate::approval_gate::{ApprovalContext, ApprovalGateOutcome, gate_tool_approval};
    use zeroclaw_api::channel::{ApprovalPosition, ApprovalSource};
    use zeroclaw_api::session_events::capture::scope_tool_preparation;

    for backend in ["sqlite", "jsonl"] {
        for (source, source_key, reason_key) in [
            (ApprovalSource::Operator, "operator", None),
            (
                ApprovalSource::TimedOut,
                "timed_out",
                Some("approval_timed_out"),
            ),
            (
                ApprovalSource::Unreachable,
                "unreachable",
                Some("approval_route_unreachable"),
            ),
            (
                ApprovalSource::Unavailable,
                "unavailable",
                Some("approval_route_unavailable"),
            ),
        ] {
            let h = Harness::new(backend);
            let requested_at = h.events().last().expect("initialized events").occurred_at;
            h.append_time.store(requested_at, Ordering::SeqCst);
            let port = h.prepare(false).await;
            let channel = ApprovalOwnerChannel {
                store: Arc::clone(&h.store),
                stream: h.stream.clone(),
                append_time: Arc::clone(&h.append_time),
                response_time: requested_at + 301_000,
                source,
            };
            let manager = ApprovalManager::for_non_interactive_backchannel(
                &zeroclaw_config::schema::RiskProfileConfig {
                    level: zeroclaw_config::autonomy::AutonomyLevel::Full,
                    always_ask: vec!["shell".into()],
                    ..Default::default()
                },
            );
            let prompt_cli =
                |_: &crate::approval::ApprovalRequest| -> std::io::Result<ApprovalResponse> {
                    unreachable!("non-interactive approval")
                };
            let render = |arguments: &serde_json::Value| arguments.to_string();
            let ctx = ApprovalContext {
                approval: Some(&manager),
                channel: Some(&channel),
                channel_name: "approval-owner-test",
                channel_reply_target: Some("operator"),
                cancellation_token: None,
                progress: None,
                model: "fixture",
                turn_id: "fixture",
                prompt_cli: &prompt_cli,
                render_arguments_summary: &render,
                scrub_credentials: str::to_owned,
            };
            let outcome = scope_tool_preparation(
                Arc::clone(&port),
                gate_tool_approval(
                    &ctx,
                    "shell",
                    &serde_json::json!({"command":"true"}),
                    0,
                    ApprovalPosition { index: 1, total: 1 },
                ),
            )
            .await;
            let events = h.events();
            let resolved = events
                .iter()
                .find_map(|event| match &event.body.data {
                    EventData::ApprovalResolved { resolution_ref, .. } => Some(resolution_ref),
                    _ => None,
                })
                .expect("durable resolution");
            let PayloadRead::Present(resolution) = h
                .store
                .read_payload(&resolved.payload_id)
                .expect("resolution")
            else {
                panic!("resolution retained");
            };
            let resolution: ApprovalResolution =
                serde_json::from_slice(&resolution.body).expect("resolution value");
            let evidence_ref = match resolution {
                ApprovalResolution::Approved {
                    verification_evidence_ref,
                    ..
                } => {
                    assert!(reason_key.is_none());
                    assert!(matches!(
                        outcome,
                        ApprovalGateOutcome::Proceed { approved: true }
                    ));
                    port.start()
                        .await
                        .expect("operator's legal long wait remains valid");
                    verification_evidence_ref
                }
                ApprovalResolution::Denied {
                    reason_key: actual,
                    verification_evidence_ref,
                } => {
                    assert_eq!(Some(actual.as_str()), reason_key);
                    assert!(matches!(outcome, ApprovalGateOutcome::Deny(_)));
                    assert!(port.start().await.is_err());
                    verification_evidence_ref
                }
            };
            let PayloadRead::Present(evidence) = h
                .store
                .read_payload(&evidence_ref.payload_id)
                .expect("evidence")
            else {
                panic!("evidence retained");
            };
            let evidence: serde_json::Value =
                serde_json::from_slice(&evidence.body).expect("evidence value");
            assert_eq!(evidence["source"], source_key);
            assert_eq!(evidence["operator_reply_available"], reason_key.is_none());
        }
    }
}

#[tokio::test]
async fn rejected_replies_keep_bound_approval_identity_without_resolving_it() {
    for backend in ["sqlite", "jsonl"] {
        let h = Harness::new(backend);
        let port = h.prepare(false).await;
        port.request_approval(Default::default())
            .await
            .expect("request");
        let request = h
            .events()
            .into_iter()
            .find(|event| event.body.kind() == EventKind::ApprovalRequested)
            .expect("requested event");
        let EventData::ApprovalRequested { request_id, .. } = &request.body.data else {
            panic!("requested kind");
        };
        let evidence = serde_json::json!({
            "source":"fixture_channel",
            "responder_id":"rejected-fixture",
            "api_key":"fixture-secret-value",
        });
        for _ in 0..2 {
            port.record_approval_reply_rejected("unauthorized_responder".into(), evidence.clone())
                .await
                .expect("rejection durable");
        }
        let events = h.events();
        let rejections: Vec<_> = events
            .iter()
            .filter(|event| event.body.kind() == EventKind::ApprovalReplyRejected)
            .collect();
        assert_eq!(rejections.len(), 2);
        assert_ne!(rejections[0].event_id, rejections[1].event_id);
        for event in rejections {
            assert_eq!(event.stream_id, request.stream_id);
            assert_eq!(event.branch_id, request.branch_id);
            assert_eq!(event.turn_id, request.turn_id);
            assert_eq!(event.invocation_id, request.invocation_id);
            let EventData::ApprovalReplyRejected {
                request_id: rejected_request,
                classification,
                evidence_ref: Some(reference),
                ..
            } = &event.body.data
            else {
                panic!("rejection kind");
            };
            assert_eq!(rejected_request.as_ref(), Some(request_id));
            assert_eq!(classification, "unauthorized_responder");
            let PayloadRead::Present(payload) = h
                .store
                .read_payload(&reference.payload_id)
                .expect("evidence payload")
            else {
                panic!("evidence present");
            };
            let actual: serde_json::Value =
                serde_json::from_slice(&payload.body).expect("evidence");
            assert_eq!(actual, evidence);
        }
        assert!(!events.iter().any(|event| matches!(
            event.body.kind(),
            EventKind::ApprovalResolved
                | EventKind::ApprovalConsumed
                | EventKind::ApprovalInvalidated
        )));
        assert!(port.start().await.is_err());
        port.resolve_approval(
            ChannelApprovalResponse::Approve,
            "approved",
            None,
            serde_json::json!({"source":"operator_reply"}),
        )
        .await
        .expect("valid reply remains available");
        port.start().await.expect("valid approval may execute");
    }
}

#[tokio::test]
async fn rejected_replies_propagate_storage_errors_and_recheck_current_authority() {
    for backend in ["sqlite", "jsonl"] {
        let h = Harness::new(backend);
        let port = h.prepare(false).await;
        port.request_approval(Default::default())
            .await
            .expect("request");
        let before = h.store.head(&h.stream).expect("head");
        h.fail_rejection.store(true, Ordering::SeqCst);
        let error = port
            .record_approval_reply_rejected("wrong_destination".into(), serde_json::json!({}))
            .await
            .expect_err("storage failure propagates");
        assert!(format!("{error:#}").contains("fixture append failure"));
        assert_eq!(h.store.head(&h.stream).expect("head"), before);
        h.fail_rejection.store(false, Ordering::SeqCst);
        h.authority.0.store(false, Ordering::SeqCst);
        let error = port
            .record_approval_reply_rejected("wrong_destination".into(), serde_json::json!({}))
            .await
            .expect_err("current authority revoked");
        assert!(format!("{error:#}").contains("fixture live deny"));
        assert_eq!(h.store.head(&h.stream).expect("head"), before);
        h.authority.0.store(true, Ordering::SeqCst);
        assert!(
            port.record_approval_reply_rejected(String::new(), serde_json::json!({}))
                .await
                .is_err()
        );
        assert_eq!(h.store.head(&h.stream).expect("head"), before);
        port.record_approval_reply_rejected("wrong_destination".into(), serde_json::json!({}))
            .await
            .expect("reply can be recorded after failure");
        assert!(port.start().await.is_err());
        port.invalidate("cancelled_wait").await.expect("invalidate");
        port.record_approval_reply_rejected("receiver_closed".into(), serde_json::json!({}))
            .await
            .expect("late refusal remains recordable");
        assert!(port.start().await.is_err());
        assert_eq!(
            h.events()
                .iter()
                .filter(|event| event.body.kind() == EventKind::ApprovalReplyRejected)
                .count(),
            2
        );
    }
}

#[tokio::test]
async fn approval_is_consumed_once_with_start_and_rechecks_live_authority() {
    for backend in ["sqlite", "jsonl"] {
        for text_protocol in [false, true] {
            let h = Harness::new(backend);
            let port = h.prepare(text_protocol).await;
            port.verify_action("shell", &serde_json::json!({"command":"true"}))
                .await
                .expect("same action");
            assert!(
                port.verify_action("shell", &serde_json::json!({"command":"different"}))
                    .await
                    .is_err()
            );
            assert!(
                port.verify_action("different", &serde_json::json!({"command":"true"}))
                    .await
                    .is_err()
            );
            assert!(port.start().await.is_err());
            port.request_approval(Default::default())
                .await
                .expect("request");
            let requested_head = h.store.head(&h.stream).expect("head");
            port.request_approval(Default::default())
                .await
                .expect("same request retry");
            assert_eq!(h.store.head(&h.stream).expect("head"), requested_head);
            port.resolve_approval(
                ChannelApprovalResponse::Approve,
                "approved",
                None,
                serde_json::json!({"fixture_operator_reply":true}),
            )
            .await
            .expect("reply durable");
            let resolved_head = h.store.head(&h.stream).expect("head");
            port.resolve_approval(
                ChannelApprovalResponse::Approve,
                "approved",
                None,
                serde_json::json!({"fixture_operator_reply":true}),
            )
            .await
            .expect("same reply retry");
            assert_eq!(h.store.head(&h.stream).expect("head"), resolved_head);
            assert!(
                port.resolve_approval(
                    ChannelApprovalResponse::Deny,
                    "changed",
                    None,
                    serde_json::json!({"fixture_operator_reply":true})
                )
                .await
                .is_err()
            );
            assert!(
                port.resolve_approval(
                    ChannelApprovalResponse::Approve,
                    "approved",
                    None,
                    serde_json::json!({"fixture_operator_reply":"different"})
                )
                .await
                .is_err()
            );
            h.authority.0.store(false, Ordering::SeqCst);
            assert!(port.start().await.is_err());
            assert!(
                !h.events()
                    .iter()
                    .any(|event| event.body.kind() == EventKind::ApprovalConsumed)
            );
            h.authority.0.store(true, Ordering::SeqCst);
            let observer = port.start().await.expect("start");
            let events = h.events();
            assert_eq!(
                events[events.len() - 2].body.kind(),
                EventKind::ApprovalConsumed
            );
            assert_eq!(
                events.last().expect("start").body.kind(),
                EventKind::ToolEffectStarted
            );
            assert_eq!(
                events[events.len() - 2].invocation_id,
                events.last().expect("start").invocation_id
            );
            assert!(port.start().await.is_err());
            let output = PayloadRecord::json(
                "session.tool-result/v1",
                &serde_json::json!({"output":"done"}),
            )
            .expect("output");
            observer
                .observe(
                    EventData::ToolEffectCompleted {
                        result_ref: StoredPayloadRef::from(&output),
                        raw_result_ref: None,
                        transformation_ref: None,
                        receipt_ref: None,
                    },
                    vec![output],
                )
                .await
                .expect("complete");
            assert!(
                observer
                    .observe(
                        EventData::InvocationOutcomeUnknown {
                            last_observed_event_id: observer.started_event_id(),
                            reason_key: "late".into(),
                            observed_partial_ref: None,
                            error_ref: None
                        },
                        vec![]
                    )
                    .await
                    .is_err()
            );
        }
    }
}
#[tokio::test]
async fn approval_resolution_preserves_the_exact_authenticated_reply_decision() {
    let decisions = [
        ApprovalReplyDecision::ApproveOnce,
        ApprovalReplyDecision::ApproveSession,
        ApprovalReplyDecision::Deny,
        ApprovalReplyDecision::Replace,
    ];
    let approves = |decision| {
        matches!(
            decision,
            ApprovalReplyDecision::ApproveOnce | ApprovalReplyDecision::ApproveSession
        )
    };
    let response_for = |decision| match decision {
        ApprovalReplyDecision::ApproveOnce => ChannelApprovalResponse::Approve,
        ApprovalReplyDecision::ApproveSession => ChannelApprovalResponse::AlwaysApprove,
        ApprovalReplyDecision::Deny => ChannelApprovalResponse::Deny,
        ApprovalReplyDecision::Replace => ChannelApprovalResponse::DenyWithEdit {
            replacement: "revised request".into(),
        },
    };
    for backend in ["sqlite", "jsonl"] {
        for recorded_decision in decisions {
            let h = Harness::new(backend);
            let port = h.prepare(false).await;
            port.request_approval(Default::default())
                .await
                .expect("request");
            let response = response_for(recorded_decision);
            let actor = EventActor::Principal {
                principal_id: "fixture-operator".into(),
            };
            port.record_approval_reply(
                actor.clone(),
                response,
                serde_json::json!({"transport":"fixture"}),
            )
            .await
            .expect("authenticated reply durable");
            let reply = h
                .events()
                .into_iter()
                .find(|event| matches!(&event.body.data, EventData::ControlInputRecorded { control_ref, .. } if control_ref.codec == "session.approval-reply/v1"))
                .expect("recorded reply");
            assert_eq!(reply.actor, actor);
            let requested = h
                .store
                .read_event(reply.causation_id.as_ref().expect("request cause"))
                .expect("request read")
                .expect("request retained");
            let EventData::ApprovalRequested { request_id, .. } = requested.body.data else {
                panic!("reply points to its approval request");
            };
            let head = h.store.head(&h.stream).expect("reply head");
            for attempted_decision in decisions {
                if attempted_decision == recorded_decision {
                    continue;
                }
                let approved = approves(attempted_decision);
                let evidence_value = serde_json::json!({
                    "decision":attempted_decision,
                    "operator_reply_available":true,
                });
                assert!(
                    port.resolve_approval(
                        response_for(attempted_decision),
                        "operator_decision",
                        Some(reply.event_id.clone()),
                        evidence_value.clone()
                    )
                    .await
                    .is_err(),
                    "kernel cannot relabel {recorded_decision:?} as {attempted_decision:?}"
                );
                assert_eq!(h.store.head(&h.stream).expect("unchanged head"), head);

                let evidence =
                    PayloadRecord::json("session.evidence/v1", &evidence_value).expect("evidence");
                let resolution = if approved {
                    ApprovalResolution::Approved {
                        confirmation_id: request_id.clone(),
                        validity: "one_execution".into(),
                        verification_evidence_ref: StoredPayloadRef::from(&evidence),
                    }
                } else {
                    ApprovalResolution::Denied {
                        reason_key: "operator_decision".into(),
                        verification_evidence_ref: StoredPayloadRef::from(&evidence),
                    }
                };
                let resolution = PayloadRecord::json("session.approval-resolution/v1", &resolution)
                    .expect("resolution payload");
                let mut event = SessionEvent::typed(
                    h.stream.clone(),
                    EventData::ApprovalResolved {
                        request_id: request_id.clone(),
                        resolution_ref: StoredPayloadRef::from(&resolution),
                    },
                    EventActor::default(),
                )
                .expect("resolution event");
                event.branch_id = reply.branch_id.clone();
                event.turn_id = reply.turn_id.clone();
                event.invocation_id = reply.invocation_id.clone();
                event.causation_id = Some(reply.event_id.clone());
                let error = h
                    .store
                    .append_batch(AppendRequest {
                        stream_id: h.stream.clone(),
                        expected_head: head.clone(),
                        idempotency_key: format!("mismatched-resolution:{attempted_decision:?}"),
                        events: vec![event],
                        payloads: vec![evidence, resolution],
                        erasure_reference_commitment: None,
                    })
                    .expect_err("storage semantics independently reject a changed decision");
                assert!(matches!(error, AppendError::InvalidReference(_)));
                assert_eq!(h.store.head(&h.stream).expect("unchanged head"), head);
            }
            port.resolve_approval(
                response_for(recorded_decision),
                "operator_decision",
                Some(reply.event_id.clone()),
                serde_json::json!({"decision":recorded_decision,"operator_reply_available":true}),
            )
            .await
            .expect("exact decision resolves");
            let resolved = h
                .events()
                .into_iter()
                .find(|event| event.body.kind() == EventKind::ApprovalResolved)
                .expect("resolved");
            assert_eq!(resolved.causation_id, Some(reply.event_id));
            assert_eq!(port.start().await.is_ok(), approves(recorded_decision));
        }
    }
}

#[tokio::test]
async fn approval_resolution_follows_the_delivered_reply_across_native_fallbacks() {
    use zeroclaw_api::channel::ChannelApprovalResponse;

    for backend in ["sqlite", "jsonl"] {
        for adopted_actor in [
            EventActor::Principal {
                principal_id: "fallback-operator".into(),
            },
            EventActor::ChannelResponder {
                channel: "channel.slack.fixture".into(),
                responder_id: "fallback-responder".into(),
            },
            EventActor::System {
                component: "approval_identity_unavailable".into(),
            },
        ] {
            let h = Harness::new(backend);
            let port = h.prepare(false).await;
            port.request_approval(Default::default())
                .await
                .expect("request");
            let abandoned = port
                .record_approval_reply(
                    EventActor::Principal {
                        principal_id: "timed-out-route-operator".into(),
                    },
                    ChannelApprovalResponse::Approve,
                    serde_json::json!({"transport":"first_native_channel"}),
                )
                .await
                .expect("first reply durable before delivery timeout");
            let evidence = serde_json::json!({
                "decision":"approve_once", "operator_reply_available":true,
            });
            let head = h.store.head(&h.stream).expect("abandoned reply head");
            assert!(
                port.resolve_approval(
                    ChannelApprovalResponse::Approve,
                    "approved",
                    None,
                    evidence.clone()
                )
                .await
                .is_err(),
                "a legacy fallback cannot borrow the first native reply's identity"
            );
            assert_eq!(h.store.head(&h.stream).expect("unchanged head"), head);
            let adopted = port
                .record_approval_reply(
                    adopted_actor.clone(),
                    ChannelApprovalResponse::Approve,
                    serde_json::json!({"transport":"second_native_channel"}),
                )
                .await
                .expect("a second native reply remains independently observable");
            assert_ne!(adopted, abandoned);
            let replies: Vec<_> = h
                .events()
                .into_iter()
                .filter(|event| matches!(&event.body.data, EventData::ControlInputRecorded { control_ref, .. } if control_ref.codec == "session.approval-reply/v1"))
                .collect();
            assert_eq!(replies.len(), 2);
            assert_eq!(replies[0].event_id, abandoned);
            assert_eq!(replies[1].event_id, adopted);
            assert_eq!(replies[1].actor, adopted_actor);
            assert_eq!(replies[0].causation_id, replies[1].causation_id);
            port.resolve_approval(
                ChannelApprovalResponse::Approve,
                "approved",
                Some(adopted.clone()),
                evidence.clone(),
            )
            .await
            .expect("the actually delivered second reply is adopted");
            let resolved_head = h.store.head(&h.stream).expect("resolved head");
            port.resolve_approval(
                ChannelApprovalResponse::Approve,
                "approved",
                Some(adopted.clone()),
                evidence,
            )
            .await
            .expect("exact resolution retry");
            assert_eq!(h.store.head(&h.stream).expect("retry head"), resolved_head);
            let resolved = h
                .events()
                .into_iter()
                .find(|event| event.body.kind() == EventKind::ApprovalResolved)
                .expect("resolution");
            assert_eq!(resolved.causation_id, Some(adopted));
            port.start().await.expect("the adopted approval executes");
            assert!(port.start().await.is_err());
            assert!(
                port.record_approval_reply(
                    adopted_actor,
                    ChannelApprovalResponse::Approve,
                    serde_json::json!({"transport":"late_native_channel"}),
                )
                .await
                .is_err(),
                "a resolved request no longer admits positive replies"
            );
        }
    }
}

#[tokio::test]
async fn approval_resolution_rejects_another_invocations_delivered_reply() {
    use zeroclaw_api::channel::ChannelApprovalResponse;

    for backend in ["sqlite", "jsonl"] {
        let h = Harness::new(backend);
        let first = h.prepare(false).await;
        first
            .request_approval(Default::default())
            .await
            .expect("first request");
        let second = h
            .owner
            .tool_capture(
                h.stream.clone(),
                h.branch.clone(),
                h.turn.clone(),
                h.authority.clone(),
            )
            .prepare(ToolPreparationIntent {
                proposed_name: "shell".into(),
                provider_call_id: Some("call-1".into()),
                name: "shell".into(),
                arguments: serde_json::json!({"command":"true"}),
                approval_requirement: "prompt".into(),
                trusted_route: serde_json::json!({"channel":"fixture"}),
            })
            .await
            .expect("second invocation");
        second
            .request_approval(Default::default())
            .await
            .expect("second request");
        let actor = EventActor::Principal {
            principal_id: "fixture-operator".into(),
        };
        let second_reply = second
            .record_approval_reply(
                actor.clone(),
                ChannelApprovalResponse::Approve,
                serde_json::json!({}),
            )
            .await
            .expect("second reply");
        let evidence =
            serde_json::json!({"decision":"approve_once","operator_reply_available":true});
        let head = h.store.head(&h.stream).expect("head");
        for wrong_id in [second_reply, h.context.clone(), EventId::new()] {
            assert!(
                first
                    .resolve_approval(
                        ChannelApprovalResponse::Approve,
                        "approved",
                        Some(wrong_id),
                        evidence.clone()
                    )
                    .await
                    .is_err()
            );
            assert_eq!(h.store.head(&h.stream).expect("unchanged head"), head);
        }
        let first_reply = first
            .record_approval_reply(
                actor,
                ChannelApprovalResponse::Approve,
                serde_json::json!({}),
            )
            .await
            .expect("first reply");
        first
            .resolve_approval(
                ChannelApprovalResponse::Approve,
                "approved",
                Some(first_reply),
                evidence,
            )
            .await
            .expect("the first invocation's own reply still resolves");
    }
}

struct CaptureFailureApprovalChannel {
    capture_failure: bool,
}

impl zeroclaw_api::attribution::Attributable for CaptureFailureApprovalChannel {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        zeroclaw_api::attribution::Role::Channel(zeroclaw_api::attribution::ChannelKind::Cli)
    }
    fn alias(&self) -> &str {
        "capture-failure"
    }
}

#[async_trait::async_trait]
impl zeroclaw_api::channel::Channel for CaptureFailureApprovalChannel {
    fn name(&self) -> &str {
        "capture-failure"
    }
    async fn send(&self, _: &zeroclaw_api::channel::SendMessage) -> anyhow::Result<()> {
        unreachable!()
    }
    async fn listen(
        &self,
        _: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
    ) -> anyhow::Result<()> {
        unreachable!()
    }
    async fn request_approval_attributed(
        &self,
        _: &str,
        _: &zeroclaw_api::channel::ChannelApprovalRequest,
    ) -> anyhow::Result<Option<zeroclaw_api::channel::AttributedApprovalResponse>> {
        let error = anyhow::Error::new(std::io::Error::other("fixture_approval_append_failed"));
        Err(if self.capture_failure {
            zeroclaw_api::session_events::capture::capture_stopped(error)
        } else {
            error
        })
    }
}

#[tokio::test]
async fn approval_gate_preserves_typed_capture_failure_instead_of_runtime_denial() {
    use crate::approval::{ApprovalManager, ApprovalResponse};
    use crate::approval_gate::{ApprovalContext, ApprovalGateOutcome, gate_tool_approval};
    use zeroclaw_api::channel::ApprovalPosition;
    use zeroclaw_api::session_events::capture::{is_capture_stopped, scope_tool_preparation};

    for backend in ["sqlite", "jsonl"] {
        for capture_failure in [true, false] {
            let h = Harness::new(backend);
            let port = h.prepare(false).await;
            let channel = CaptureFailureApprovalChannel { capture_failure };
            let manager = ApprovalManager::for_non_interactive_backchannel(
                &zeroclaw_config::schema::RiskProfileConfig {
                    level: zeroclaw_config::autonomy::AutonomyLevel::Full,
                    always_ask: vec!["shell".into()],
                    ..Default::default()
                },
            );
            let prompt_cli =
                |_: &crate::approval::ApprovalRequest| -> std::io::Result<ApprovalResponse> {
                    unreachable!()
                };
            let render = |arguments: &serde_json::Value| arguments.to_string();
            let ctx = ApprovalContext {
                approval: Some(&manager),
                channel: Some(&channel),
                channel_name: "capture-failure",
                channel_reply_target: Some("operator"),
                cancellation_token: None,
                progress: None,
                model: "fixture",
                turn_id: "fixture",
                prompt_cli: &prompt_cli,
                render_arguments_summary: &render,
                scrub_credentials: str::to_owned,
            };
            let outcome = scope_tool_preparation(
                Arc::clone(&port),
                gate_tool_approval(
                    &ctx,
                    "shell",
                    &serde_json::json!({"command":"true"}),
                    0,
                    ApprovalPosition { index: 1, total: 1 },
                ),
            )
            .await;
            if capture_failure {
                let ApprovalGateOutcome::Error(error) = outcome else {
                    panic!("capture failure must abort the gate");
                };
                assert!(is_capture_stopped(&error));
                assert!(error.chain().any(|source| {
                    source
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.to_string() == "fixture_approval_append_failed")
                }));
                assert!(
                    !h.events()
                        .iter()
                        .any(|event| event.body.kind() == EventKind::ApprovalResolved)
                );
            } else {
                assert!(matches!(outcome, ApprovalGateOutcome::Deny(_)));
                let resolution = h
                    .events()
                    .into_iter()
                    .find(|event| event.body.kind() == EventKind::ApprovalResolved)
                    .expect("transport denial is still recorded");
                assert!(resolution.causation_id.is_none());
            }
            assert!(port.start().await.is_err());
        }
    }
}

#[tokio::test]
async fn denial_and_invalidation_never_start_a_tool() {
    for backend in ["sqlite", "jsonl"] {
        let h = Harness::new(backend);
        let port = h.prepare(false).await;
        port.request_approval(Default::default())
            .await
            .expect("request");
        port.resolve_approval(
            ChannelApprovalResponse::Deny,
            "operator_denied",
            None,
            serde_json::json!({"fixture_reply":"deny"}),
        )
        .await
        .expect("deny");
        assert!(port.start().await.is_err());
        assert!(
            port.resolve_approval(
                ChannelApprovalResponse::Approve,
                "replay",
                None,
                serde_json::json!({})
            )
            .await
            .is_err()
        );
        assert!(
            !h.events()
                .iter()
                .any(|event| event.body.kind() == EventKind::ToolEffectStarted)
        );
        let h = Harness::new(backend);
        let port = h.prepare(false).await;
        port.request_approval(Default::default())
            .await
            .expect("request");
        port.invalidate("cancelled_wait").await.expect("invalidate");
        assert!(
            port.resolve_approval(
                ChannelApprovalResponse::Approve,
                "stale",
                None,
                serde_json::json!({})
            )
            .await
            .is_err()
        );
    }
}

struct OwnershipLost;
impl super::super::recovery::StreamExecutionLossVerifier for OwnershipLost {
    fn verify_stream_loss(
        &self,
        stream: &SessionStreamId,
        through: &EventCursor,
    ) -> Result<PayloadRecord, SessionError> {
        PayloadRecord::json(
            "session.evidence/v1",
            &serde_json::json!({"source":"fixture_owner_lease","stream":stream,"through":through}),
        )
        .map_err(AppendError::from)
        .map_err(SessionError::from)
    }
}

#[tokio::test]
async fn recovery_invalidates_pending_and_unconsumed_approvals() {
    for backend in ["sqlite", "jsonl"] {
        for approved in [false, true] {
            let h = Harness::new(backend);
            let port = h.prepare(false).await;
            port.request_approval(Default::default())
                .await
                .expect("request");
            if approved {
                port.resolve_approval(
                    ChannelApprovalResponse::Approve,
                    "approved",
                    None,
                    serde_json::json!({"fixture_reply":"approve"}),
                )
                .await
                .expect("approve");
            }
            h.owner
                .recover_interrupted_stream(
                    h.stream.clone(),
                    "approval-owner-lost".into(),
                    Arc::new(OwnershipLost),
                )
                .await
                .expect("recover");
            assert_eq!(
                h.events()
                    .iter()
                    .filter(|event| event.body.kind() == EventKind::ApprovalInvalidated)
                    .count(),
                1
            );
            assert!(port.start().await.is_err());
            if !approved {
                assert!(
                    port.resolve_approval(
                        ChannelApprovalResponse::Approve,
                        "late_reply",
                        None,
                        serde_json::json!({})
                    )
                    .await
                    .is_err()
                );
            }
            assert!(
                !h.events()
                    .iter()
                    .any(|event| event.body.kind() == EventKind::ToolEffectStarted)
            );
        }
    }
}

#[tokio::test]
async fn verified_process_cancellation_is_durable_idempotent_and_not_failed() {
    for backend in ["sqlite", "jsonl"] {
        let h = Harness::new(backend);
        let prepared = h.prepare(false).await;
        prepared
            .request_approval(Default::default())
            .await
            .expect("approval");
        prepared
            .resolve_approval(
                ChannelApprovalResponse::Approve,
                "fixture_approved",
                None,
                serde_json::json!({"source":"operator_reply"}),
            )
            .await
            .expect("reply");
        let observer = prepared.start().await.expect("start");
        let evidence = serde_json::json!({"source":"host_child_wait","process_reaped":true,"exit_status":"signal: 9"});
        let receipt = observer
            .cancel_verified("timeout", evidence.clone())
            .await
            .expect("verified stop");
        h.authority.0.store(false, Ordering::SeqCst);
        assert_eq!(
            observer
                .cancel_verified("timeout", evidence)
                .await
                .expect("settlement retry after revoke"),
            receipt
        );
        assert!(
            observer
                .cancel_verified(
                    "different",
                    serde_json::json!({"source":"host_child_wait","process_reaped":true})
                )
                .await
                .is_err()
        );
        assert!(observer.has_outcome().await.expect("canonical outcome"));
        let events = h.events();
        assert_eq!(
            events
                .iter()
                .filter(|e| e.body.kind() == EventKind::InvocationCancelled)
                .count(),
            1
        );
        assert!(
            !events
                .iter()
                .any(|e| e.body.kind() == EventKind::ToolEffectFailed)
        );
    }
}
