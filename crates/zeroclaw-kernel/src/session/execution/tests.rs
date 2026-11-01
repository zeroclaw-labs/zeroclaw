use super::*;
use zeroclaw_api::session_events::*;
use zeroclaw_infra::session_events::open_event_store;

struct Access;
impl SessionAccessPolicy for Access {
    fn authorize(
        &self,
        _: &PrincipalId,
        _: &SessionStreamId,
        _: SessionAccess,
    ) -> Result<(), SessionError> {
        Ok(())
    }
}
impl PermissionResolver for Access {
    fn authorize_context(&self, _: &SessionStreamId, _: &BranchId) -> Result<(), SessionError> {
        Ok(())
    }
    fn authorize(&self, _: &EffectIntent) -> Result<(), SessionError> {
        Ok(())
    }
}
struct Executor {
    calls: usize,
}

struct QueuedExecutor {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
#[async_trait::async_trait]
impl TurnExecutorPort for QueuedExecutor {
    async fn execute(
        &mut self,
        _: &str,
        _: Arc<dyn ProviderAdmissionPort>,
    ) -> anyhow::Result<String> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok("queued first answer".into())
    }
}

struct ContextExecutor;
#[async_trait::async_trait]
impl TurnExecutorPort for ContextExecutor {
    async fn execute(
        &mut self,
        _: &str,
        admission: Arc<dyn ProviderAdmissionPort>,
    ) -> anyhow::Result<String> {
        use zeroclaw_api::session_events::capture::{
            ContextTransformIntent, ProviderDispatchIntent,
        };
        let intent = ContextTransformIntent {
            reason_key: "fixture_trim".into(),
            algorithm_version: "fixture/v1".into(),
            budget: 1024,
        };
        let first = admission
            .begin_context_transform(intent.clone())
            .await?
            .expect("native transform");
        let stale = admission
            .begin_context_transform(intent.clone())
            .await?
            .expect("native transform");
        first
            .complete(vec![zeroclaw_api::model_provider::ChatMessage::user(
                "retained",
            )])
            .await?;
        first
            .complete(vec![zeroclaw_api::model_provider::ChatMessage::user(
                "retained",
            )])
            .await?;
        assert!(
            first
                .complete(vec![zeroclaw_api::model_provider::ChatMessage::user(
                    "changed"
                )])
                .await
                .is_err(),
            "a retry cannot change an installed projection"
        );
        assert!(
            stale.complete(vec![]).await.is_err(),
            "stale installation must not replace a newer projection"
        );
        stale
            .cancelled("context_input_no_longer_current".into())
            .await?;
        stale
            .cancelled("context_input_no_longer_current".into())
            .await?;
        assert!(stale.failed("stale context target".into()).await.is_err());
        assert!(stale.cancelled("changed outcome".into()).await.is_err());
        use zeroclaw_api::session_events::capture::{
            ModelChangeDisposition, ModelOverrideSelection,
        };
        admission
            .observe_model_change(
                ModelOverrideSelection::Set {
                    provider_ref: "fixture".into(),
                    model: "selected".into(),
                    thinking: None,
                    route_mode: zeroclaw_api::session_events::capture::ModelRouteMode::Manual,
                },
                ModelChangeDisposition::Applied,
            )
            .await?;
        admission
            .observe_model_change(
                ModelOverrideSelection::Set {
                    provider_ref: "absent".into(),
                    model: "rejected".into(),
                    thinking: None,
                    route_mode: zeroclaw_api::session_events::capture::ModelRouteMode::Manual,
                },
                ModelChangeDisposition::Rejected {
                    reason_key: "provider_missing".into(),
                    diagnostic: None,
                },
            )
            .await?;
        admission
            .observe_model_change(
                ModelOverrideSelection::Clear,
                ModelChangeDisposition::Applied,
            )
            .await?;
        let observer = admission
            .admit(ProviderDispatchIntent {
                requested_route: None,
                previous_attempt_event_id: None,
                tool_round_policy: zeroclaw_api::model_provider::ToolRoundPolicy::Batch,
                thinking: None,
                provider_ref: "fixture".into(),
                model: "fixture".into(),
                messages: vec![zeroclaw_api::model_provider::ChatMessage::user("retained")],
                tools: None,
                temperature: None,
            })
            .await?;
        let text = PayloadRecord::new(
            PayloadId::new(),
            "session.text/v1".into(),
            b"canonical model answer".to_vec(),
        )?;
        let arguments = PayloadRecord::json(
            "session.tool-arguments/v1",
            &serde_json::json!({"command":"true"}),
        )?;
        let calls: Vec<_> = ["pending-call", "approved-call"].into_iter().map(|id|
            PayloadRecord::json("session.tool-call/v1", &serde_json::json!({
                "provider_call_id":id,"name":"shell","arguments_ref":StoredPayloadRef::from(&arguments),
            }))
        ).collect::<Result<_, _>>()?;
        let mut blocks = vec![ResponseBlock::Text {
            text_ref: StoredPayloadRef::from(&text),
        }];
        blocks.extend(calls.iter().map(|call| ResponseBlock::ToolCall {
            tool_call_ref: StoredPayloadRef::from(call),
        }));
        let response = PayloadRecord::json(
            "session.response/v1",
            &ProviderResponsePayload {
                blocks,
                completion: ContentCompletion::Complete,
                finish_evidence_ref: None,
                usage_ref: None,
                reasoning_absence: Some("not_returned".into()),
            },
        )?;
        let mut payloads = vec![text, response.clone(), arguments];
        payloads.extend(calls);
        observer
            .observe(
                EventData::ProviderAttemptCompleted {
                    response_ref: StoredPayloadRef::from(&response),
                    served_attribution_ref: None,
                    last_progress_event_id: None,
                },
                payloads,
            )
            .await?;
        let tools = zeroclaw_api::session_events::capture::current_tool_admission()
            .expect("admitted tools");
        for (id, approved) in [("pending-call", false), ("approved-call", true)] {
            let prepared = tools
                .prepare(
                    zeroclaw_api::session_events::capture::ToolPreparationIntent {
                        proposed_name: "shell".into(),
                        provider_call_id: Some(id.into()),
                        name: "shell".into(),
                        arguments: serde_json::json!({"command":"true"}),
                        approval_requirement: "prompt".into(),
                        trusted_route: serde_json::json!({"fixture":true}),
                    },
                )
                .await?;
            prepared.request_approval(Default::default()).await?;
            if approved {
                prepared
                    .resolve_approval(
                        zeroclaw_api::channel::ChannelApprovalResponse::Approve,
                        "fixture_approved",
                        None,
                        serde_json::json!({"fixture":true}),
                    )
                    .await?;
            }
            // The host ends the turn without executing either call.
        }
        Ok("display notice + canonical model answer".into())
    }
}

struct PanicExecutor;

struct InterruptedExecutor {
    ready: Arc<tokio::sync::Notify>,
}
#[async_trait::async_trait]
impl TurnExecutorPort for InterruptedExecutor {
    async fn execute(
        &mut self,
        _: &str,
        admission: Arc<dyn ProviderAdmissionPort>,
    ) -> anyhow::Result<String> {
        use zeroclaw_api::session_events::capture::{
            ContextTransformIntent, ProviderDispatchIntent,
        };
        admission
            .begin_context_transform(ContextTransformIntent {
                reason_key: "fixture_transform".into(),
                algorithm_version: "fixture/v1".into(),
                budget: 1024,
            })
            .await?;
        admission
            .admit(ProviderDispatchIntent {
                requested_route: None,
                previous_attempt_event_id: None,
                tool_round_policy: zeroclaw_api::model_provider::ToolRoundPolicy::Batch,
                thinking: None,
                provider_ref: "fixture".into(),
                model: "fixture".into(),
                messages: vec![],
                tools: None,
                temperature: None,
            })
            .await?;
        self.ready.notify_one();
        std::future::pending().await
    }
}
struct LossProof;
impl crate::session::recovery::StreamExecutionLossVerifier for LossProof {
    fn verify_stream_loss(
        &self,
        _: &SessionStreamId,
        through: &EventCursor,
    ) -> Result<PayloadRecord, SessionError> {
        Ok(PayloadRecord::json(
            "session.evidence/v1",
            &serde_json::json!({
                "source":"fixture_execution_owner", "verified_no_writer":true, "through":through,
            }),
        )
        .map_err(AppendError::from)?)
    }
}
#[async_trait::async_trait]
impl TurnExecutorPort for PanicExecutor {
    async fn execute(
        &mut self,
        _: &str,
        _: Arc<dyn ProviderAdmissionPort>,
    ) -> anyhow::Result<String> {
        panic!("fixture executor panic")
    }
}
#[async_trait::async_trait]
impl TurnExecutorPort for Executor {
    async fn execute(
        &mut self,
        _: &str,
        _: Arc<dyn ProviderAdmissionPort>,
    ) -> anyhow::Result<String> {
        self.calls += 1;
        Ok("fixture answer".into())
    }
}

#[tokio::test]
async fn input_retry_returns_original_receipt_without_reexecuting() {
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
            &serde_json::json!({"revision":"fixture"}),
        )
        .expect("binding");
        let context =
            PayloadRecord::json("session.context/v1", &serde_json::json!({"messages":[]}))
                .expect("empty context");
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
        .expect("context");
        projection.branch_id = Some(branch.clone());
        let context_event_id = projection.event_id.clone();
        store
            .append_batch(AppendRequest {
                stream_id: stream.clone(),
                expected_head: EventCursor::beginning(stream.clone()),
                idempotency_key: "root".into(),
                events: vec![root, projection],
                payloads: vec![context, ingress.clone(), binding.clone()],
                erasure_reference_commitment: None,
            })
            .expect("initialize");
        let coordinator = Arc::new(SessionCoordinator::new(store));
        let command = UserTurnCommand {
            stream_id: stream.clone(),
            branch_id: branch,
            turn_id: TurnId::new(),
            message_id: MessageId::new(),
            ingress_key: "fixture-input-key".into(),
            parts: vec![EngineInputPart::Text("hello".into())],
            ingress_ref: StoredPayloadRef::from(&ingress),
            reply_route_ref: None,
            execution_binding_ref: StoredPayloadRef::from(&binding),
            context_event_id,
            input_payloads: vec![],
        };
        let mut executor = Executor { calls: 0 };
        let first = coordinator
            .execute_user_turn(
                command.clone(),
                PrincipalId::shared_operator(),
                Arc::new(Access),
                Arc::new(Access),
                &mut executor,
            )
            .await
            .expect("first");
        let second = coordinator
            .execute_user_turn(
                command.clone(),
                PrincipalId::shared_operator(),
                Arc::new(Access),
                Arc::new(Access),
                &mut executor,
            )
            .await
            .expect("retry");
        assert_eq!(first, second);
        assert_eq!(executor.calls, 1);
        let mut changed = command.clone();
        changed.parts = vec![EngineInputPart::Text("different".into())];
        assert!(matches!(
            coordinator
                .execute_user_turn(
                    changed,
                    PrincipalId::shared_operator(),
                    Arc::new(Access),
                    Arc::new(Access),
                    &mut executor
                )
                .await,
            Err(SessionError::Append(AppendError::IdempotencyConflict(_)))
        ));
        assert_eq!(executor.calls, 1);
        // Attachment order, registration identity and retry commitment are
        // checked at the real atomic admission boundary on both backends.
        let file = zeroclaw_api::session_events::capture::EngineFileInput {
            file_id: "fixture-file-owner-id".into(),
            registration: serde_json::json!({"source":"gateway_attachment", "name":"fixture.txt", "path":"/fixture.txt"}),
            representation: PayloadRecord::json(
                "session.attachment-reference/v1",
                &zeroclaw_api::media::AttachmentReference {
                    kind: zeroclaw_api::media::AttachmentKind::Document,
                    target: "/fixture.txt".into(),
                },
            )
            .expect("representation"),
            policy_evidence: serde_json::json!({"source":"fixture_file_owner", "revision":1}),
        };
        let mut attachments = command.clone();
        attachments.turn_id = TurnId::new();
        attachments.message_id = MessageId::new();
        attachments.ingress_key = "fixture-attachments".into();
        attachments.parts.extend([
            EngineInputPart::File(file.clone()),
            EngineInputPart::File(file.clone()),
        ]);
        let original = coordinator
            .execute_user_turn(
                attachments.clone(),
                PrincipalId::shared_operator(),
                Arc::new(Access),
                Arc::new(Access),
                &mut executor,
            )
            .await
            .expect("attachment admission");
        assert_eq!(
            original,
            coordinator
                .execute_user_turn(
                    attachments.clone(),
                    PrincipalId::shared_operator(),
                    Arc::new(Access),
                    Arc::new(Access),
                    &mut executor
                )
                .await
                .expect("attachment retry")
        );
        for mutation in ["bytes", "metadata", "policy"] {
            let mut changed = attachments.clone();
            let EngineInputPart::File(changed_file) = &mut changed.parts[2] else {
                panic!("fixture file part");
            };
            match mutation {
                "bytes" => {
                    changed_file.representation = PayloadRecord::new(
                        PayloadId::new(),
                        "session.bytes/v1".into(),
                        b"different bytes".to_vec(),
                    )
                    .expect("changed")
                }
                "metadata" => {
                    changed_file.registration["name"] = serde_json::json!("different.txt")
                }
                _ => changed_file.policy_evidence["revision"] = serde_json::json!(2),
            }
            assert!(matches!(
                coordinator
                    .execute_user_turn(
                        changed,
                        PrincipalId::shared_operator(),
                        Arc::new(Access),
                        Arc::new(Access),
                        &mut executor
                    )
                    .await,
                Err(SessionError::Append(AppendError::IdempotencyConflict(_)))
            ));
        }
        let mut reuse = attachments;
        reuse.turn_id = TurnId::new();
        reuse.message_id = MessageId::new();
        reuse.ingress_key = "fixture-file-reuse".into();
        reuse.parts = vec![EngineInputPart::File(file.clone())];
        coordinator
            .execute_user_turn(
                reuse.clone(),
                PrincipalId::shared_operator(),
                Arc::new(Access),
                Arc::new(Access),
                &mut executor,
            )
            .await
            .expect("reuse canonical file identity");
        coordinator
            .execute_user_turn(
                reuse,
                PrincipalId::shared_operator(),
                Arc::new(Access),
                Arc::new(Access),
                &mut executor,
            )
            .await
            .expect("file-only retry");
        let mut interleaved = command.clone();
        interleaved.turn_id = TurnId::new();
        interleaved.message_id = MessageId::new();
        interleaved.ingress_key = "fixture-interleaved".into();
        interleaved.parts = vec![
            EngineInputPart::Text("before".into()),
            EngineInputPart::File(file.clone()),
            EngineInputPart::Text("after".into()),
        ];
        coordinator
            .execute_user_turn(
                interleaved.clone(),
                PrincipalId::shared_operator(),
                Arc::new(Access),
                Arc::new(Access),
                &mut executor,
            )
            .await
            .expect("interleaved admission");
        coordinator
            .execute_user_turn(
                interleaved.clone(),
                PrincipalId::shared_operator(),
                Arc::new(Access),
                Arc::new(Access),
                &mut executor,
            )
            .await
            .expect("interleaved retry");
        interleaved.parts.swap(0, 1);
        assert!(matches!(
            coordinator
                .execute_user_turn(
                    interleaved,
                    PrincipalId::shared_operator(),
                    Arc::new(Access),
                    Arc::new(Access),
                    &mut executor
                )
                .await,
            Err(SessionError::Append(AppendError::IdempotencyConflict(_)))
        ));
        let file_events = coordinator
            .read_page(&stream, &EventCursor::beginning(stream.clone()), None, 256)
            .expect("file events")
            .events;
        assert_eq!(
            file_events
                .iter()
                .filter(|event| event.body.kind() == EventKind::FileRegistered)
                .count(),
            1
        );
        assert_eq!(
            file_events
                .iter()
                .filter_map(|event| match event.body.data {
                    EventData::FileAttached { part_index, .. } => Some(part_index),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            [1, 2, 0, 1]
        );
        let projected = coordinator
            .query(PrincipalId::shared_operator(), Arc::new(Access))
            .messages(stream.clone())
            .await
            .expect("file message projection");
        assert!(
            projected
                .iter()
                .any(|message| message.files == ["fixture-file-owner-id", "fixture-file-owner-id"])
        );
        let attached = projected
            .iter()
            .find(|message| message.files.len() == 2)
            .expect("file message");
        assert_eq!(attached.attachments.len(), 2);
        assert!(
            attached
                .attachments
                .iter()
                .all(|reference| reference.target == "/fixture.txt")
        );
        // Channel ingress captures bytes and keeps the typed reference in the
        // per-input metadata. Reusing a FileId must use the current input's
        // reference, not its first registration or a later observation.
        let mut channel = command.clone();
        channel.turn_id = TurnId::new();
        channel.message_id = MessageId::new();
        channel.ingress_key = "fixture-channel-attachment".into();
        let channel_reference = zeroclaw_api::media::AttachmentReference {
            kind: zeroclaw_api::media::AttachmentKind::Document,
            target: "/channel-file.txt".into(),
        };
        let mut channel_file = file.clone();
        channel_file.registration = serde_json::json!({"reference": channel_reference});
        channel_file.representation = PayloadRecord::new(
            PayloadId::new(),
            "session.bytes/v1".into(),
            b"original bytes".to_vec(),
        )
        .expect("channel bytes");
        channel.parts.push(EngineInputPart::File(channel_file));
        coordinator
            .execute_user_turn(
                channel,
                PrincipalId::shared_operator(),
                Arc::new(Access),
                Arc::new(Access),
                &mut executor,
            )
            .await
            .expect("channel attachment turn");
        let projected = coordinator
            .query(PrincipalId::shared_operator(), Arc::new(Access))
            .messages(stream.clone())
            .await
            .expect("channel history");
        assert!(
            projected
                .iter()
                .any(|message| message.attachments.as_slice()
                    == std::slice::from_ref(&channel_reference))
        );
        assert_eq!(
            projected
                .iter()
                .find(|message| message.files.len() == 2)
                .expect("original file history")
                .attachments
                .len(),
            2
        );
        // The second input is durable while the first turn is still active.
        // Its settlement must ignore the preceding turn's later terminal event.
        let mut queued_first = command.clone();
        queued_first.turn_id = TurnId::new();
        queued_first.message_id = MessageId::new();
        queued_first.ingress_key = "queued-first".into();
        let mut queued_second = command.clone();
        queued_second.turn_id = TurnId::new();
        queued_second.message_id = MessageId::new();
        queued_second.ingress_key = "queued-second".into();
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let first_owner = coordinator.clone();
        let first_entered = entered.clone();
        let first_release = release.clone();
        let active = zeroclaw_spawn::spawn!(async move {
            first_owner
                .execute_user_turn(
                    queued_first,
                    PrincipalId::shared_operator(),
                    Arc::new(Access),
                    Arc::new(Access),
                    &mut QueuedExecutor {
                        entered: first_entered,
                        release: first_release,
                    },
                )
                .await
        });
        entered.notified().await;
        let second_owner = coordinator.clone();
        let queued = zeroclaw_spawn::spawn!(async move {
            second_owner
                .execute_user_turn(
                    queued_second,
                    PrincipalId::shared_operator(),
                    Arc::new(Access),
                    Arc::new(Access),
                    &mut Executor { calls: 0 },
                )
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if coordinator
                    .store
                    .lookup_receipt(&stream, "queued-second")
                    .expect("queued receipt")
                    .is_some()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("second input admitted before first terminal");
        release.notify_one();
        active
            .await
            .expect("first task")
            .expect("first queued outcome");
        queued
            .await
            .expect("second task")
            .expect("second queued outcome ignores another turn's terminal");
        let mut next = command.clone();
        next.turn_id = TurnId::new();
        next.message_id = MessageId::new();
        next.ingress_key = "fixture-context".into();
        let context_receipt = coordinator
            .execute_user_turn(
                next,
                PrincipalId::shared_operator(),
                Arc::new(Access),
                Arc::new(Access),
                &mut ContextExecutor,
            )
            .await
            .expect("context turn");
        let context_outcome = coordinator
            .read_page(
                &stream,
                &EventCursor {
                    stream_id: stream.clone(),
                    sequence: context_receipt.first.sequence - 1,
                },
                Some(&context_receipt.last),
                256,
            )
            .expect("context outcome");
        let EventData::TurnCommitted { completion_ref } = &context_outcome
            .events
            .last()
            .expect("context terminal")
            .body
            .data
        else {
            panic!("context execution must commit a completion");
        };
        let PayloadRead::Present(completion) = coordinator
            .store
            .read_payload(&completion_ref.payload_id)
            .expect("context completion")
        else {
            panic!("context completion is missing");
        };
        let completion: TurnCompletion =
            serde_json::from_slice(&completion.body).expect("context completion body");
        assert!(
            matches!(completion, TurnCompletion::Success { .. }),
            "context fixture must complete successfully: {completion:?}"
        );
        let mut interrupted = command.clone();
        interrupted.turn_id = TurnId::new();
        interrupted.message_id = MessageId::new();
        interrupted.ingress_key = "fixture-interrupted".into();
        let ready = Arc::new(tokio::sync::Notify::new());
        let mut executor = InterruptedExecutor {
            ready: Arc::clone(&ready),
        };
        let mut execution = Box::pin(coordinator.execute_user_turn(
            interrupted.clone(),
            PrincipalId::shared_operator(),
            Arc::new(Access),
            Arc::new(Access),
            &mut executor,
        ));
        tokio::select! {
            () = ready.notified() => {}
            result = &mut execution => panic!("interrupted fixture unexpectedly settled: {result:?}"),
        }
        drop(execution);
        let recovered = coordinator
            .recover_interrupted_stream(stream.clone(), "fixture-loss".into(), Arc::new(LossProof))
            .await
            .expect("recover")
            .expect("receipt");
        assert_eq!(
            Some(recovered),
            coordinator
                .recover_interrupted_stream(
                    stream.clone(),
                    "fixture-loss".into(),
                    Arc::new(LossProof)
                )
                .await
                .expect("recovery retry")
        );
        assert!(
            coordinator
                .execute_user_turn(
                    interrupted.clone(),
                    PrincipalId::shared_operator(),
                    Arc::new(Access),
                    Arc::new(Access),
                    &mut executor
                )
                .await
                .is_err(),
            "interrupted effects cannot execute again"
        );
        let page = coordinator
            .read_page(&stream, &EventCursor::beginning(stream.clone()), None, 256)
            .expect("recovered history");
        let interrupted_events: Vec<_> = page
            .events
            .iter()
            .filter(|event| event.turn_id.as_ref() == Some(&interrupted.turn_id))
            .collect();
        assert!(
            interrupted_events
                .iter()
                .any(|event| event.body.kind() == EventKind::ContextTransformCancelled)
        );
        assert!(
            interrupted_events
                .iter()
                .any(|event| event.body.kind() == EventKind::InvocationOutcomeUnknown)
        );
        assert_eq!(
            interrupted_events.last().expect("terminal").body.kind(),
            EventKind::TurnOutcomeUnknown
        );
        let events = coordinator
            .read_page(&stream, &EventCursor::beginning(stream.clone()), None, 256)
            .expect("history")
            .events;
        let selections: Vec<_> = events
            .iter()
            .filter(|event| event.body.kind() == EventKind::SessionModelOverrideChanged)
            .collect();
        assert_eq!(selections.len(), 3);
        assert!(matches!(
            &selections[0].body.data,
            EventData::SessionModelOverrideChanged {
                resolution: ModelResolution::Applied {
                    previous_selection_event_id: None
                }
            }
        ));
        assert!(matches!(
            &selections[1].body.data,
            EventData::SessionModelOverrideChanged {
                resolution: ModelResolution::Rejected { .. }
            }
        ));
        assert!(
            matches!(&selections[2].body.data,EventData::SessionModelOverrideChanged {resolution:ModelResolution::Applied {previous_selection_event_id:Some(id)}} if id==&selections[0].event_id)
        );
        for selection in selections {
            let cause = events
                .iter()
                .find(|event| Some(&event.event_id) == selection.causation_id.as_ref())
                .expect("control origin");
            assert_eq!(cause.body.kind(), EventKind::ControlInputRecorded);
        }
        let installed = events
            .iter()
            .rev()
            .find(|event| {
                matches!(
                    event.body.data,
                    EventData::ContextProjectionCreated {
                        transform_started_event_id: Some(_),
                        ..
                    }
                )
            })
            .expect("installed transform");
        let prepared = events
            .iter()
            .find(|event| matches!(event.body.data, EventData::ProviderRequestPrepared { .. }))
            .expect("provider request");
        assert!(
            matches!(&prepared.body.data,EventData::ProviderRequestPrepared {context_event_id,..} if context_event_id==&installed.event_id)
        );
        let content = events
            .iter()
            .rev()
            .find(|event| matches!(event.body.data, EventData::AssistantContentRecorded { .. }))
            .expect("assistant");
        assert!(
            matches!(&content.body.data,EventData::AssistantContentRecorded {provenance_refs,..} if provenance_refs.len()==3)
        );
        assert_eq!(events.iter().filter(|event| matches!(&event.body.data,
            EventData::ApprovalInvalidated {reason_key,..} if reason_key=="approval_turn_execution_finished")).count(), 2);
        assert!(
            !events
                .iter()
                .any(|event| event.body.kind() == EventKind::ToolEffectStarted)
        );
        let mut panicked = command;
        panicked.turn_id = TurnId::new();
        panicked.message_id = MessageId::new();
        panicked.ingress_key = "fixture-panic".into();
        let receipt = coordinator
            .execute_user_turn(
                panicked,
                PrincipalId::shared_operator(),
                Arc::new(Access),
                Arc::new(Access),
                &mut PanicExecutor,
            )
            .await
            .expect("panic has a durable outcome");
        let page = coordinator
            .read_page(
                &stream,
                &EventCursor {
                    stream_id: stream.clone(),
                    sequence: receipt.first.sequence - 1,
                },
                Some(&receipt.last),
                256,
            )
            .expect("panic outcome");
        assert!(
            matches!(
                &page.events.last().expect("terminal").body.data,
                EventData::TurnCommitted { .. }
            ),
            "a local executor panic without an external invocation is failed, not unknown"
        );
    }
}
