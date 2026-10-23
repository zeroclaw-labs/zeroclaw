use super::super::engine::{EngineSession, EngineSessionAuthority};
use super::super::query::{SessionAccess, SessionAccessPolicy};
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use zeroclaw_api::model_provider::ConversationMessage;
use zeroclaw_api::principal::PrincipalId;
use zeroclaw_api::session_events::capture::*;

struct Authority(AtomicBool);
impl SessionAccessPolicy for Authority {
    fn authorize_actor(
        &self,
        actor: &EventActor,
        stream: &SessionStreamId,
        access: SessionAccess,
    ) -> Result<(), SessionError> {
        if matches!(actor,EventActor::System {component} if component=="fixture_system")
            && !matches!(access, SessionAccess::Erase)
        {
            if self.0.load(Ordering::SeqCst) {
                Ok(())
            } else {
                Err(SessionError::PermissionDenied("revoked".into()))
            }
        } else if let EventActor::Principal { principal_id } = actor {
            SessionAccessPolicy::authorize(self, &PrincipalId(principal_id.clone()), stream, access)
        } else {
            Err(SessionError::PermissionDenied("system scope denied".into()))
        }
    }
    fn authorize(
        &self,
        _: &PrincipalId,
        _: &SessionStreamId,
        _: SessionAccess,
    ) -> Result<(), SessionError> {
        if self.0.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(SessionError::PermissionDenied("revoked".into()))
        }
    }
}
impl PermissionResolver for Authority {
    fn authorize(&self, intent: &EffectIntent) -> Result<(), SessionError> {
        SessionAccessPolicy::authorize(
            self,
            &PrincipalId::shared_operator(),
            &intent.stream_id,
            SessionAccess::Write,
        )
    }
    fn authorize_context(
        &self,
        stream: &SessionStreamId,
        _: &BranchId,
    ) -> Result<(), SessionError> {
        SessionAccessPolicy::authorize(
            self,
            &PrincipalId::shared_operator(),
            stream,
            SessionAccess::Write,
        )
    }
    fn child_authority(
        &self,
        target: &str,
        _: &SessionStreamId,
    ) -> Result<Arc<dyn EngineSessionAuthority>, SessionError> {
        if target == "denied-fixture" {
            return Err(SessionError::PermissionDenied(
                "fixture child denied".into(),
            ));
        }
        Ok(Arc::new(Authority(AtomicBool::new(
            target != "actor-denied-fixture",
        ))))
    }
}
impl EngineSessionAuthority for Authority {
    fn execution_binding(&self, input: &EngineTurnInput) -> Result<PayloadRecord, SessionError> {
        let mut binding = serde_json::json!({"source":"fixture"});
        if let Some(workspace) = &input.execution_workspace {
            binding["execution_context"] = serde_json::json!({"cwd":workspace});
        }
        Ok(
            PayloadRecord::json("session.execution-binding/v1", &binding)
                .map_err(AppendError::from)?,
        )
    }
    fn maintenance_binding(&self) -> Result<PayloadRecord, SessionError> {
        Ok(PayloadRecord::json(
            "session.execution-binding/v1",
            &serde_json::json!({"source":"fixture"}),
        )
        .map_err(AppendError::from)?)
    }
    fn diagnostic(&self, error: &anyhow::Error) -> Option<String> {
        Some(format!("{error:#}"))
    }
}
fn session(directory: &std::path::Path, backend: &str) -> (EngineSession, Arc<Authority>) {
    let store =
        zeroclaw_infra::session_events::open_event_store(directory, backend).expect("store");
    let owner = Arc::new(SessionCoordinator::new(store).with_search(Arc::new(
        zeroclaw_infra::session_events::search::FullTextSearch,
    )));
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
    let initial = PayloadRecord::json(
        "session.context/v1",
        &serde_json::json!({"messages":[],"history_has_trim_breadcrumb":false}),
    )
    .expect("context");
    let mut projection = SessionEvent::typed(
        stream.clone(),
        EventData::ContextProjectionCreated {
            source_ranges: vec![],
            summary_ref: None,
            budget: 0,
            reason_key: "initial".into(),
            algorithm_version: "fixture/v1".into(),
            result_ref: StoredPayloadRef::from(&initial),
            summary_source_refs: vec![],
            transform_started_event_id: None,
        },
        EventActor::default(),
    )
    .expect("projection");
    projection.branch_id = Some(branch.clone());
    owner
        .append(AppendRequest {
            stream_id: stream.clone(),
            expected_head: EventCursor::beginning(stream.clone()),
            idempotency_key: "root".into(),
            events: vec![root, projection],
            payloads: vec![initial],
            erasure_reference_commitment: None,
        })
        .expect("create");
    let authority = Arc::new(Authority(AtomicBool::new(true)));
    (
        EngineSession::new(
            owner,
            stream,
            branch,
            PrincipalId::shared_operator(),
            authority.clone(),
        ),
        authority,
    )
}
fn input() -> EngineTurnInput {
    EngineTurnInput {
        execution_workspace: None,
        turn_id: TurnId::new().to_string(),
        agent_alias: Some("fixture".into()),
        ingress: zeroclaw_api::ingress::IngressContext::agent_direct(),
        reply_route: None,
        parts: zeroclaw_api::session_events::capture::EngineInputPart::text_and_files(
            "parent".into(),
            vec![],
        ),
        provider_ref: "fixture".into(),
        model: "fixture".into(),
    }
}

#[tokio::test]
async fn execution_metadata_serializes_with_commands_and_rechecks_live_authority() {
    for backend in ["sqlite", "jsonl"] {
        let directory = tempfile::tempdir().expect("directory");
        let (session, authority) = session(directory.path(), backend);
        futures_util::future::try_join_all((0..24).map(|index| {
            let session = &session;
            async move {
                if index % 2 == 0 {
                    session
                        .observe_metadata(SessionMetadataObservation::ContextUsage {
                            input_tokens: Some(index),
                        })
                        .await
                } else {
                    session
                        .record_control(serde_json::json!({"observed":index}))
                        .await
                }
            }
        }))
        .await
        .expect("metadata and execution commands share serialization");
        session
            .observe_metadata(SessionMetadataObservation::ContextUsage { input_tokens: None })
            .await
            .expect("clear unknown context count");
        session
            .observe_metadata(SessionMetadataObservation::Plan {
                entries: serde_json::from_value(serde_json::json!([
                    {"content":"review token=sk-plan-fixture-secret", "status":"in_progress"}
                ]))
                .expect("plan"),
            })
            .await
            .expect("plan observation");
        let page = session
            .owner
            .store
            .read_page(
                &session.stream,
                &EventCursor::beginning(session.stream.clone()),
                None,
                128,
                1024 * 1024,
            )
            .expect("events");
        let mut counts = Vec::new();
        let mut plan = None;
        for event in &page.events {
            if let EventData::SessionMetadataChanged {
                field,
                new_value_ref,
            } = &event.body.data
            {
                let PayloadRead::Present(value) = session
                    .owner
                    .store
                    .read_payload(&new_value_ref.payload_id)
                    .expect("metadata")
                else {
                    panic!("metadata unavailable");
                };
                if field == "token_count" {
                    counts.push(serde_json::from_slice::<u64>(&value.body).expect("count"));
                } else if field == "plan" {
                    plan = Some(String::from_utf8(value.body).expect("plan bytes"));
                }
            }
        }
        assert_eq!(counts.len(), 13);
        assert_eq!(counts.last(), Some(&0));
        let plan = plan.expect("durable plan");
        assert!(plan.contains("review"));
        assert!(plan.contains("sk-plan-fixture-secret"));
        let before = session.owner.head(&session.stream).expect("head");
        authority.0.store(false, Ordering::SeqCst);
        assert!(
            session
                .observe_metadata(SessionMetadataObservation::ContextUsage {
                    input_tokens: Some(100)
                })
                .await
                .is_err()
        );
        assert_eq!(session.owner.head(&session.stream).expect("head"), before);
    }
}

#[tokio::test]
async fn stale_maintenance_source_is_rejected_before_any_event_is_appended() {
    use super::super::context::ContextMaintenanceCommand;
    for backend in ["sqlite", "jsonl"] {
        let directory = tempfile::tempdir().expect("directory");
        let (session, authority) = session(directory.path(), backend);
        let old = session.latest_context().await.expect("original projection");
        session
            .replace_context(
                ContextSnapshot {
                    messages: vec![zeroclaw_api::model_provider::ConversationMessage::Chat(
                        ChatMessage::user("newer context"),
                    )],
                    history_has_trim_breadcrumb: Some(false),
                },
                "newer_source",
            )
            .await
            .expect("newer projection");
        let before = session.owner.head(&session.stream).expect("head");
        let result = session
            .owner
            .replace_context(
                ContextMaintenanceCommand {
                    stream: session.stream.clone(),
                    branch: session.branch.clone(),
                    input_projection: old.event_id,
                    execution_binding: authority.maintenance_binding().expect("binding"),
                    intent: ContextTransformIntent {
                        reason_key: "stale_restore".into(),
                        algorithm_version: "fixture/v1".into(),
                        budget: 0,
                    },
                },
                ContextSnapshot {
                    messages: vec![],
                    history_has_trim_breadcrumb: Some(false),
                },
                authority,
            )
            .await;
        assert!(
            matches!(result, Err(SessionError::StaleTarget(_))),
            "{result:?}"
        );
        assert_eq!(
            session
                .owner
                .head(&session.stream)
                .expect("head after rejection"),
            before
        );
    }
}

#[tokio::test]
async fn complete_context_restores_tool_pairing_and_reasoning_without_disclosure_grant() {
    use super::super::query::PayloadSelector;
    use zeroclaw_api::model_provider::{ConversationMessage, ToolCall, ToolResultMessage};
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::TempDir::new().expect("directory");
        let (session, authority) = session(dir.path(), backend);
        let snapshot = ContextSnapshot {
            messages: vec![
                ConversationMessage::Chat(ChatMessage::user("read")),
                ConversationMessage::AssistantToolCalls {
                    text: Some("checking".into()),
                    tool_calls: vec![ToolCall {
                        id: "call-1".into(),
                        name: "file_read".into(),
                        arguments: "{}".into(),
                        extra_content: Some(
                            serde_json::json!({"thought_signature":"fixture-signature"}),
                        ),
                    }],
                    reasoning_content: Some("fixture reasoning continuation".into()),
                },
                ConversationMessage::ToolResults(vec![ToolResultMessage {
                    tool_call_id: "call-1".into(),
                    tool_name: "file_read".into(),
                    content: "bytes".into(),
                }]),
                ConversationMessage::Chat(ChatMessage::assistant("done")),
            ],
            history_has_trim_breadcrumb: Some(true),
        };
        session
            .replace_context(snapshot.clone(), "full_context_fixture")
            .await
            .expect("install");
        let restored = session
            .restore_context()
            .await
            .expect("restore")
            .expect("snapshot");
        assert_eq!(
            canonical_json(&restored).expect("json"),
            canonical_json(&snapshot).expect("json")
        );
        let projection = session.latest_context().await.expect("projection");
        assert!(
            session
                .query()
                .read_payload(
                    session.stream.clone(),
                    PayloadSelector {
                        event_sequence: projection.sequence,
                        reference_path: vec![0, 0],
                    },
                    1024 * 1024
                )
                .await
                .is_err(),
            "ordinary history permission must not disclose context reasoning"
        );
        authority.0.store(false, Ordering::SeqCst);
        assert!(
            session.restore_context().await.is_err(),
            "internal restore must recheck live authority"
        );
    }
}

#[tokio::test]
async fn inherited_model_selection_round_trips_without_freezing_configured_route() {
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::TempDir::new().expect("directory");
        let (mut port, authority) = session(dir.path(), backend);
        port.observe_model_change(
            ModelOverrideSelection::Set {
                provider_ref: "old.configured".into(),
                model: "old-model".into(),
                thinking: None,
                route_mode: ModelRouteMode::Manual,
            },
            ModelChangeDisposition::Applied,
        )
        .await
        .expect("old explicit selection");
        for selection in [
            ModelOverrideSelection::Inherit {
                thinking: Some("high".into()),
            },
            ModelOverrideSelection::Inherit { thinking: None },
            ModelOverrideSelection::Clear,
        ] {
            port.observe_model_change(selection.clone(), ModelChangeDisposition::Applied)
                .await
                .expect("inherit configured route");
            assert_eq!(
                port.restore_model_override().await.unwrap(),
                Some(selection.clone())
            );
            let history = port.owner.stream_history(&port.stream).unwrap();
            let control = history
                .iter()
                .rev()
                .find(|event| event.body.kind() == EventKind::ControlInputRecorded)
                .unwrap();
            let EventData::ControlInputRecorded { control_ref, .. } = &control.body.data else {
                panic!("control payload");
            };
            let PayloadRead::Present(control) = port
                .owner
                .store
                .read_payload(&control_ref.payload_id)
                .unwrap()
            else {
                panic!("retained control");
            };
            let control: serde_json::Value = serde_json::from_slice(&control.body).unwrap();
            assert!(control["selection"].get("provider_ref").is_none());
            assert!(control["selection"].get("model").is_none());
            assert!(control["selection"].get("route_mode").is_none());
            let stream = port.stream.clone();
            let branch = port.branch.clone();
            drop(port);
            port = EngineSession::new(
                Arc::new(SessionCoordinator::new(
                    zeroclaw_infra::session_events::open_event_store(dir.path(), backend).unwrap(),
                )),
                stream,
                branch,
                PrincipalId::shared_operator(),
                authority.clone(),
            );
            assert_eq!(
                port.restore_model_override().await.unwrap(),
                Some(selection)
            );
        }
    }
}

#[tokio::test]
async fn turn_model_selection_keeps_session_mode_across_turns_branches_and_reopen() {
    async fn capture_answer(model: &str) -> anyhow::Result<()> {
        let observer = current_provider_admission()
            .expect("provider scope")
            .admit(ProviderDispatchIntent {
                requested_route: None,
                previous_attempt_event_id: None,
                provider_ref: "fixture".into(),
                model: model.into(),
                messages: vec![ChatMessage::user("fixture")],
                tools: None,
                temperature: None,
                thinking: None,
                tool_round_policy: zeroclaw_api::model_provider::ToolRoundPolicy::Single,
            })
            .await?;
        let response = PayloadRecord::json(
            "session.response/v1",
            &ProviderResponsePayload {
                blocks: vec![],
                completion: ContentCompletion::Complete,
                finish_evidence_ref: None,
                usage_ref: None,
                reasoning_absence: Some("not_returned".into()),
            },
        )?;
        observer
            .observe(
                EventData::ProviderAttemptCompleted {
                    response_ref: StoredPayloadRef::from(&response),
                    served_attribution_ref: None,
                    last_progress_event_id: None,
                },
                vec![response],
            )
            .await?;
        Ok(())
    }

    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::TempDir::new().expect("directory");
        let (port, authority) = session(dir.path(), backend);
        let baseline = ModelOverrideSelection::Set {
            provider_ref: "fixture".into(),
            model: "baseline".into(),
            thinking: Some("high".into()),
            route_mode: ModelRouteMode::Automatic,
        };
        let temporary = ModelOverrideSelection::Set {
            provider_ref: "fixture".into(),
            model: "temporary".into(),
            thinking: Some("high".into()),
            route_mode: ModelRouteMode::Automatic,
        };
        let initial = baseline.clone();
        port.execute(
            input(),
            Box::pin(async move {
                current_provider_admission()
                    .expect("initial scope")
                    .observe_model_change(initial, ModelChangeDisposition::Applied)
                    .await?;
                Ok("initial selection".into())
            }),
        )
        .await
        .expect("baseline selection");
        let initial_head = port.owner.head(&port.stream).expect("head");
        assert!(
            port.observe_model_change(temporary.clone(), ModelChangeDisposition::AppliedForTurn)
                .await
                .is_err(),
            "idle commands cannot install a turn-local selection"
        );
        assert_eq!(port.owner.head(&port.stream).unwrap(), initial_head);

        let (send, receive) = tokio::sync::oneshot::channel();
        let change = temporary.clone();
        port.execute(
            input(),
            Box::pin(async move {
                capture_answer("baseline").await?;
                let admission = current_provider_admission().expect("admission");
                admission
                    .observe_model_change(change, ModelChangeDisposition::AppliedForTurn)
                    .await?;
                capture_answer("temporary").await?;
                send.send(admission)
                    .map_err(|_| anyhow::Error::msg("fixture receiver closed"))?;
                Ok("first turn".into())
            }),
        )
        .await
        .expect("first turn");
        let late = receive.await.expect("bound admission");
        let ended_head = port.owner.head(&port.stream).unwrap();
        for disposition in [
            ModelChangeDisposition::Applied,
            ModelChangeDisposition::AppliedForTurn,
        ] {
            assert!(
                late.observe_model_change(temporary.clone(), disposition)
                    .await
                    .is_err(),
                "a retained admission cannot change a finished turn"
            );
            assert_eq!(port.owner.head(&port.stream).unwrap(), ended_head);
        }
        late.observe_model_change(
            temporary,
            ModelChangeDisposition::Rejected {
                reason_key: "late_model_switch_rejected".into(),
                diagnostic: Some("late diagnostic remains observable".into()),
            },
        )
        .await
        .expect("late rejection is diagnostic only");
        drop(late);

        assert_eq!(
            port.restore_model_override().await.unwrap(),
            Some(baseline.clone())
        );
        let child = port.fork(BranchId::new(), ended_head).await.expect("fork");
        assert_eq!(
            child.restore_model_override().await.unwrap(),
            Some(baseline.clone())
        );
        for bound in [&port, &child] {
            let current = baseline.clone();
            let persistent = bound.branch == port.branch;
            bound
                .execute(
                    input(),
                    Box::pin(async move {
                        if persistent {
                            current_provider_admission()
                                .expect("later scope")
                                .observe_model_change(current, ModelChangeDisposition::Applied)
                                .await?;
                        }
                        capture_answer("baseline").await?;
                        Ok("later turn".into())
                    }),
                )
                .await
                .expect("later turn");
        }

        let history = port.owner.stream_history(&port.stream).expect("history");
        let persistent: Vec<_> = history
            .iter()
            .filter(|event| {
                matches!(
                    event.body.data,
                    EventData::SessionModelOverrideChanged {
                        resolution: ModelResolution::Applied { .. }
                    }
                )
            })
            .collect();
        assert_eq!(persistent.len(), 2);
        assert!(
            matches!(&persistent[1].body.data, EventData::SessionModelOverrideChanged {
            resolution: ModelResolution::Applied { previous_selection_event_id: Some(previous) }
        } if previous == &persistent[0].event_id)
        );
        let turn_selection = history
            .iter()
            .find(|event| {
                matches!(
                    event.body.data,
                    EventData::SessionModelOverrideChanged {
                        resolution: ModelResolution::AppliedForTurn
                    }
                )
            })
            .expect("turn selection");
        let request_selections: Vec<_> = history
            .iter()
            .filter_map(|event| {
                let EventData::ProviderRequestPrepared {
                    route_attribution_ref,
                    request_ref,
                    ..
                } = &event.body.data
                else {
                    return None;
                };
                let PayloadRead::Present(route) = port
                    .owner
                    .store
                    .read_payload(&route_attribution_ref.payload_id)
                    .unwrap()
                else {
                    panic!("route");
                };
                let PayloadRead::Present(request) = port
                    .owner
                    .store
                    .read_payload(&request_ref.payload_id)
                    .unwrap()
                else {
                    panic!("request");
                };
                let request: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                assert_eq!(request["tool_round_policy"], "single");
                let route: serde_json::Value = serde_json::from_slice(&route.body).unwrap();
                Some(route["selection_event_id"].clone())
            })
            .collect();
        assert_eq!(
            request_selections,
            vec![
                serde_json::json!(persistent[0].event_id),
                serde_json::json!(turn_selection.event_id),
                serde_json::json!(persistent[1].event_id),
                serde_json::json!(persistent[0].event_id),
            ]
        );

        for (bound, invalid_previous) in [
            (&port, &turn_selection.event_id),
            (&child, &persistent[1].event_id),
        ] {
            let (control, payloads) = bound
                .control_input(serde_json::json!({
                    "command": "model_override", "selection": baseline
                }))
                .unwrap();
            let mut invalid = SessionEvent::typed(
                bound.stream.clone(),
                EventData::SessionModelOverrideChanged {
                    resolution: ModelResolution::Applied {
                        previous_selection_event_id: Some(invalid_previous.clone()),
                    },
                },
                control.actor.clone(),
            )
            .unwrap();
            invalid.branch_id = Some(bound.branch.clone());
            invalid.causation_id = Some(control.event_id.clone());
            let head = bound.owner.head(&bound.stream).unwrap();
            assert!(
                bound
                    .owner
                    .store
                    .append_batch(AppendRequest {
                        stream_id: bound.stream.clone(),
                        expected_head: head.clone(),
                        idempotency_key: format!("invalid-previous:{}", invalid.event_id),
                        events: vec![control, invalid],
                        payloads,
                        erasure_reference_commitment: None,
                    })
                    .is_err(),
                "temporary or unfrozen parent selections cannot become the persistent predecessor"
            );
            assert_eq!(bound.owner.head(&bound.stream).unwrap(), head);
        }

        // Raw append must enforce the lifecycle independently of the capture port.
        for turn in [None, turn_selection.turn_id.clone(), Some(TurnId::new())] {
            for resolution in [
                ModelResolution::Applied {
                    previous_selection_event_id: Some(persistent[1].event_id.clone()),
                },
                ModelResolution::AppliedForTurn,
            ] {
                let mut invalid = SessionEvent::typed(
                    port.stream.clone(),
                    EventData::SessionModelOverrideChanged { resolution },
                    EventActor::default(),
                )
                .unwrap();
                invalid.branch_id = Some(port.branch.clone());
                invalid.turn_id = turn.clone();
                invalid.causation_id = turn_selection.causation_id.clone();
                let head = port.owner.head(&port.stream).unwrap();
                assert!(
                    port.owner
                        .store
                        .append_batch(AppendRequest {
                            stream_id: port.stream.clone(),
                            expected_head: head.clone(),
                            idempotency_key: format!("invalid-model:{}", invalid.event_id),
                            events: vec![invalid],
                            payloads: vec![],
                            erasure_reference_commitment: None,
                        })
                        .is_err()
                );
                assert_eq!(port.owner.head(&port.stream).unwrap(), head);
            }
        }

        let stream = port.stream.clone();
        let branch = port.branch.clone();
        drop(child);
        drop(port);
        let reopened = EngineSession::new(
            Arc::new(SessionCoordinator::new(
                zeroclaw_infra::session_events::open_event_store(dir.path(), backend).unwrap(),
            )),
            stream,
            branch,
            PrincipalId::shared_operator(),
            authority,
        );
        assert_eq!(
            reopened.restore_model_override().await.unwrap(),
            Some(baseline)
        );
    }
}

#[tokio::test]
async fn cancelled_turn_cannot_apply_late_persistent_or_temporary_model_selection() {
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::TempDir::new().expect("directory");
        let (port, _) = session(dir.path(), backend);
        let baseline = ModelOverrideSelection::Inherit {
            thinking: Some("high".into()),
        };
        port.observe_model_change(baseline.clone(), ModelChangeDisposition::Applied)
            .await
            .expect("baseline");
        let (send, receive) = tokio::sync::oneshot::channel();
        let error = port
            .execute(
                input(),
                Box::pin(async move {
                    send.send(current_provider_admission().expect("bound admission"))
                        .map_err(|_| anyhow::Error::msg("fixture receiver closed"))?;
                    Err(zeroclaw_api::turn_stop::ToolLoopCancelled.into())
                }),
            )
            .await
            .expect_err("cancelled turn");
        assert!(
            zeroclaw_api::turn_stop::is_tool_loop_cancelled(&error),
            "{backend}: {error:#}"
        );
        let admission = receive.await.expect("retained admission");
        let head = port.owner.head(&port.stream).unwrap();
        for disposition in [
            ModelChangeDisposition::Applied,
            ModelChangeDisposition::AppliedForTurn,
        ] {
            assert!(
                admission
                    .observe_model_change(
                        ModelOverrideSelection::Set {
                            provider_ref: "late.provider".into(),
                            model: "late-model".into(),
                            thinking: None,
                            route_mode: ModelRouteMode::Manual,
                        },
                        disposition
                    )
                    .await
                    .is_err()
            );
            assert_eq!(port.owner.head(&port.stream).unwrap(), head);
        }
        assert_eq!(port.restore_model_override().await.unwrap(), Some(baseline));
        assert!(
            port.owner
                .stream_history(&port.stream)
                .unwrap()
                .iter()
                .any(|event| event.body.kind() == EventKind::TurnCancelled)
        );
    }
}

#[tokio::test]
async fn idle_commands_project_canonical_messages_and_recheck_revocation() {
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::TempDir::new().expect("directory");
        let (session, authority) = session(dir.path(), backend);
        session.rename("title").await.expect("rename");
        let id = session
            .append_assistant("operator content")
            .await
            .expect("append");
        let messages = session
            .query()
            .messages(session.stream.clone())
            .await
            .expect("messages");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message_id, id);
        assert_eq!(messages[0].role, "assistant");
        assert_eq!(
            session
                .restore_context()
                .await
                .expect("restore")
                .expect("snapshot")
                .chat_projection()[0]
                .content,
            "operator content"
        );
        for (selection, outcome) in [
            (
                ModelOverrideSelection::Set {
                    provider_ref: "fixture".into(),
                    model: "model-a".into(),
                    thinking: None,
                    route_mode: ModelRouteMode::Manual,
                },
                ModelChangeDisposition::Applied,
            ),
            (
                ModelOverrideSelection::Set {
                    provider_ref: "absent".into(),
                    model: "model-b".into(),
                    thinking: None,
                    route_mode: ModelRouteMode::Manual,
                },
                ModelChangeDisposition::Rejected {
                    reason_key: "provider_missing".into(),
                    diagnostic: None,
                },
            ),
            (
                ModelOverrideSelection::Clear,
                ModelChangeDisposition::Applied,
            ),
        ] {
            session
                .observe_model_change(selection, outcome)
                .await
                .expect("model control");
        }
        assert_eq!(
            session
                .restore_model_override()
                .await
                .expect("restore selection"),
            Some(ModelOverrideSelection::Clear)
        );
        session
            .request_cancellation(None, "fixture-incarnation", "fixture_cancel_intent")
            .await
            .expect("session cancellation intent");
        let summary = session
            .query()
            .summary(session.stream.clone())
            .await
            .expect("canonical list metadata");
        assert_eq!(summary.message_count, 1);
        assert_eq!(summary.title(), Some("title"));
        assert!(!summary.running);
        let head = session.owner.store.head(&session.stream).expect("head");
        authority.0.store(false, Ordering::SeqCst);
        assert!(session.rename("denied").await.is_err());
        assert!(session.append_assistant("denied").await.is_err());
        assert_eq!(
            session.owner.store.head(&session.stream).expect("head"),
            head
        );
    }
}

#[tokio::test]
async fn late_child_terminal_reconciles_unknown_without_second_outcome_or_replay() {
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::TempDir::new().expect("directory");
        let (session, _) = session(dir.path(), backend);
        let (send, receive) = tokio::sync::oneshot::channel();
        assert!(
            session
                .execute(
                    input(),
                    Box::pin(async move {
                        let child = current_provider_admission()
                            .expect("parent capture")
                            .begin_child(ChildDispatchIntent {
                                relation: "delegate".into(),
                                target_agent: "child".into(),
                                prompt: "child seed".into(),
                                execution_mode: ChildExecutionMode::Sync,
                                task_id: None,
                            })
                            .await?;
                        send.send(child)
                            .map_err(|_| anyhow::Error::msg("receiver lost"))?;
                        anyhow::bail!("parent interrupted before child outcome")
                    })
                )
                .await
                .is_err()
        );
        let child = receive.await.expect("child");
        assert_eq!(
            child
                .execute(Box::pin(async { Ok("child result".into()) }))
                .await
                .expect("late child"),
            "child result"
        );
        assert_eq!(
            child
                .execute(Box::pin(async { panic!("replay must not execute") }))
                .await
                .expect("retry"),
            "child result"
        );
        let history = session
            .query()
            .project_history(session.stream.clone())
            .await
            .expect("history");
        assert!(
            history
                .invocations
                .values()
                .all(|invocation| invocation.status
                    == zeroclaw_api::session_events::projection::InvocationStatus::Completed)
        );
        let page = session
            .query()
            .read_page(
                session.stream.clone(),
                EventCursor::beginning(session.stream.clone()),
                None,
                100,
                4 * 1024 * 1024,
            )
            .await
            .expect("page");
        assert_eq!(
            page.events
                .iter()
                .filter(|event| event.body.kind() == EventKind::OutcomeReconciled)
                .count(),
            1
        );
        assert_eq!(
            page.events
                .iter()
                .filter(|event| event.body.kind() == EventKind::InvocationOutcomeUnknown)
                .count(),
            1
        );
        assert_eq!(
            page.events
                .iter()
                .filter(|event| event.body.kind() == EventKind::ChildInvocationCompleted)
                .count(),
            0
        );
        assert!(
            !page
                .events
                .iter()
                .any(|event| event.body.kind() == EventKind::ChildResultConsumed),
            "a stopped parent cannot consume a late child result"
        );
    }
}

#[tokio::test]
async fn joined_child_interruption_retains_started_provider_as_unknown() {
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::TempDir::new().expect("directory");
        let (session, _) = session(dir.path(), backend);
        let (send, receive) = tokio::sync::oneshot::channel();
        session
            .execute(
                input(),
                Box::pin(async move {
                    let child = current_provider_admission()
                        .expect("parent")
                        .begin_child(ChildDispatchIntent {
                            relation: "delegate".into(),
                            target_agent: "child".into(),
                            prompt: "child seed".into(),
                            execution_mode: ChildExecutionMode::Background,
                            task_id: Some("fixture-task".into()),
                        })
                        .await?;
                    send.send(child)
                        .map_err(|_| anyhow::Error::msg("receiver lost"))?;
                    Ok("scheduled".into())
                }),
            )
            .await
            .expect("parent ends independently");
        let child = receive.await.expect("child");
        let run = child.clone();
        let entered = Arc::new(tokio::sync::Notify::new());
        let signal = entered.clone();
        let worker = zeroclaw_spawn::spawn!(async move {
            run.execute(Box::pin(async move {
                let _observer = current_provider_admission()
                    .expect("child")
                    .admit(ProviderDispatchIntent {
                        requested_route: None,
                        previous_attempt_event_id: None,
                        tool_round_policy: zeroclaw_api::model_provider::ToolRoundPolicy::Batch,
                        thinking: None,
                        provider_ref: "fixture".into(),
                        model: "fixture".into(),
                        messages: vec![ChatMessage::user("seed")],
                        tools: None,
                        temperature: None,
                    })
                    .await?;
                signal.notify_one();
                std::future::pending::<anyhow::Result<String>>().await
            }))
            .await
        });
        entered.notified().await;
        worker.abort();
        assert!(worker.await.expect_err("joined abort").is_cancelled());
        child
            .interrupted("fixture_joined_abort")
            .await
            .expect("interruption");
        child
            .interrupted("fixture_joined_abort")
            .await
            .expect("interruption retry");
        let projection = session
            .query()
            .project_history(session.stream.clone())
            .await
            .expect("projection");
        assert!(
            projection
                .invocations
                .values()
                .all(|invocation| invocation.status
                    == zeroclaw_api::session_events::projection::InvocationStatus::Unknown)
        );
        assert!(
            projection.turns.values().all(|turn| turn.status
                == zeroclaw_api::session_events::projection::TurnStatus::Committed)
        );
    }
}

#[tokio::test]
async fn system_actor_is_explicitly_admitted_and_never_becomes_a_principal() {
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::TempDir::new().expect("directory");
        let (mut session, authority) = session(dir.path(), backend);
        session.actor = EventActor::System {
            component: "fixture_system".into(),
        };
        session
            .execute(
                input(),
                Box::pin(async {
                    let child = current_provider_admission()
                        .expect("capture")
                        .begin_child(ChildDispatchIntent {
                            relation: "delegate".into(),
                            target_agent: "child".into(),
                            prompt: "system child".into(),
                            execution_mode: ChildExecutionMode::Sync,
                            task_id: None,
                        })
                        .await?;
                    child
                        .execute(Box::pin(async { Ok("system answer".into()) }))
                        .await
                }),
            )
            .await
            .expect("trusted system admission");
        let page = session
            .query()
            .read_page(
                session.stream.clone(),
                EventCursor::beginning(session.stream.clone()),
                None,
                256,
                4 * 1024 * 1024,
            )
            .await
            .expect("history");
        assert!(
            page.events
                .iter()
                .filter(|event| event.body.kind() == EventKind::TurnAccepted)
                .all(|event| event.actor == session.actor)
        );
        authority.0.store(false, Ordering::SeqCst);
        assert!(
            session
                .execute(
                    input(),
                    Box::pin(async { panic!("revoked system cannot execute") })
                )
                .await
                .is_err()
        );
        session.actor = EventActor::System {
            component: "foreign_component".into(),
        };
        assert!(session.restore_context().await.is_err());
    }
}

#[tokio::test]
async fn context_only_input_preserves_identity_without_engine_or_model_output() {
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::TempDir::new().expect("directory");
        let (session, authority) = session(dir.path(), backend);
        let input = input();
        let id = session
            .record_context_input(
                input.clone(),
                "dated observed message",
                "passive_channel_context",
            )
            .await
            .expect("record");
        let head = session.owner.store.head(&session.stream).expect("head");
        assert_eq!(
            session
                .record_context_input(
                    input.clone(),
                    "dated observed message",
                    "passive_channel_context"
                )
                .await
                .expect("retry"),
            id
        );
        assert_eq!(
            session.owner.store.head(&session.stream).expect("head"),
            head
        );
        assert!(
            session
                .record_context_input(
                    input.clone(),
                    "different enrichment",
                    "passive_channel_context"
                )
                .await
                .is_err()
        );
        assert!(
            session
                .record_context_input(input.clone(), "dated observed message", "different_reason")
                .await
                .is_err()
        );
        assert!(
            session
                .execute(
                    input.clone(),
                    Box::pin(async { panic!("context input cannot dispatch") })
                )
                .await
                .is_err()
        );
        assert_eq!(
            session.owner.store.head(&session.stream).expect("head"),
            head
        );
        let messages = session
            .query()
            .messages(session.stream.clone())
            .await
            .expect("messages");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message_id, id);
        assert_eq!(messages[0].role, "user");
        let snapshot = session
            .restore_context()
            .await
            .expect("restore")
            .expect("snapshot");
        assert_eq!(snapshot.messages.len(), 1);
        assert_eq!(
            snapshot.chat_projection()[0].content,
            "dated observed message"
        );
        let page = session
            .query()
            .read_page(
                session.stream.clone(),
                EventCursor::beginning(session.stream.clone()),
                None,
                256,
                4 * 1024 * 1024,
            )
            .await
            .expect("events");
        assert!(!page.events.iter().any(|event| matches!(
            event.body.kind(),
            EventKind::TurnStarted
                | EventKind::ProviderAttemptStarted
                | EventKind::AssistantContentRecorded
        )));
        assert!(page.events.iter().any(|event|matches!(&event.body.data,EventData::IngressDecisionRecorded {decision,..} if decision=="context_only")));
        assert!(
            !session
                .query()
                .summary(session.stream.clone())
                .await
                .expect("summary")
                .running
        );
        authority.0.store(false, Ordering::SeqCst);
        assert!(
            session
                .record_context_input(input, "dated observed message", "passive_channel_context")
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn close_preserves_history_and_blocks_execution_without_polling_engine() {
    for backend in ["sqlite", "jsonl"] {
        let directory = tempfile::TempDir::new().expect("directory");
        let (session, authority) = session(directory.path(), backend);
        session
            .append_assistant("retained historical fact")
            .await
            .expect("message");
        session.close("operator_closed").await.expect("close");
        let head = session.owner.store.head(&session.stream).expect("head");
        let polled = AtomicBool::new(false);
        let result = session
            .execute(
                input(),
                Box::pin(async {
                    polled.store(true, Ordering::SeqCst);
                    Ok("must not execute".into())
                }),
            )
            .await;
        assert!(result.is_err());
        assert!(!polled.load(Ordering::SeqCst));
        assert_eq!(
            session.owner.store.head(&session.stream).expect("head"),
            head
        );
        let messages = session
            .query()
            .messages(session.stream.clone())
            .await
            .expect("history");
        assert_eq!(messages[0].text, "retained historical fact");
        authority.0.store(false, Ordering::SeqCst);
        assert!(
            session
                .query()
                .messages(session.stream.clone())
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn child_session_close_cannot_be_reported_as_execution_completion() {
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::TempDir::new().expect("directory");
        let (session, _) = session(dir.path(), backend);
        session
            .execute(
                input(),
                Box::pin(async {
                    current_provider_admission()
                        .expect("capture")
                        .begin_child(ChildDispatchIntent {
                            relation: "delegate".into(),
                            target_agent: "child".into(),
                            prompt: "seed".into(),
                            execution_mode: ChildExecutionMode::Background,
                            task_id: None,
                        })
                        .await?;
                    Ok("scheduled".into())
                }),
            )
            .await
            .expect("parent");
        let page = session
            .owner
            .store
            .read_page(
                &session.stream,
                &EventCursor::beginning(session.stream.clone()),
                None,
                100,
                4 * 1024 * 1024,
            )
            .expect("parent history");
        let request = page
            .events
            .iter()
            .find(|event| event.body.kind() == EventKind::ChildInvocationRequested)
            .expect("request");
        let child = SessionStreamId::parse(format!(
            "child-{}",
            request.invocation_id.as_ref().expect("invocation")
        ))
        .expect("child stream");
        let mut close = SessionEvent::typed(
            child.clone(),
            EventData::SessionClosed {
                reason_key: "fixture_closed".into(),
            },
            EventActor::default(),
        )
        .expect("close");
        let child_page = session
            .owner
            .store
            .read_page(
                &child,
                &EventCursor::beginning(child.clone()),
                None,
                100,
                4 * 1024 * 1024,
            )
            .expect("child history");
        close.branch_id = child_page.events[0].branch_id.clone();
        let closed = session
            .owner
            .store
            .append_batch(AppendRequest {
                stream_id: child.clone(),
                expected_head: session.owner.store.head(&child).expect("child head"),
                idempotency_key: "fixture-child-close".into(),
                events: vec![close],
                payloads: vec![],
                erasure_reference_commitment: None,
            })
            .expect("close is valid lifecycle evidence");
        let mut completed = SessionEvent::typed(
            session.stream.clone(),
            EventData::ChildInvocationCompleted {
                child_terminal_cursor: closed.last,
                consumed_result_ref: None,
            },
            request.actor.clone(),
        )
        .expect("completion");
        completed.branch_id = request.branch_id.clone();
        completed.turn_id = request.turn_id.clone();
        completed.invocation_id = request.invocation_id.clone();
        completed.causation_id = Some(request.event_id.clone());
        let head = session.owner.store.head(&session.stream).expect("head");
        assert!(
            session
                .owner
                .store
                .append_batch(AppendRequest {
                    stream_id: session.stream.clone(),
                    expected_head: head.clone(),
                    idempotency_key: "fixture-false-child-completion".into(),
                    events: vec![completed],
                    payloads: vec![],
                    erasure_reference_commitment: None,
                })
                .is_err(),
            "session closure provides no execution outcome"
        );
        assert_eq!(
            session.owner.store.head(&session.stream).expect("head"),
            head
        );
        session
            .query()
            .project_history(session.stream.clone())
            .await
            .expect("readable parent history");
    }
}

#[tokio::test]
async fn child_completion_and_actual_consumption_are_separate_and_retry_safe() {
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::TempDir::new().expect("directory");
        let (session, _) = session(dir.path(), backend);
        session.execute(input(),Box::pin(async {
            let capture=current_provider_admission().expect("scope");
            let child=capture.begin_child(ChildDispatchIntent {relation:"delegate".into(),target_agent:"child".into(),prompt:"seed".into(),execution_mode:ChildExecutionMode::Background,task_id:Some("fixture-task".into())}).await?;
            child.execute(Box::pin(async {Ok("answer".into())})).await?;
            let result=serde_json::json!({"task_id":"fixture-task","status":"completed","output":"answer"});
            capture.consume_child_result("fixture-task",&result).await?;
            capture.consume_child_result("fixture-task",&result).await?;
            assert!(capture.consume_child_result("fixture-task",&serde_json::json!({"output":"changed"})).await.is_err());
            Ok("parent answer".into())
        })).await.expect("turn");
        let page = session
            .query()
            .read_page(
                session.stream.clone(),
                EventCursor::beginning(session.stream.clone()),
                None,
                100,
                4 * 1024 * 1024,
            )
            .await
            .expect("page");
        let completed = page
            .events
            .iter()
            .find(|event| event.body.kind() == EventKind::ChildInvocationCompleted)
            .expect("completion");
        let consumed: Vec<_> = page
            .events
            .iter()
            .filter(|event| event.body.kind() == EventKind::ChildResultConsumed)
            .collect();
        assert_eq!(consumed.len(), 1);
        assert!(completed.sequence < consumed[0].sequence);
        assert!(
            matches!(&consumed[0].body.data, EventData::ChildResultConsumed {child_outcome_event_id,..} if *child_outcome_event_id==completed.event_id)
        );
    }
}

#[tokio::test]
async fn delivery_admission_is_durable_and_uncertain_network_errors_remain_unknown() {
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::tempdir().expect("directory");
        let (session, authority) = session(dir.path(), backend);
        let intent = zeroclaw_api::session_events::capture::DeliveryIntent {
            turn_id: None,
            transport: "fixture".into(),
            target: "local-viewer".into(),
            content: "answer".into(),
            completion_boundary: "adapter_acceptance".into(),
        };
        session
            .deliver(
                intent.clone(),
                Box::pin(async {
                    let page = session
                        .query()
                        .read_page(
                            session.stream.clone(),
                            EventCursor::beginning(session.stream.clone()),
                            None,
                            100,
                            4 * 1024 * 1024,
                        )
                        .await?;
                    assert_eq!(
                        page.events.last().expect("started").body.kind(),
                        EventKind::DeliveryStarted
                    );
                    Ok(serde_json::json!({"boundary":"adapter_acceptance"}))
                }),
            )
            .await
            .expect("delivery");
        assert!(
            session
                .deliver(
                    intent.clone(),
                    Box::pin(async { anyhow::bail!("connection_reset_after_send") })
                )
                .await
                .is_err()
        );
        assert!(
            session
                .deliver(
                    intent.clone(),
                    Box::pin(async {
                        Err(
                            zeroclaw_api::session_events::capture::delivery_definitely_failed(
                                anyhow::Error::msg("rejected before adapter write"),
                            ),
                        )
                    }),
                )
                .await
                .is_err()
        );
        let history = session
            .query()
            .project_history(session.stream.clone())
            .await
            .expect("history");
        assert_eq!(
            history
                .invocations
                .values()
                .filter(|invocation| invocation.status
                    == zeroclaw_api::session_events::projection::InvocationStatus::Unknown)
                .count(),
            1
        );
        assert_eq!(
            history
                .invocations
                .values()
                .filter(|invocation| invocation.status
                    == zeroclaw_api::session_events::projection::InvocationStatus::Failed)
                .count(),
            1
        );
        authority.0.store(false, Ordering::SeqCst);
        assert!(
            session
                .deliver(
                    intent,
                    Box::pin(async { panic!("revoked send must not be polled") })
                )
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn rejected_approval_reply_is_audit_only_and_survives_both_backends() {
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::tempdir().expect("directory");
        let (session, _) = session(dir.path(), backend);
        session
            .record_approval_reply_rejected(
                Some("foreign-or-expired-request".into()),
                "unknown_or_expired_request".into(),
                serde_json::json!({"transport":"websocket","decision":"approve"}),
            )
            .await
            .expect("append rejected approval audit");
        let page = session
            .query()
            .read_page(
                session.stream.clone(),
                EventCursor::beginning(session.stream.clone()),
                None,
                100,
                4 * 1024 * 1024,
            )
            .await
            .expect("page");
        let rejected = page
            .events
            .iter()
            .find(|event| event.body.kind() == EventKind::ApprovalReplyRejected)
            .expect("rejected approval audit");
        assert!(matches!(
            &rejected.body.data,
            EventData::ApprovalReplyRejected {
                request_id: Some(request_id),
                classification,
                ..
            } if request_id == "foreign-or-expired-request"
                && classification == "unknown_or_expired_request"
        ));
        let projection = session
            .query()
            .project_history(session.stream.clone())
            .await
            .expect("history");
        assert!(projection.invocations.is_empty());
    }
}

#[tokio::test]
async fn active_engine_context_replacement_uses_its_owned_lane_without_deadlock() {
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::TempDir::new().expect("directory");
        let (session, _) = session(dir.path(), backend);
        let command = input();
        let turn = command.turn_id.clone();
        let expected = ContextSnapshot {
            messages: (vec![zeroclaw_api::model_provider::ChatMessage::user(
                "restored historical input",
            )])
            .into_iter()
            .map(zeroclaw_api::model_provider::ConversationMessage::Chat)
            .collect(),
            history_has_trim_breadcrumb: Some(false),
        };
        let replacement = expected.clone();
        let bound = session.clone();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            session.execute(
                command,
                Box::pin(async move {
                    assert_eq!(current_engine_input().expect("input scope").turn_id, turn);
                    bound
                        .replace_context(replacement, "in_turn_restore")
                        .await?;
                    Ok("completed".into())
                }),
            ),
        )
        .await
        .expect("nested maintenance must not wait for its own foreground lane")
        .expect("turn");
        assert_eq!(result, "completed");
        let restored = session
            .restore_context()
            .await
            .expect("restore")
            .expect("projection");
        assert_eq!(
            canonical_json(&restored).expect("codec"),
            canonical_json(&expected).expect("codec")
        );
        let page = session
            .query()
            .read_page(
                session.stream.clone(),
                EventCursor::beginning(session.stream.clone()),
                None,
                256,
                4 * 1024 * 1024,
            )
            .await
            .expect("events");
        assert_eq!(
            page.events
                .iter()
                .filter(|event| event.body.kind() == EventKind::ContextTransformStarted)
                .count(),
            1
        );
        assert!(
            page.events
                .iter()
                .filter(|event| event.body.kind() == EventKind::ContextTransformStarted)
                .all(|event| event.turn_id.is_some())
        );
    }
}

struct ContextWriteFailureStore {
    inner: Arc<dyn SessionEventStore>,
    mode: &'static str,
}
impl SessionEventStore for ContextWriteFailureStore {
    fn append_batch(&self, request: AppendRequest) -> Result<AppendReceipt, AppendError> {
        if request.events.iter().any(|event| {
            matches!(
                event.body.data,
                EventData::ContextProjectionCreated {
                    transform_started_event_id: Some(_),
                    ..
                }
            )
        }) {
            if self.mode == "uncertain_after" {
                self.inner.append_batch(request)?;
            }
            return Err(if self.mode.starts_with("uncertain_") {
                AppendError::CommitOutcomeUnknown("fixture context commit uncertain".into())
            } else {
                AppendError::StorageUnavailable("fixture context write rejected".into())
            });
        }
        if self.mode == "settlement_failed"
            && request.events.iter().any(|event| {
                matches!(
                    event.body.kind(),
                    EventKind::ContextTransformFailed | EventKind::ContextTransformCancelled
                )
            })
        {
            return Err(AppendError::StorageUnavailable(
                "fixture context settlement unavailable".into(),
            ));
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

#[tokio::test]
async fn context_installation_errors_settle_once_without_inventing_uncertain_outcomes() {
    for backend in ["sqlite", "jsonl"] {
        for active in [false, true] {
            for mode in [
                "rejected",
                "uncertain_before",
                "uncertain_after",
                "settlement_failed",
            ] {
                let directory = tempfile::TempDir::new().expect("directory");
                let (mut port, _) = session(directory.path(), backend);
                port.owner = Arc::new(SessionCoordinator::new(Arc::new(
                    ContextWriteFailureStore {
                        inner: port.owner.store.clone(),
                        mode,
                    },
                )));
                let bound = port.clone();
                let replace = async move {
                    bound
                        .replace_context(
                            ContextSnapshot {
                                messages: vec![
                                    zeroclaw_api::model_provider::ConversationMessage::Chat(
                                        ChatMessage::user("replacement input"),
                                    ),
                                ],
                                history_has_trim_breadcrumb: Some(false),
                            },
                            "fixture_installation",
                        )
                        .await?;
                    Ok("completed".to_string())
                };
                let error = if active {
                    port.execute(input(), Box::pin(replace)).await
                } else {
                    replace.await
                }
                .expect_err("injected installation error");
                let diagnostic = format!("{error:#}");
                if mode.starts_with("uncertain_") {
                    assert!(
                        diagnostic.contains("fixture context commit uncertain"),
                        "{diagnostic}"
                    );
                } else {
                    assert!(
                        diagnostic.contains("fixture context write rejected"),
                        "{diagnostic}"
                    );
                }
                if mode == "settlement_failed" {
                    assert!(
                        diagnostic.contains("fixture context settlement unavailable"),
                        "{diagnostic}"
                    );
                }
                let history = port.owner.stream_history(&port.stream).expect("history");
                let starts: Vec<_> = history
                    .iter()
                    .filter(|event| event.body.kind() == EventKind::ContextTransformStarted)
                    .collect();
                assert_eq!(starts.len(), 1, "{backend}/{active}/{mode}");
                let started = &starts[0].event_id;
                let outcomes: Vec<_> = history
                    .iter()
                    .filter(|event| match &event.body.data {
                        EventData::ContextProjectionCreated {
                            transform_started_event_id,
                            ..
                        } => transform_started_event_id.as_ref() == Some(started),
                        EventData::ContextTransformFailed {
                            started_event_id, ..
                        }
                        | EventData::ContextTransformCancelled {
                            started_event_id, ..
                        } => started_event_id == started,
                        _ => false,
                    })
                    .collect();
                assert_eq!(
                    outcomes.len(),
                    usize::from(matches!(mode, "rejected" | "uncertain_after")),
                    "{backend}/{active}/{mode}"
                );
                if mode == "rejected" {
                    let EventData::ContextTransformFailed { error_ref, .. } =
                        &outcomes[0].body.data
                    else {
                        panic!("known installation failure must retain its diagnostic");
                    };
                    let PayloadRead::Present(payload) = port
                        .owner
                        .store
                        .read_payload(&error_ref.payload_id)
                        .expect("diagnostic")
                    else {
                        panic!("failure diagnostic must be durable");
                    };
                    assert!(
                        String::from_utf8(payload.body)
                            .expect("utf8")
                            .contains("fixture context write rejected")
                    );
                }
                if mode == "uncertain_after" {
                    assert_eq!(outcomes[0].body.kind(), EventKind::ContextProjectionCreated);
                }
            }
        }
    }
}

#[tokio::test]
async fn summary_uses_the_projection_committed_by_the_previous_lane_owner() {
    for backend in ["sqlite", "jsonl"] {
        let directory = tempfile::TempDir::new().expect("directory");
        let (port, _) = session(directory.path(), backend);
        let lane = port.owner.foreground_lock(&port.stream);
        let permit = lane.lock().await;
        // Queue an update first, then a summary behind it. Tokio's FIFO lane
        // must make the summary consume the update's committed projection.
        let update = port.append_assistant("previous lane owner committed this");
        tokio::pin!(update);
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(
                std::future::Future::poll(update.as_mut(), cx).is_pending()
            ))
            .await
        );
        let summary = port.summarize_context(
            ProviderPurpose::Summary,
            Box::new(|source| {
                Box::pin(async move {
                    assert_eq!(
                        match &source[source.len() - 2] {
                            ConversationMessage::Chat(chat) => &chat.content,
                            _ => panic!("chat input"),
                        },
                        "previous lane owner committed this"
                    );
                    anyhow::bail!("fixture_summary_stop")
                })
            }),
        );
        tokio::pin!(summary);
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(
                std::future::Future::poll(summary.as_mut(), cx).is_pending()
            ))
            .await
        );
        drop(permit);
        let (updated, summarized) = tokio::join!(update, summary);
        updated.expect("update committed");
        assert!(
            summarized
                .expect_err("fixture stop")
                .to_string()
                .contains("fixture_summary_stop")
        );
        assert_eq!(
            port.restore_context()
                .await
                .expect("restore")
                .expect("context")
                .chat_projection()
                .last()
                .expect("retained input")
                .content,
            "previous lane owner committed this"
        );
    }
}

#[tokio::test]
async fn empty_summary_settles_without_replacing_context() {
    for backend in ["sqlite", "jsonl"] {
        for text in [
            "",
            " \n ",
            "<think>internal reasoning</think>",
            "<think>unfinished",
        ] {
            let directory = tempfile::TempDir::new().expect("directory");
            let (port, _) = session(directory.path(), backend);
            port.append_assistant("retained original")
                .await
                .expect("seed");
            let before = port
                .restore_context()
                .await
                .expect("restore")
                .expect("context");
            let error = port
                .summarize_context(
                    ProviderPurpose::Summary,
                    Box::new(move |_| Box::pin(async move { Ok(text.to_owned()) })),
                )
                .await
                .expect_err("empty summary rejected");
            assert!(error.to_string().contains("empty_context_summary"));
            let after = port
                .restore_context()
                .await
                .expect("restore")
                .expect("context");
            assert_eq!(
                serde_json::to_value(before).expect("before"),
                serde_json::to_value(after).expect("after")
            );
            let history = port.owner.stream_history(&port.stream).expect("history");
            assert_eq!(
                history.last().expect("outcome").body.kind(),
                EventKind::ContextTransformFailed
            );
            assert_eq!(
                history
                    .iter()
                    .filter(|event| event.body.kind() == EventKind::ContextTransformFailed)
                    .count(),
                1
            );
        }
    }
}

#[tokio::test]
async fn summary_executor_panics_settle_and_revoked_executors_never_run() {
    for backend in ["sqlite", "jsonl"] {
        let directory = tempfile::TempDir::new().expect("directory");
        let (port, authority) = session(directory.path(), backend);
        let error = port
            .summarize_context(
                ProviderPurpose::Summary,
                Box::new(|_| panic!("fixture executor construction")),
            )
            .await
            .expect_err("panic settled");
        assert!(error.to_string().contains("summary_executor_panic"));
        assert_eq!(
            port.owner
                .stream_history(&port.stream)
                .expect("history")
                .last()
                .expect("outcome")
                .body
                .kind(),
            EventKind::ContextTransformFailed
        );
        let through = port.owner.store.head(&port.stream).expect("head");
        authority.0.store(false, Ordering::SeqCst);
        assert!(
            port.summarize_context(
                ProviderPurpose::Summary,
                Box::new(|_| panic!("revoked executor must not run"))
            )
            .await
            .expect_err("revoked")
            .to_string()
            .contains("revoked")
        );
        assert_eq!(port.owner.store.head(&port.stream).expect("head"), through);
    }
}

#[tokio::test]
async fn summary_installation_errors_settle_once_without_inventing_uncertain_outcomes() {
    for backend in ["sqlite", "jsonl"] {
        for active in [false, true] {
            for mode in [
                "rejected",
                "uncertain_before",
                "uncertain_after",
                "settlement_failed",
            ] {
                let directory = tempfile::TempDir::new().expect("directory");
                let (mut port, _) = session(directory.path(), backend);
                port.owner = Arc::new(SessionCoordinator::new(Arc::new(
                    ContextWriteFailureStore {
                        inner: port.owner.store.clone(),
                        mode,
                    },
                )));
                let bound = port.clone();
                let summarize = async move {
                    bound
                        .summarize_context(
                            ProviderPurpose::Summary,
                            Box::new(move |_source| {
                                Box::pin(async move {
                                    let text = Box::pin(async {
                                        let observer = current_provider_admission()
                                    .expect("summary capture")
                                    .admit(ProviderDispatchIntent {
                                        requested_route: None,
                                        previous_attempt_event_id: None,
                                        tool_round_policy:
                                            zeroclaw_api::model_provider::ToolRoundPolicy::Batch,
                                        thinking: None,
                                        provider_ref: "fixture".into(),
                                        model: "fixture".into(),
                                        messages: vec![ChatMessage::user("summarize")],
                                        tools: None,
                                        temperature: None,
                                    })
                                    .await?;
                                        let text = PayloadRecord::new(
                                            PayloadId::new(),
                                            "session.text/v1".into(),
                                            b"captured summary".to_vec(),
                                        )?;
                                        let response = PayloadRecord::json(
                                            "session.response/v1",
                                            &ProviderResponsePayload {
                                                blocks: vec![ResponseBlock::Text {
                                                    text_ref: StoredPayloadRef::from(&text),
                                                }],
                                                completion: ContentCompletion::Complete,
                                                finish_evidence_ref: None,
                                                usage_ref: None,
                                                reasoning_absence: Some("not_returned".into()),
                                            },
                                        )?;
                                        observer
                                            .observe(
                                                EventData::ProviderAttemptCompleted {
                                                    response_ref: StoredPayloadRef::from(&response),
                                                    served_attribution_ref: None,
                                                    last_progress_event_id: None,
                                                },
                                                vec![text, response],
                                            )
                                            .await?;
                                        Ok::<String, anyhow::Error>("captured summary".into())
                                    })
                                    .await?;
                                    Ok(text)
                                })
                            }),
                        )
                        .await
                        .map(|_| String::new())
                };
                let error = if active {
                    port.execute(input(), Box::pin(summarize)).await
                } else {
                    summarize.await
                }
                .expect_err("injected summary installation error");
                let diagnostic = format!("{error:#}");
                let original = if mode.starts_with("uncertain_") {
                    "fixture context commit uncertain"
                } else {
                    "fixture context write rejected"
                };
                assert!(
                    diagnostic.contains(original),
                    "{backend}/{active}/{mode}: {diagnostic}"
                );
                assert!(
                    !diagnostic.contains("idempotency"),
                    "the installation error must not be replaced by a second outcome: {diagnostic}"
                );
                if mode == "settlement_failed" {
                    assert!(diagnostic.contains("fixture context settlement unavailable"));
                }
                let history = port.owner.stream_history(&port.stream).expect("history");
                let starts: Vec<_> = history
                    .iter()
                    .filter(|event| event.body.kind() == EventKind::ContextTransformStarted)
                    .collect();
                assert_eq!(starts.len(), 1);
                let started = &starts[0].event_id;
                let outcomes: Vec<_> = history
                    .iter()
                    .filter(|event| match &event.body.data {
                        EventData::ContextProjectionCreated {
                            transform_started_event_id,
                            ..
                        } => transform_started_event_id.as_ref() == Some(started),
                        EventData::ContextTransformFailed {
                            started_event_id, ..
                        }
                        | EventData::ContextTransformCancelled {
                            started_event_id, ..
                        } => started_event_id == started,
                        _ => false,
                    })
                    .collect();
                assert_eq!(
                    outcomes.len(),
                    usize::from(matches!(mode, "rejected" | "uncertain_after")),
                    "{backend}/{active}/{mode}"
                );
                assert_eq!(
                    history
                        .iter()
                        .filter(|event| event.body.kind() == EventKind::ProviderAttemptCompleted)
                        .count(),
                    1
                );
                assert!(
                    !history
                        .iter()
                        .any(|event| event.body.kind() == EventKind::InvocationOutcomeUnknown)
                );
                if mode == "rejected" {
                    let EventData::ContextTransformFailed { error_ref, .. } =
                        &outcomes[0].body.data
                    else {
                        panic!("known summary installation failure must be recorded");
                    };
                    let PayloadRead::Present(payload) = port
                        .owner
                        .store
                        .read_payload(&error_ref.payload_id)
                        .expect("error payload")
                    else {
                        panic!("diagnostic must remain readable");
                    };
                    assert!(
                        String::from_utf8(payload.body)
                            .expect("utf8")
                            .contains(original)
                    );
                }
                if mode == "uncertain_after" {
                    let EventData::ContextProjectionCreated {
                        summary_ref,
                        summary_source_refs,
                        ..
                    } = &outcomes[0].body.data
                    else {
                        panic!("committed summary must survive an uncertain acknowledgement");
                    };
                    assert!(summary_ref.is_some());
                    assert_eq!(summary_source_refs.len(), 1);
                    let restored = port
                        .restore_context()
                        .await
                        .expect("restore")
                        .expect("context");
                    assert_eq!(
                        restored.chat_projection()[0].content,
                        "[Context summary]\ncaptured summary"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn summary_generation_settlement_failure_preserves_original_error_chain() {
    for backend in ["sqlite", "jsonl"] {
        for cancelled in [false, true] {
            let directory = tempfile::TempDir::new().expect("directory");
            let (mut port, _) = session(directory.path(), backend);
            port.owner = Arc::new(SessionCoordinator::new(Arc::new(
                ContextWriteFailureStore {
                    inner: port.owner.store.clone(),
                    mode: "settlement_failed",
                },
            )));
            let error = port
                .summarize_context(
                    ProviderPurpose::Summary,
                    Box::new(move |_source| {
                        Box::pin(async move {
                            Err(if cancelled {
                                anyhow::Error::new(zeroclaw_api::turn_stop::ToolLoopCancelled)
                            } else {
                                anyhow::Error::new(std::io::Error::other(
                                    "fixture summary generation failed",
                                ))
                            })
                        })
                    }),
                )
                .await
                .expect_err("generation and settlement failed");
            assert!(format!("{error:#}").contains("fixture context settlement unavailable"));
            assert_eq!(
                zeroclaw_api::turn_stop::is_tool_loop_cancelled(&error),
                cancelled
            );
            if !cancelled {
                assert!(error.chain().any(|cause| cause.is::<std::io::Error>()));
                assert!(format!("{error:#}").contains("fixture summary generation failed"));
            }
            let history = port.owner.stream_history(&port.stream).expect("history");
            assert_eq!(
                history
                    .iter()
                    .filter(|event| event.body.kind() == EventKind::ContextTransformStarted)
                    .count(),
                1
            );
            assert!(!history.iter().any(|event| matches!(
                event.body.kind(),
                EventKind::ContextTransformFailed | EventKind::ContextTransformCancelled
            )));
        }
    }
}

#[tokio::test]
async fn elicitation_admission_uses_committed_turn_and_stream_lifecycle() {
    for backend in ["sqlite", "jsonl"] {
        for state in ["accepted", "running", "cancelled", "requeued", "closed"] {
            for bound_to_turn in [true, false] {
                let directory = tempfile::TempDir::new().expect("directory");
                let (port, _) = session(directory.path(), backend);
                let command = port.input_command(input()).await.expect("command");
                let accepted = port
                    .owner
                    .admit_user_input(&command, &port.actor, &port.actor, port.authority.clone())
                    .await
                    .expect("admission")
                    .detach_for_recovery();
                if state != "accepted" {
                    let mut started = SessionEvent::typed(
                        port.stream.clone(),
                        EventData::TurnStarted {
                            accepted_event_id: accepted,
                            execution_binding_ref: command.execution_binding_ref.clone(),
                        },
                        port.actor.clone(),
                    )
                    .expect("started");
                    started.branch_id = Some(port.branch.clone());
                    started.turn_id = Some(command.turn_id.clone());
                    port.owner
                        .append(AppendRequest {
                            stream_id: port.stream.clone(),
                            expected_head: port.owner.store.head(&port.stream).expect("head"),
                            idempotency_key: format!("turn-start:{}", command.turn_id),
                            events: vec![started],
                            payloads: vec![],
                            erasure_reference_commitment: None,
                        })
                        .expect("start");
                }

                // Freeze the same control batch before its owner can finish.
                // Committing it later models a detached blocking append waiter.
                let (control, mut payloads) = port
                    .control_input(serde_json::json!({"command":"elicitation_request"}))
                    .expect("control");
                let prompt = PayloadRecord::json("session.text/v1", &"question").expect("prompt");
                let schema =
                    PayloadRecord::json("session.bytes/v1", &serde_json::json!({"type":"string"}))
                        .expect("schema");
                let mut requested = SessionEvent::typed(
                    port.stream.clone(),
                    EventData::ElicitationRequested {
                        request_id: "delayed-request".into(),
                        prompt_ref: StoredPayloadRef::from(&prompt),
                        response_schema_ref: StoredPayloadRef::from(&schema),
                        expires_at: chrono::Utc::now().timestamp_millis() + 60_000,
                        session_incarnation: port.owner.execution_incarnation.clone(),
                    },
                    port.actor.clone(),
                )
                .expect("request");
                requested.branch_id = Some(port.branch.clone());
                requested.turn_id = bound_to_turn.then(|| command.turn_id.clone());
                requested.causation_id = Some(control.event_id.clone());
                payloads.extend([prompt, schema]);

                match state {
                    "cancelled" => {
                        let mut terminal = SessionEvent::typed(
                            port.stream.clone(),
                            EventData::TurnCancelled {
                                reason_key: "fixture_owner_cancelled".into(),
                                unresolved_invocation_ids: vec![],
                            },
                            port.actor.clone(),
                        )
                        .expect("terminal");
                        terminal.branch_id = Some(port.branch.clone());
                        terminal.turn_id = Some(command.turn_id.clone());
                        port.owner
                            .append(AppendRequest {
                                stream_id: port.stream.clone(),
                                expected_head: port.owner.store.head(&port.stream).expect("head"),
                                idempotency_key: "fixture-cancelled".into(),
                                events: vec![terminal],
                                payloads: vec![],
                                erasure_reference_commitment: None,
                            })
                            .expect("cancelled");
                    }
                    "requeued" => {
                        port.owner
                            .recover_interrupted_stream(
                                port.stream.clone(),
                                "fixture-owner-lost".into(),
                                Arc::new(ElicitationOwnerLost),
                            )
                            .await
                            .expect("recovery");
                    }
                    "closed" => port.close("fixture_closed").await.expect("close"),
                    _ => {}
                }

                let head = port.owner.store.head(&port.stream).expect("head");
                let result = port
                    .commit_control(vec![control, requested], payloads)
                    .await;
                let expected = state != "closed" && (!bound_to_turn || state == "running");
                assert_eq!(
                    result.is_ok(),
                    expected,
                    "{backend}/{state}/{bound_to_turn}"
                );
                if !expected {
                    let diagnostic = result.expect_err("refusal").to_string();
                    assert!(
                        diagnostic.contains(if state == "closed" {
                            "closed stream rejects new execution"
                        } else {
                            "turn elicitation requires a running turn"
                        }),
                        "{diagnostic}"
                    );
                    assert_eq!(port.owner.store.head(&port.stream).expect("head"), head);
                }
                let history = port.owner.stream_history(&port.stream).expect("history");
                assert_eq!(
                    history
                        .iter()
                        .filter(|event| event.body.kind() == EventKind::ElicitationRequested)
                        .count(),
                    usize::from(expected)
                );
                zeroclaw_api::session_events::rules::HistoryIndex::from_history(&history)
                    .expect("history replay");
            }
        }
    }
}

#[tokio::test]
async fn turn_settlement_resolves_pending_elicitation_without_touching_other_requests() {
    for backend in ["sqlite", "jsonl"] {
        for outcome in ["success", "failed", "cancelled"] {
            let directory = tempfile::TempDir::new().expect("directory");
            let (port, _) = session(directory.path(), backend);
            port.request_elicitation(
                "independent-request",
                "independent question",
                &serde_json::json!({"type":"string"}),
                chrono::Utc::now().timestamp_millis() + 60_000,
            )
            .await
            .expect("independent request");
            let active = port.clone();
            let result = port
                .execute(
                    input(),
                    Box::pin(async move {
                        for request in ["pending-request", "answered-request"] {
                            active
                                .request_elicitation(
                                    request,
                                    "fixture question",
                                    &serde_json::json!({"type":"string"}),
                                    chrono::Utc::now().timestamp_millis() + 60_000,
                                )
                                .await?;
                        }
                        // An ingress response may resolve outside the turn task scope.
                        let responder = active.clone();
                        ::zeroclaw_spawn::spawn!(async move {
                            responder
                                .resolve_elicitation(
                                    "answered-request",
                                    &serde_json::json!({"state":"answered","answer":"yes"}),
                                )
                                .await
                        })
                        .await??;
                        match outcome {
                            "success" => Ok("finished".into()),
                            "failed" => Err(anyhow::Error::msg("fixture execution failed")),
                            _ => Err(zeroclaw_api::turn_stop::ToolLoopCancelled.into()),
                        }
                    }),
                )
                .await;
            assert_eq!(result.is_ok(), outcome == "success");
            let events = port.owner.stream_history(&port.stream).expect("history");
            let resolutions: Vec<_> = events
                .iter()
                .filter_map(|event| match &event.body.data {
                    EventData::ElicitationResolved {
                        request_id,
                        resolution_ref,
                    } => Some((event, request_id.as_str(), resolution_ref)),
                    _ => None,
                })
                .collect();
            assert_eq!(resolutions.len(), 2, "{backend}/{outcome}");
            assert_eq!(
                resolutions
                    .iter()
                    .filter(|(_, id, _)| *id == "answered-request")
                    .count(),
                1
            );
            let (resolved, _, reference) = resolutions
                .into_iter()
                .find(|(_, id, _)| *id == "pending-request")
                .expect("pending request settled");
            let PayloadRead::Present(payload) = port
                .owner
                .store
                .read_payload(&reference.payload_id)
                .expect("resolution")
            else {
                panic!("resolution missing");
            };
            let body: serde_json::Value =
                serde_json::from_slice(&payload.body).expect("resolution body");
            assert_eq!(
                body["state"],
                if outcome == "cancelled" {
                    "cancelled"
                } else {
                    "interrupted"
                }
            );
            let terminal = events
                .iter()
                .find(|event| {
                    matches!(
                        event.body.kind(),
                        EventKind::TurnCommitted | EventKind::TurnCancelled
                    )
                })
                .expect("turn terminal");
            assert!(resolved.sequence < terminal.sequence);
            zeroclaw_api::session_events::rules::HistoryIndex::from_history(&events)
                .expect("valid history");
            assert!(
                port.resolve_elicitation(
                    "pending-request",
                    &serde_json::json!({"state":"answered"})
                )
                .await
                .is_err()
            );
            port.resolve_elicitation(
                "independent-request",
                &serde_json::json!({"state":"answered","answer":"later"}),
            )
            .await
            .expect("independent request remains answerable");
        }
    }
}

#[tokio::test]
async fn elicitation_records_input_and_one_resolution_without_taking_foreground_lane() {
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::TempDir::new().expect("directory");
        let (port, authority) = session(dir.path(), backend);
        let active = port.clone();
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            port.execute(
                input(),
                Box::pin(async move {
                    active
                        .request_elicitation(
                            "fixture-elicitation",
                            "fixture question",
                            &serde_json::json!({"type":"string","enum":["one","two"]}),
                            chrono::Utc::now().timestamp_millis() + 60_000,
                        )
                        .await?;
                    active
                        .resolve_elicitation(
                            "fixture-elicitation",
                            &serde_json::json!({"state":"answered","answer":"two"}),
                        )
                        .await?;
                    Ok("answer accepted".into())
                }),
            ),
        )
        .await
        .expect("no foreground deadlock")
        .expect("turn");
        assert!(
            port.resolve_elicitation(
                "fixture-elicitation",
                &serde_json::json!({"state":"answered","answer":"one"})
            )
            .await
            .is_err()
        );
        assert!(
            port.resolve_elicitation("foreign-request", &serde_json::json!({"state":"answered"}))
                .await
                .is_err()
        );
        let page = port
            .query()
            .read_page(
                port.stream.clone(),
                EventCursor::beginning(port.stream.clone()),
                None,
                256,
                4 * 1024 * 1024,
            )
            .await
            .expect("history");
        assert_eq!(
            page.events
                .iter()
                .filter(|event| event.body.kind() == EventKind::ElicitationRequested)
                .count(),
            1
        );
        assert_eq!(
            page.events
                .iter()
                .filter(|event| event.body.kind() == EventKind::ElicitationResolved)
                .count(),
            1
        );
        zeroclaw_api::session_events::rules::HistoryIndex::from_history(&page.events)
            .expect("reconstructed elicitation lifecycle");
        authority.0.store(false, Ordering::SeqCst);
        assert!(
            port.request_elicitation(
                "revoked-request",
                "question",
                &serde_json::json!({"type":"string"}),
                chrono::Utc::now().timestamp_millis() + 60_000
            )
            .await
            .is_err()
        );
    }
}

struct ElicitationOwnerLost;
impl super::super::recovery::StreamExecutionLossVerifier for ElicitationOwnerLost {
    fn verify_stream_loss(
        &self,
        _: &SessionStreamId,
        through: &EventCursor,
    ) -> Result<PayloadRecord, SessionError> {
        Ok(PayloadRecord::json(
            "session.evidence/v1",
            &serde_json::json!({"source":"fixture_owner_joined", "through":through}),
        )
        .map_err(AppendError::from)?)
    }
}

#[tokio::test]
async fn recovery_settles_pending_elicitation_once_and_rejects_late_answer() {
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::TempDir::new().expect("directory");
        let (port, _) = session(dir.path(), backend);
        port.request_elicitation(
            "lost-request",
            "fixture question",
            &serde_json::json!({"type":"string"}),
            chrono::Utc::now().timestamp_millis() + 60_000,
        )
        .await
        .expect("request");
        let receipt = port
            .owner
            .recover_interrupted_stream(
                port.stream.clone(),
                "fixture-owner-loss".into(),
                Arc::new(ElicitationOwnerLost),
            )
            .await
            .expect("recovery")
            .expect("settlement");
        assert_eq!(
            port.owner
                .recover_interrupted_stream(
                    port.stream.clone(),
                    "fixture-owner-loss".into(),
                    Arc::new(ElicitationOwnerLost)
                )
                .await
                .expect("retry"),
            Some(receipt)
        );
        assert!(
            port.resolve_elicitation(
                "lost-request",
                &serde_json::json!({"state":"answered","answer":"too late"})
            )
            .await
            .is_err()
        );
        let page = port
            .query()
            .read_page(
                port.stream.clone(),
                EventCursor::beginning(port.stream.clone()),
                None,
                256,
                4 * 1024 * 1024,
            )
            .await
            .expect("events");
        let resolved: Vec<_> = page
            .events
            .iter()
            .filter(|e| e.body.kind() == EventKind::ElicitationResolved)
            .collect();
        assert_eq!(resolved.len(), 1);
        let EventData::ElicitationResolved { resolution_ref, .. } = &resolved[0].body.data else {
            unreachable!()
        };
        let PayloadRead::Present(payload) = port
            .owner
            .store
            .read_payload(&resolution_ref.payload_id)
            .expect("resolution")
        else {
            panic!("missing resolution")
        };
        let body: serde_json::Value = serde_json::from_slice(&payload.body).expect("body");
        assert_eq!(body["state"], "interrupted");
        assert_eq!(body["reason_key"], "elicitation_execution_owner_lost");
        zeroclaw_api::session_events::rules::HistoryIndex::from_history(&page.events)
            .expect("replay");
    }
}

#[tokio::test]
async fn unknown_delivery_stops_same_turn_provider_and_child_admission() {
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::TempDir::new().expect("directory");
        let (session, _) = session(dir.path(), backend);
        let active = session.clone();
        let command = input();
        let turn = command.turn_id.clone();
        let result = session
            .execute(
                command,
                Box::pin(async move {
                    let error = active
                        .deliver(
                            DeliveryIntent {
                                turn_id: Some(turn),
                                transport: "fixture".into(),
                                target: "fixture-viewer".into(),
                                content: "partial".into(),
                                completion_boundary: "adapter_acceptance".into(),
                            },
                            Box::pin(async { anyhow::bail!("fixture disconnect after send") }),
                        )
                        .await
                        .expect_err("uncertain delivery");
                    assert!(error.downcast_ref::<SessionCaptureStopped>().is_some());
                    let capture = current_provider_admission().expect("provider scope");
                    assert!(
                        capture
                            .admit(ProviderDispatchIntent {
                                requested_route: None,
                                previous_attempt_event_id: None,
                                tool_round_policy:
                                    zeroclaw_api::model_provider::ToolRoundPolicy::Batch,
                                thinking: None,
                                provider_ref: "fixture".into(),
                                model: "fixture".into(),
                                messages: vec![],
                                tools: None,
                                temperature: None
                            })
                            .await
                            .is_err()
                    );
                    assert!(
                        capture
                            .begin_child(ChildDispatchIntent {
                                relation: "delegate".into(),
                                target_agent: "child".into(),
                                prompt: "seed".into(),
                                execution_mode: ChildExecutionMode::Sync,
                                task_id: None
                            })
                            .await
                            .is_err()
                    );
                    Err(error)
                }),
            )
            .await;
        assert!(result.is_err());
        let page = session
            .query()
            .read_page(
                session.stream.clone(),
                EventCursor::beginning(session.stream.clone()),
                None,
                256,
                4 * 1024 * 1024,
            )
            .await
            .expect("history");
        assert_eq!(
            page.events
                .iter()
                .filter(|e| e.body.kind() == EventKind::InvocationOutcomeUnknown)
                .count(),
            1
        );
        assert!(!page.events.iter().any(|e| matches!(
            e.body.kind(),
            EventKind::ProviderAttemptStarted
                | EventKind::ChildInvocationRequested
                | EventKind::ToolEffectStarted
        )));
    }
}

#[tokio::test]
async fn ordered_opaque_input_round_trips_and_retry_does_not_execute() {
    for backend in ["sqlite", "jsonl"] {
        let directory = tempfile::tempdir().expect("directory");
        let (session, _) = session(directory.path(), backend);
        let mut command = input();
        command.parts = vec![
            EngineInputPart::Text("before".into()),
            EngineInputPart::Opaque {
                type_name: "future_content".into(),
                body: serde_json::json!({"type":"future_content","value":[1,2]}),
            },
            EngineInputPart::Text("after".into()),
        ];
        assert_eq!(
            session
                .execute(command.clone(), Box::pin(async { Ok("answer".into()) }))
                .await
                .expect("execute"),
            "answer"
        );
        let head = session.owner.store.head(&session.stream).expect("head");
        assert_eq!(
            session
                .execute(
                    command.clone(),
                    Box::pin(async { panic!("retry must not execute") })
                )
                .await
                .expect("retry"),
            "answer"
        );
        assert_eq!(
            session.owner.store.head(&session.stream).expect("head"),
            head
        );
        let messages = session
            .query()
            .messages(session.stream.clone())
            .await
            .expect("messages");
        assert_eq!(messages[0].text, "beforeafter");
        let page = session
            .query()
            .read_page(
                session.stream.clone(),
                EventCursor::beginning(session.stream.clone()),
                None,
                256,
                4 * 1024 * 1024,
            )
            .await
            .expect("page");
        let event = page
            .events
            .iter()
            .find(|event| event.body.kind() == EventKind::UserContentRecorded)
            .expect("input");
        let PayloadRead::Present(record) = session
            .query()
            .read_payload(
                session.stream.clone(),
                super::super::query::PayloadSelector {
                    event_sequence: event.sequence,
                    reference_path: vec![0],
                },
                4 * 1024 * 1024,
            )
            .await
            .expect("content")
        else {
            panic!("content missing")
        };
        let content: ContentPayload = serde_json::from_slice(&record.body).expect("content codec");
        assert_eq!(content.parts.len(), 3);
        assert!(
            matches!(&content.parts[1], RecordedContentPart::Opaque {type_name,..} if type_name=="future_content")
        );
        let PayloadRead::Present(body) = session
            .query()
            .read_payload(
                session.stream.clone(),
                super::super::query::PayloadSelector {
                    event_sequence: event.sequence,
                    reference_path: vec![0, 1],
                },
                4 * 1024 * 1024,
            )
            .await
            .expect("opaque")
        else {
            panic!("opaque missing")
        };
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body.body).expect("opaque codec"),
            serde_json::json!({"type":"future_content","value":[1,2]})
        );
        if let EngineInputPart::Opaque { body, .. } = &mut command.parts[1] {
            *body = serde_json::json!({"changed":true});
        }
        assert!(
            session
                .execute(
                    command,
                    Box::pin(async { panic!("conflict must not execute") })
                )
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn file_owner_identity_and_bytes_survive_idle_registration_and_retry() {
    for backend in ["sqlite", "jsonl"] {
        let directory = tempfile::tempdir().expect("directory");
        let (session, authority) = session(directory.path(), backend);
        let file = EngineFileInput {
            file_id: "upload-owner-file".into(),
            registration: serde_json::json!({"source":"fixture_upload_owner","filename":"fixture.bin"}),
            representation: PayloadRecord::new(
                PayloadId::new(),
                "session.bytes/v1".into(),
                vec![0, 1, 2, 255],
            )
            .expect("bytes"),
            policy_evidence: serde_json::json!({"source":"fixture_authorized_upload"}),
        };
        session
            .record_file(file.clone(), "upload_rendition_observed")
            .await
            .expect("record");
        let head = session.owner.store.head(&session.stream).expect("head");
        let mut retry = file.clone();
        retry.representation.payload_id = PayloadId::new();
        session
            .record_file(retry, "upload_rendition_observed")
            .await
            .expect("retry");
        assert_eq!(
            session.owner.store.head(&session.stream).expect("head"),
            head
        );
        let mut changed = file.clone();
        changed.representation =
            PayloadRecord::new(PayloadId::new(), "session.bytes/v1".into(), vec![9])
                .expect("changed bytes");
        assert!(
            session
                .record_file(changed, "upload_rendition_observed")
                .await
                .is_err()
        );
        let page = session
            .query()
            .read_page(
                session.stream.clone(),
                EventCursor::beginning(session.stream.clone()),
                None,
                256,
                4 * 1024 * 1024,
            )
            .await
            .expect("page");
        let registered = page
            .events
            .iter()
            .find(|event| event.body.kind() == EventKind::FileRegistered)
            .expect("registered");
        let PayloadRead::Present(bytes) = session
            .query()
            .read_payload(
                session.stream.clone(),
                super::super::query::PayloadSelector {
                    event_sequence: registered.sequence,
                    reference_path: vec![0, 1],
                },
                1024,
            )
            .await
            .expect("bytes")
        else {
            panic!("bytes missing")
        };
        assert_eq!(bytes.body, vec![0, 1, 2, 255]);
        let mut input = input();
        input.parts.push(EngineInputPart::File(file.clone()));
        session
            .execute(input, Box::pin(async { Ok("attached".into()) }))
            .await
            .expect("attach");
        let page = session
            .query()
            .read_page(
                session.stream.clone(),
                EventCursor::beginning(session.stream.clone()),
                None,
                256,
                4 * 1024 * 1024,
            )
            .await
            .expect("page");
        assert_eq!(
            page.events
                .iter()
                .filter(|event| event.body.kind() == EventKind::FileRegistered)
                .count(),
            1
        );
        assert_eq!(
            page.events
                .iter()
                .filter(|event| event.body.kind() == EventKind::FileAttached)
                .count(),
            1
        );
        authority.0.store(false, Ordering::SeqCst);
        assert!(
            session
                .record_file(file, "upload_rendition_observed")
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn cooperative_turn_stop_retains_uncertain_effects_without_claiming_remote_cancellation() {
    for backend in ["sqlite", "jsonl"] {
        for pending_provider in [false, true] {
            let dir = tempfile::TempDir::new().expect("directory");
            let (session, _) = session(dir.path(), backend);
            let input = input();
            let turn = TurnId::parse(input.turn_id.clone()).expect("turn");
            let result = session
                .execute(
                    input,
                    Box::pin(async move {
                        if pending_provider {
                            let observation = current_provider_admission()
                                .expect("capture")
                                .admit(ProviderDispatchIntent {
                                    requested_route: None,
                                    previous_attempt_event_id: None,
                                    tool_round_policy:
                                        zeroclaw_api::model_provider::ToolRoundPolicy::Batch,
                                    thinking: None,
                                    provider_ref: "fixture".into(),
                                    model: "fixture".into(),
                                    messages: vec![ChatMessage::user("fixture")],
                                    tools: None,
                                    temperature: None,
                                })
                                .await?;
                            drop(observation);
                        }
                        Err(zeroclaw_api::turn_stop::ToolLoopCancelled.into())
                    }),
                )
                .await;
            assert!(result.is_err());
            let page = session
                .query()
                .read_page(
                    session.stream.clone(),
                    EventCursor::beginning(session.stream.clone()),
                    None,
                    256,
                    4 * 1024 * 1024,
                )
                .await
                .expect("page");
            let terminal = page
                .events
                .iter()
                .find(|event| {
                    event.turn_id.as_ref() == Some(&turn)
                        && event.body.kind() == EventKind::TurnCancelled
                })
                .expect("cancelled turn");
            let EventData::TurnCancelled {
                unresolved_invocation_ids,
                ..
            } = &terminal.body.data
            else {
                panic!("kind")
            };
            assert_eq!(
                unresolved_invocation_ids.len(),
                usize::from(pending_provider)
            );
            assert_eq!(
                page.events
                    .iter()
                    .filter(|event| event.body.kind() == EventKind::InvocationOutcomeUnknown)
                    .count(),
                usize::from(pending_provider)
            );
            assert!(
                !page
                    .events
                    .iter()
                    .any(|event| event.body.kind() == EventKind::InvocationCancelled)
            );
        }
    }
}

#[tokio::test]
async fn message_pages_freeze_selection_preserve_ids_and_recheck_authority() {
    for backend in ["sqlite", "jsonl"] {
        let directory = tempfile::TempDir::new().expect("directory");
        let (session, authority) = session(directory.path(), backend);
        let a = session.append_assistant("first").await.expect("first");
        let b = session.append_assistant("second").await.expect("second");
        let page = session
            .query()
            .message_page(session.stream.clone(), None, 1)
            .await
            .expect("last page");
        assert_eq!(page.messages.len(), 1);
        assert_eq!(page.messages[0].message_id, b);
        let cursor = page.next_cursor.expect("cursor");
        session.append_assistant("later").await.expect("later");
        let prior = session
            .query()
            .message_page(session.stream.clone(), Some(cursor.clone()), 1)
            .await
            .expect("prior page");
        assert_eq!(prior.messages.len(), 1);
        assert_eq!(prior.messages[0].message_id, a);
        assert!(prior.next_cursor.is_none());
        let mut foreign = cursor.clone();
        foreign.through.stream_id = SessionStreamId::new();
        assert!(
            session
                .query()
                .message_page(session.stream.clone(), Some(foreign), 1)
                .await
                .is_err()
        );
        authority.0.store(false, Ordering::SeqCst);
        assert!(
            session
                .query()
                .message_page(session.stream.clone(), Some(cursor), 1)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn native_search_uses_visible_messages_and_current_authority_and_erasure() {
    for backend in ["sqlite", "jsonl"] {
        let directory = tempfile::TempDir::new().expect("directory");
        let (session, authority) = session(directory.path(), backend);
        let first = session
            .append_assistant("中文会话事件 TOKEN")
            .await
            .expect("first");
        let second = session
            .append_assistant("unrelated answer")
            .await
            .expect("second");
        let query = session.query();
        let hits = query
            .search_messages(session.stream.clone(), "token", 1)
            .await
            .expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].message_id, first);
        assert_eq!(
            query
                .search_messages(session.stream.clone(), "文会", 1)
                .await
                .expect("unicode")[0]
                .message_id,
            first
        );
        authority.0.store(false, Ordering::SeqCst);
        assert!(matches!(
            query
                .search_messages(session.stream.clone(), "TOKEN", 1)
                .await,
            Err(SessionError::PermissionDenied(_))
        ));
        authority.0.store(true, Ordering::SeqCst);
        let page = session
            .owner
            .store
            .read_page(
                &session.stream,
                &EventCursor::beginning(session.stream.clone()),
                None,
                100,
                1024 * 1024,
            )
            .expect("events");
        let event = page
            .events
            .iter()
            .find(|event| matches!(event.body.data, EventData::AssistantContentRecorded { .. }))
            .expect("message");
        let EventData::AssistantContentRecorded { content_ref, .. } = &event.body.data else {
            unreachable!()
        };
        let PayloadRead::Present(record) = session
            .owner
            .store
            .read_payload(&content_ref.payload_id)
            .expect("content")
        else {
            panic!("present")
        };
        let content: ContentPayload = serde_json::from_slice(&record.body).expect("decode");
        let RecordedContentPart::Text { text_ref } = &content.parts[0] else {
            panic!("text")
        };
        let intent = SessionEvent::typed(
            session.stream.clone(),
            EventData::PayloadErasureRequested {
                targets: vec![text_ref.clone()],
                reason_category: "operator_requested".into(),
            },
            session.actor.clone(),
        )
        .expect("intent");
        session
            .owner
            .admit_erasure(
                PrincipalId::shared_operator(),
                authority.clone(),
                AppendRequest {
                    stream_id: session.stream.clone(),
                    expected_head: page.through,
                    idempotency_key: "erase-search-leaf".into(),
                    events: vec![intent],
                    payloads: vec![],
                    erasure_reference_commitment: None,
                },
            )
            .await
            .expect("erase intent");
        assert!(
            query
                .search_messages(session.stream.clone(), "TOKEN", 1)
                .await
                .expect("search after erasure")
                .is_empty()
        );
        assert_eq!(
            query
                .search_messages(session.stream.clone(), "unrelated", 1)
                .await
                .expect("remaining search")[0]
                .message_id,
            second
        );
        let messages = query
            .messages(session.stream.clone())
            .await
            .expect("remaining history");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].message_id, first);
        assert!(messages[0].text.is_empty());
        assert_eq!(messages[0].unavailable_parts[0].part_index, 0);
        assert_eq!(
            messages[0].unavailable_parts[0].disposition,
            payloads::PayloadDisposition::ErasurePending
        );
        assert_eq!(messages[1].text, "unrelated answer");
        let latest = query
            .message_page(session.stream.clone(), None, 1)
            .await
            .expect("latest");
        let frozen = latest.next_cursor.expect("older cursor");
        let older = query
            .message_page(session.stream.clone(), Some(frozen.clone()), 1)
            .await
            .expect("older erased row");
        assert_eq!(older.messages[0].message_id, first);
        let events = session
            .owner
            .stream_history(&session.stream)
            .expect("events");
        let intent = events
            .iter()
            .find(|event| event.body.kind() == EventKind::PayloadErasureRequested)
            .expect("intent");
        session
            .owner
            .append(AppendRequest {
                stream_id: session.stream.clone(),
                expected_head: session.owner.store.head(&session.stream).expect("head"),
                idempotency_key: "confirm-search-erasure".into(),
                events: vec![
                    SessionEvent::typed(
                        session.stream.clone(),
                        EventData::PayloadRedacted {
                            erasure_requested_event_id: intent.event_id.clone(),
                        },
                        session.actor.clone(),
                    )
                    .expect("redacted"),
                ],
                payloads: vec![],
                erasure_reference_commitment: None,
            })
            .expect("confirm erasure");
        let older = query
            .message_page(session.stream.clone(), Some(frozen), 1)
            .await
            .expect("frozen cursor after confirmation");
        assert_eq!(
            older.messages[0].unavailable_parts[0].disposition,
            payloads::PayloadDisposition::Redacted
        );
        authority.0.store(false, Ordering::SeqCst);
        assert!(matches!(
            query.messages(session.stream.clone()).await,
            Err(SessionError::PermissionDenied(_))
        ));
    }
}

#[tokio::test]
async fn frozen_branches_keep_parent_messages_ids_and_ignore_later_parent_edits() {
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::TempDir::new().expect("directory");
        let (session, _) = session(dir.path(), backend);
        session.append_assistant("inherited").await.expect("append");
        let inherited = session
            .query()
            .messages(session.stream.clone())
            .await
            .expect("messages");
        let inherited_selection = ModelOverrideSelection::Set {
            provider_ref: "fixture".into(),
            model: "frozen-model".into(),
            thinking: None,
            route_mode: ModelRouteMode::Manual,
        };
        session
            .observe_model_change(inherited_selection.clone(), ModelChangeDisposition::Applied)
            .await
            .expect("initial model");
        let cursor = session.owner.head(&session.stream).expect("head");
        let child = session
            .fork(BranchId::new(), cursor.clone())
            .await
            .expect("fork");
        session
            .observe_model_change(
                ModelOverrideSelection::Clear,
                ModelChangeDisposition::Applied,
            )
            .await
            .expect("later model clear");
        assert_eq!(
            child
                .restore_model_override()
                .await
                .expect("frozen selection"),
            Some(inherited_selection)
        );
        child
            .observe_model_change(
                ModelOverrideSelection::Set {
                    provider_ref: "fixture".into(),
                    model: "branch-model".into(),
                    thinking: None,
                    route_mode: ModelRouteMode::Manual,
                },
                ModelChangeDisposition::Applied,
            )
            .await
            .expect("branch selection");
        let model_history = session
            .owner
            .stream_history(&session.stream)
            .expect("model history");
        let previous = model_history
            .iter()
            .find(|event| {
                event.branch_id.as_ref() == Some(&child.branch)
                    && event.body.kind() == EventKind::SessionModelOverrideChanged
            })
            .expect("branch change");
        assert!(
            matches!(&previous.body.data, EventData::SessionModelOverrideChanged {resolution:ModelResolution::Applied {previous_selection_event_id:Some(id)}} if model_history.iter().any(|event| &event.event_id==id && event.branch_id.as_ref()==Some(&session.branch) && event.sequence<=cursor.sequence))
        );
        let inherited_event = session
            .owner
            .stream_history(&session.stream)
            .expect("history")
            .into_iter()
            .find(|e| e.body.kind() == EventKind::AssistantContentRecorded)
            .expect("content");
        session
            .hide_content(inherited_event.event_id, "fixture_hide")
            .await
            .expect("hide");
        session
            .append_assistant("parent later")
            .await
            .expect("append later");
        assert_eq!(
            session
                .query()
                .messages(session.stream.clone())
                .await
                .expect("parent")[0]
                .text,
            "parent later"
        );
        let child_messages = child
            .query()
            .messages(child.stream.clone())
            .await
            .expect("child");
        assert_eq!(child_messages.len(), 1);
        assert_eq!(child_messages[0].message_id, inherited[0].message_id);
        assert_eq!(
            child
                .restore_context()
                .await
                .expect("restore")
                .expect("snapshot")
                .chat_projection()[0]
                .content,
            "inherited"
        );
        // A nested fork may freeze on an inherited ancestor coordinate.
        let nested = child
            .fork(BranchId::new(), cursor)
            .await
            .expect("nested ancestor fork");
        assert_eq!(
            nested
                .query()
                .messages(nested.stream.clone())
                .await
                .expect("nested")[0]
                .message_id,
            inherited[0].message_id
        );
        assert!(
            session
                .restore_context()
                .await
                .expect("restore parent")
                .expect("context")
                .chat_projection()
                .iter()
                .all(|message| message.content != "inherited")
        );
        let empty_branch = BranchId::new();
        let empty = session
            .fork(
                empty_branch.clone(),
                EventCursor::beginning(session.stream.clone()),
            )
            .await
            .expect("empty fork");
        session
            .fork(empty_branch, EventCursor::beginning(session.stream.clone()))
            .await
            .expect("idempotent empty fork");
        assert!(
            empty
                .query()
                .messages(empty.stream.clone())
                .await
                .expect("empty")
                .is_empty()
        );
    }
}

#[tokio::test]
async fn context_sources_keep_ids_unknown_positions_and_validate_references() {
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::TempDir::new().expect("directory");
        let (session, _) = session(dir.path(), backend);
        session.append_assistant("source").await.expect("append");
        let original = session.latest_context().await.expect("projection");
        let original_refs =
            super::super::context_messages::context_refs(session.owner.store.as_ref(), &original)
                .expect("refs");
        let context = session
            .restore_context()
            .await
            .expect("restore")
            .expect("context");
        let mut expanded = context.clone();
        expanded
            .messages
            .push(zeroclaw_api::model_provider::ConversationMessage::Chat(
                ChatMessage::user("new source"),
            ));
        session
            .replace_context(expanded.clone(), "fixture_expand")
            .await
            .expect("transform");
        let next = session.latest_context().await.expect("projection");
        let refs =
            super::super::context_messages::context_refs(session.owner.store.as_ref(), &next)
                .expect("refs");
        assert_eq!(refs[0], original_refs[0]);
        assert_ne!(refs[0].message_id, refs[1].message_id);
        session
            .replace_context(expanded, "fixture_noop")
            .await
            .expect("no change");
        let rows = serde_json::json!([{"type":"FutureConversation","data":{"note":"preserved raw"}},{"type":"Chat","data":{"role":"user","content":"known after unknown"}}]);
        let decoded = super::super::engine::decode_context_messages(
            &serde_json::to_vec(&rows).expect("bytes"),
        )
        .expect("forward compatibility");
        assert_eq!(decoded.len(), 1);
        assert!(
            matches!(&decoded[0],zeroclaw_api::model_provider::ConversationMessage::Chat(chat) if chat.content=="known after unknown")
        );
        assert!(
            super::super::engine::decode_context_messages(
                br#"[{"type":"Chat","data":{"role":1}}]"#
            )
            .is_err()
        );
        // A new context may not relabel an existing source or duplicate its ID.
        let mut bad = refs.clone();
        bad.push(refs[0].clone());
        let payload = PayloadRecord::json(
            "session.context/v1",
            &serde_json::json!({"message_refs":bad}),
        )
        .expect("bad context");
        let mut event = SessionEvent::typed(
            session.stream.clone(),
            EventData::ContextProjectionCreated {
                source_ranges: vec![],
                summary_ref: None,
                budget: 0,
                reason_key: "fixture_invalid".into(),
                algorithm_version: "fixture/v1".into(),
                result_ref: StoredPayloadRef::from(&payload),
                summary_source_refs: vec![],
                transform_started_event_id: None,
            },
            EventActor::default(),
        )
        .expect("event");
        event.branch_id = Some(session.branch.clone());
        assert!(
            session
                .owner
                .store
                .append_batch(AppendRequest {
                    stream_id: session.stream.clone(),
                    expected_head: session.owner.head(&session.stream).expect("head"),
                    idempotency_key: "invalid-context".into(),
                    events: vec![event],
                    payloads: vec![payload],
                    erasure_reference_commitment: None
                })
                .is_err()
        );
    }
}

#[tokio::test]
async fn queued_turn_drop_settles_without_polling_executor() {
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::TempDir::new().expect("directory");
        let (session, _) = session(dir.path(), backend);
        let lane = session.owner.foreground_lock(&session.stream);
        let permit = lane.lock().await;
        let command = input();
        let turn = TurnId::parse(&command.turn_id).expect("turn");
        let mut run = Box::pin(session.execute(
            command,
            Box::pin(async { panic!("queued executor must not run") }),
        ));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut run)
                .await
                .is_err()
        );
        drop(run);
        drop(permit);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let projection = session
                    .query()
                    .project_history(session.stream.clone())
                    .await
                    .expect("projection");
                if projection.turns.get(&turn).is_some_and(|view| {
                    view.status == zeroclaw_api::session_events::projection::TurnStatus::Cancelled
                }) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("queued cancellation settled");
    }
}

#[tokio::test]
async fn child_rejection_and_interrupted_initialization_are_durable_without_dispatch() {
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::TempDir::new().expect("directory");
        let (session, _) = session(dir.path(), backend);
        let active = session.clone();
        session.execute(input(),Box::pin(async move {
            let capture=current_provider_admission().expect("scope");
            assert!(capture.begin_child(ChildDispatchIntent {relation:"delegate".into(),target_agent:"denied-fixture".into(),prompt:"rejected prompt".into(),execution_mode:ChildExecutionMode::Sync,task_id:None}).await.is_err());
            assert!(capture.begin_child(ChildDispatchIntent {relation:"delegate".into(),target_agent:"actor-denied-fixture".into(),prompt:"actor rejected prompt".into(),execution_mode:ChildExecutionMode::Sync,task_id:None}).await.is_err());
            // Inject the exact durable state at the parent/child commit boundary.
            let turn=active.active_turn()?.expect("turn");
            let context=active.latest_context().await?;
            let target=PayloadRecord::json("session.evidence/v1",&serde_json::json!({"agent_alias":"fixture-child"}))?;
            let seed=PayloadRecord::json("session.bytes/v1",&serde_json::json!({"prompt":"frozen recovery prompt","parent_projection_event_id":context.event_id}))?;
            let evidence=PayloadRecord::json("session.evidence/v1",&serde_json::json!({"source":"fixture_admission"}))?;
            let invocation=InvocationId::new();
            let mut request=SessionEvent::typed(active.stream.clone(),EventData::ChildInvocationRequested {relation:"delegate".into(),execution_mode:ChildExecutionMode::Sync,target_agent_ref:StoredPayloadRef::from(&target),input_projection_ref:StoredPayloadRef::from(&seed),policy_evidence_ref:StoredPayloadRef::from(&evidence),task_id:None},EventActor::System {component:"child_executor".into()})?;
            request.branch_id=Some(active.branch.clone());request.turn_id=Some(turn);request.invocation_id=Some(invocation.clone());
            active.owner.store.append_batch(AppendRequest {stream_id:active.stream.clone(),expected_head:active.owner.head(&active.stream)?,idempotency_key:"fixture-child-boundary".into(),events:vec![request.clone()],payloads:vec![target,seed,evidence],erasure_reference_commitment:None})?;
            let request=active.owner.store.read_event(&request.event_id)?.expect("committed request");
            let child=active.owner.recover_requested_child(request.clone(),Arc::new(ElicitationOwnerLost)).await?;
            let head=active.owner.head(&child)?;
            assert_eq!(active.owner.recover_requested_child(request,Arc::new(ElicitationOwnerLost)).await?,child);
            assert_eq!(active.owner.head(&child)?,head);
            let events=active.owner.stream_history(&child)?;
            assert_eq!(events.iter().filter(|event|event.body.kind()==EventKind::DerivedSessionStarted).count(),1);
            assert!(!events.iter().any(|event| matches!(event.body.kind(),EventKind::TurnAccepted|EventKind::ProviderAttemptStarted|EventKind::ToolEffectStarted)));
            Ok("parent finished".into())
        })).await.expect_err("unstarted recovered child keeps parent outcome unknown");
        let events = session
            .owner
            .stream_history(&session.stream)
            .expect("history");
        assert_eq!(
            events
                .iter()
                .filter(|event| event.body.kind() == EventKind::ChildInvocationRejected)
                .count(),
            2
        );
    }
}

#[tokio::test]
async fn final_context_and_turn_share_one_content_creation_and_retry_result() {
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::tempdir().expect("directory");
        let (session, _) = session(dir.path(), backend);
        let active = session.clone();
        let command = input();
        session
            .execute(
                command.clone(),
                Box::pin(async move {
                    active
                        .replace_context(
                            ContextSnapshot {
                                messages: vec![
                                    zeroclaw_api::model_provider::ConversationMessage::Chat(
                                        ChatMessage::assistant("answer"),
                                    ),
                                ],
                                history_has_trim_breadcrumb: None,
                            },
                            "final_answer",
                        )
                        .await?;
                    Ok("answer".into())
                }),
            )
            .await
            .expect("execute");
        let projection = session.latest_context().await.expect("projection");
        let refs =
            super::super::context_messages::context_refs(session.owner.store.as_ref(), &projection)
                .expect("refs");
        let ContextMessageSource::Content {
            content_event_id, ..
        } = &refs[0].source
        else {
            panic!("canonical content source");
        };
        let events = session
            .owner
            .stream_history(&session.stream)
            .expect("history");
        let contents: Vec<_> = events
            .iter()
            .filter(|e| e.body.kind() == EventKind::AssistantContentRecorded)
            .collect();
        assert_eq!(contents.len(), 1);
        assert_eq!(&contents[0].event_id, content_event_id);
        assert_eq!(
            session
                .execute(command, Box::pin(async { panic!("retry cannot dispatch") }))
                .await
                .expect("retry"),
            "answer"
        );
        let PayloadRead::Present(payload) = session
            .owner
            .store
            .read_payload(match &contents[0].body.data {
                EventData::AssistantContentRecorded { content_ref, .. } => &content_ref.payload_id,
                _ => unreachable!(),
            })
            .expect("content")
        else {
            panic!("present");
        };
        let content: ContentPayload = serde_json::from_slice(&payload.body).expect("body");
        assert_eq!(content.message_id, refs[0].message_id);
    }
}

#[tokio::test]
async fn reopened_queued_inputs_preserve_order_parts_and_current_authorization() {
    for backend in ["sqlite", "jsonl"] {
        for had_started in [false, true] {
            let dir = tempfile::TempDir::new().expect("directory");
            let (port, authority) = session(dir.path(), backend);
            let mut first = input();
            first.turn_id = "recovery-z-first".into();
            first.execution_workspace = Some("/fixture/admitted-workspace".into());
            first.reply_route =
                Some(serde_json::json!({"transport":"fixture", "target":"original-thread"}));
            first.parts = vec![
                EngineInputPart::Text("first before crash".into()),
                EngineInputPart::File(EngineFileInput {
                    file_id: "queued-file".into(),
                    registration: serde_json::json!({"name":"fixture.bin"}),
                    representation: PayloadRecord::new(
                        PayloadId::new(),
                        "session.bytes/v1".into(),
                        vec![0, 1, 2, 3],
                    )
                    .expect("bytes"),
                    policy_evidence: serde_json::json!({"source":"fixture_admission"}),
                }),
                EngineInputPart::Opaque {
                    type_name: "fixture_extension".into(),
                    body: serde_json::json!({"value":"preserved"}),
                },
                EngineInputPart::Text("last part".into()),
            ];
            let mut second = input();
            second.turn_id = "recovery-a-second".into();
            second.parts = vec![EngineInputPart::Text("second before crash".into())];
            for (index, pending) in [first.clone(), second.clone()].into_iter().enumerate() {
                let command = port.input_command(pending).await.expect("command");
                let accepted = port
                    .owner
                    .admit_user_input(&command, &port.actor, &port.actor, port.authority.clone())
                    .await
                    .expect("accepted durable")
                    .detach_for_recovery();
                if had_started && index == 0 {
                    let mut started = SessionEvent::typed(
                        port.stream.clone(),
                        EventData::TurnStarted {
                            accepted_event_id: accepted,
                            execution_binding_ref: command.execution_binding_ref.clone(),
                        },
                        port.actor.clone(),
                    )
                    .expect("started");
                    started.branch_id = Some(port.branch.clone());
                    started.turn_id = Some(command.turn_id.clone());
                    port.owner
                        .store
                        .append_batch(AppendRequest {
                            stream_id: port.stream.clone(),
                            expected_head: port.owner.store.head(&port.stream).expect("head"),
                            idempotency_key: format!("turn-start:{}", command.turn_id),
                            events: vec![started],
                            payloads: vec![],
                            erasure_reference_commitment: None,
                        })
                        .expect("start committed before engine poll");
                }
            }
            let stream = port.stream.clone();
            let branch = port.branch.clone();
            drop(port);
            let store = zeroclaw_infra::session_events::open_event_store(dir.path(), backend)
                .expect("reopen");
            let owner = Arc::new(SessionCoordinator::new(store));
            owner
                .recover_interrupted_stream(
                    stream.clone(),
                    "fixture-restart".into(),
                    Arc::new(ElicitationOwnerLost),
                )
                .await
                .expect("recover");
            let port = EngineSession::new(
                owner,
                stream.clone(),
                branch,
                PrincipalId::shared_operator(),
                authority.clone(),
            );
            authority.0.store(false, Ordering::SeqCst);
            assert!(
                port.pending_inputs("current-provider".into(), "current-model".into())
                    .await
                    .is_err()
            );
            authority.0.store(true, Ordering::SeqCst);
            let pending = port
                .pending_inputs("current-provider".into(), "current-model".into())
                .await
                .expect("pending");
            assert_eq!(
                pending
                    .iter()
                    .map(|input| input.turn_id.as_str())
                    .collect::<Vec<_>>(),
                vec![first.turn_id.as_str(), second.turn_id.as_str()]
            );
            assert_eq!(pending[0].reply_route, first.reply_route);
            assert_eq!(pending[0].execution_workspace, first.execution_workspace);
            assert_eq!(pending[0].ingress, first.ingress);
            assert_eq!(pending[0].provider_ref, "current-provider");
            assert_eq!(pending[0].model, "current-model");
            assert!(
                matches!(&pending[0].parts[0],EngineInputPart::Text(text) if text=="first before crash")
            );
            let EngineInputPart::File(file) = &pending[0].parts[1] else {
                panic!("file")
            };
            assert_eq!(file.representation.body, vec![0, 1, 2, 3]);
            assert_eq!(file.registration, serde_json::json!({"name":"fixture.bin"}));
            assert_eq!(
                file.policy_evidence,
                serde_json::json!({"source":"fixture_admission"})
            );
            assert!(
                matches!(&pending[0].parts[2],EngineInputPart::Opaque {type_name,body} if type_name=="fixture_extension" && body==&serde_json::json!({"value":"preserved"}))
            );
            assert!(
                matches!(&pending[0].parts[3],EngineInputPart::Text(text) if text=="last part")
            );
            let head = port.owner.store.head(&stream).expect("head");
            let command = port
                .input_command(pending[0].clone())
                .await
                .expect("current command");
            let binding = command
                .input_payloads
                .iter()
                .find(|payload| StoredPayloadRef::from(*payload) == command.execution_binding_ref)
                .expect("binding")
                .clone();
            let spec = PayloadRecord::json("session.transform/v1", &serde_json::json!({"reason_key":"fixture", "algorithm_version":"fixture/v1", "budget":0})).expect("spec");
            let mut early_transform = SessionEvent::typed(
                stream.clone(),
                EventData::ContextTransformStarted {
                    input_projection_event_id: command.context_event_id.clone(),
                    transform_spec_ref: StoredPayloadRef::from(&spec),
                    execution_binding_ref: command.execution_binding_ref.clone(),
                },
                port.actor.clone(),
            )
            .expect("transform");
            early_transform.branch_id = Some(port.branch.clone());
            early_transform.turn_id = Some(command.turn_id.clone());
            let refusal = port
                .owner
                .store
                .append_batch(AppendRequest {
                    stream_id: stream.clone(),
                    expected_head: head.clone(),
                    idempotency_key: "invalid-queued-transform".into(),
                    events: vec![early_transform],
                    payloads: vec![spec, binding.clone()],
                    erasure_reference_commitment: None,
                })
                .expect_err("requeued input has no running execution");
            assert!(refusal.to_string().contains("requeued turn must restart"));
            if !had_started {
                let history = port.owner.stream_history(&stream).expect("history");
                let requeued = history
                    .iter()
                    .find(|event| {
                        event.turn_id.as_ref() == Some(&command.turn_id)
                            && matches!(
                                event.body.data,
                                EventData::RecoveryObserved {
                                    disposition: RecoveryDisposition::Requeued,
                                    ..
                                }
                            )
                    })
                    .expect("requeued");
                let EventData::RecoveryObserved { evidence_ref, .. } = &requeued.body.data else {
                    unreachable!()
                };
                let mut rescheduled = SessionEvent::typed(
                    stream.clone(),
                    EventData::RecoveryObserved {
                        prior_execution_ref: None,
                        through_cursor: EventCursor {
                            stream_id: stream.clone(),
                            sequence: requeued.sequence,
                        },
                        disposition: RecoveryDisposition::Rescheduled {
                            execution_binding_ref: command.execution_binding_ref.clone(),
                        },
                        evidence_ref: evidence_ref.clone(),
                    },
                    port.actor.clone(),
                )
                .expect("rescheduled");
                rescheduled.branch_id = Some(port.branch.clone());
                rescheduled.turn_id = Some(command.turn_id.clone());
                rescheduled.causation_id = Some(requeued.event_id.clone());
                let refusal = port
                    .owner
                    .store
                    .append_batch(AppendRequest {
                        stream_id: stream.clone(),
                        expected_head: head.clone(),
                        idempotency_key: "invalid-first-reschedule".into(),
                        events: vec![rescheduled],
                        payloads: vec![binding],
                        erasure_reference_commitment: None,
                    })
                    .expect_err("first execution must create TurnStarted");
                assert!(
                    refusal
                        .to_string()
                        .contains("reschedule requires a started turn")
                );
            }
            assert_eq!(port.owner.store.head(&stream).expect("head"), head);
            assert!(
                port.execute(
                    pending[1].clone(),
                    Box::pin(async { panic!("cannot skip earlier queued input") })
                )
                .await
                .is_err()
            );
            assert_eq!(port.owner.store.head(&stream).expect("head"), head);
            assert!(
                port.execute(
                    input(),
                    Box::pin(async { panic!("new work cannot pass recovery queue") })
                )
                .await
                .is_err()
            );
            assert_eq!(port.owner.store.head(&stream).expect("head"), head);
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let first_calls = calls.clone();
            let duplicate_calls = calls.clone();
            let (first_result, duplicate_result) = tokio::join!(
                port.execute(
                    pending[0].clone(),
                    Box::pin(async move {
                        first_calls.fetch_add(1, Ordering::SeqCst);
                        let restored = current_engine_input().expect("bound recovered input");
                        assert_eq!(restored.turn_id, "recovery-z-first");
                        assert_eq!(restored.parts.len(), 4);
                        Ok("first result".into())
                    })
                ),
                port.execute(
                    pending[0].clone(),
                    Box::pin(async move {
                        duplicate_calls.fetch_add(1, Ordering::SeqCst);
                        Ok("first result".into())
                    })
                ),
            );
            assert_eq!(first_result.expect("first"), "first result");
            assert_eq!(duplicate_result.expect("same outcome"), "first result");
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            let remaining = port
                .pending_inputs("current-provider".into(), "current-model".into())
                .await
                .expect("remaining");
            assert_eq!(remaining.len(), 1);
            port.execute(
                remaining[0].clone(),
                Box::pin(async { Ok("second result".into()) }),
            )
            .await
            .expect("second");
            assert!(
                port.pending_inputs("current-provider".into(), "current-model".into())
                    .await
                    .expect("drained")
                    .is_empty()
            );
            let history = port.owner.stream_history(&stream).expect("history");
            assert_eq!(
                history
                    .iter()
                    .filter(|event| event.body.kind() == EventKind::TurnStarted)
                    .count(),
                2
            );
            let rescheduled = history
                .iter()
                .filter(|event| {
                    matches!(
                        event.body.data,
                        EventData::RecoveryObserved {
                            disposition: RecoveryDisposition::Rescheduled { .. },
                            ..
                        }
                    )
                })
                .count();
            assert_eq!(rescheduled, usize::from(had_started));
        }
    }
}

#[tokio::test]
async fn recovered_queued_input_resolves_the_current_canonical_agent_alias() {
    for backend in ["sqlite", "jsonl"] {
        let directory = tempfile::tempdir().unwrap();
        let (port, authority) = session(directory.path(), backend);
        let command = port.input_command(input()).await.unwrap();
        port.owner
            .admit_user_input(&command, &port.actor, &port.actor, port.authority.clone())
            .await
            .unwrap()
            .detach_for_recovery();
        let body = PayloadRecord::json("session.authority/v1", &"renamed-agent").unwrap();
        let event = SessionEvent::typed(
            port.stream.clone(),
            EventData::SessionMetadataChanged {
                field: "owning_agent".into(),
                new_value_ref: StoredPayloadRef::from(&body),
            },
            port.actor.clone(),
        )
        .unwrap();
        port.owner
            .store
            .append_batch(AppendRequest {
                stream_id: port.stream.clone(),
                expected_head: port.owner.store.head(&port.stream).unwrap(),
                idempotency_key: "agent-rename".into(),
                events: vec![event],
                payloads: vec![body],
                erasure_reference_commitment: None,
            })
            .unwrap();
        let stream = port.stream.clone();
        let branch = port.branch.clone();
        drop(port);
        let store =
            zeroclaw_infra::session_events::open_event_store(directory.path(), backend).unwrap();
        let owner = Arc::new(SessionCoordinator::new(store));
        owner
            .recover_interrupted_stream(
                stream.clone(),
                "restart".into(),
                Arc::new(ElicitationOwnerLost),
            )
            .await
            .unwrap();
        let port = EngineSession::new(
            owner,
            stream,
            branch,
            PrincipalId::shared_operator(),
            authority,
        );
        let pending = port
            .pending_inputs("current-provider".into(), "current-model".into())
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].agent_alias.as_deref(), Some("renamed-agent"));
    }
}

#[tokio::test]
async fn recovery_never_requeues_a_started_provider_effect() {
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::TempDir::new().expect("directory");
        let (port, _) = session(dir.path(), backend);
        let input = input();
        let ready = Arc::new(tokio::sync::Notify::new());
        let notify = ready.clone();
        let mut execution = Box::pin(port.execute(
            input.clone(),
            Box::pin(async move {
                let _observer = current_provider_admission()
                    .expect("provider capture")
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
                    .await
                    .expect("effect started");
                notify.notify_one();
                std::future::pending::<anyhow::Result<String>>().await
            }),
        ));
        tokio::select! {
            ()=ready.notified()=>{},
            result=&mut execution=>panic!("unexpected result: {result:?}"),
        }
        drop(execution);
        port.owner
            .recover_interrupted_stream(
                port.stream.clone(),
                "effect-owner-loss".into(),
                Arc::new(ElicitationOwnerLost),
            )
            .await
            .expect("recover");
        assert!(
            port.pending_inputs("fixture".into(), "fixture".into())
                .await
                .expect("pending")
                .is_empty()
        );
        assert!(
            port.execute(
                input,
                Box::pin(async { panic!("started effect cannot replay") })
            )
            .await
            .is_err()
        );
    }
}

#[tokio::test]
async fn closed_sessions_cancel_recoverable_inputs_without_dispatch() {
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::TempDir::new().expect("directory");
        let (port, _) = session(dir.path(), backend);
        let input = input();
        let command = port.input_command(input.clone()).await.expect("command");
        port.owner
            .admit_user_input(&command, &port.actor, &port.actor, port.authority.clone())
            .await
            .expect("admit")
            .detach_for_recovery();
        port.close("fixture_closed").await.expect("close");
        port.owner
            .recover_interrupted_stream(
                port.stream.clone(),
                "closed-recovery".into(),
                Arc::new(ElicitationOwnerLost),
            )
            .await
            .expect("recover");
        assert!(
            port.pending_inputs("fixture".into(), "fixture".into())
                .await
                .expect("pending")
                .is_empty()
        );
        let history = port.owner.stream_history(&port.stream).expect("history");
        assert!(history.iter().any(|event| matches!(&event.body.data, EventData::TurnCancelled {reason_key, ..} if reason_key == "session_closed_before_recovery")));
        assert!(
            !history
                .iter()
                .any(|event| event.body.kind() == EventKind::TurnStarted)
        );
    }
}

#[tokio::test]
async fn recovered_input_rejection_requires_current_authority_and_unblocks_queue() {
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::TempDir::new().expect("directory");
        let (port, authority) = session(dir.path(), backend);
        let first = input();
        let mut second = input();
        second.turn_id = TurnId::new().to_string();
        for input in [first.clone(), second.clone()] {
            let command = port.input_command(input).await.expect("command");
            port.owner
                .admit_user_input(&command, &port.actor, &port.actor, port.authority.clone())
                .await
                .expect("admit")
                .detach_for_recovery();
        }
        port.owner
            .recover_interrupted_stream(
                port.stream.clone(),
                "rejected-recovery".into(),
                Arc::new(ElicitationOwnerLost),
            )
            .await
            .expect("recover");
        let before = port.owner.store.head(&port.stream).expect("head");
        authority.0.store(false, Ordering::SeqCst);
        assert!(
            port.reject_pending_input(&first.turn_id, "fixture_permission_revoked")
                .await
                .is_err()
        );
        assert_eq!(port.owner.store.head(&port.stream).expect("head"), before);
        let other = EngineSession::new(
            port.owner.clone(),
            port.stream.clone(),
            port.branch.clone(),
            PrincipalId("other-actor".into()),
            authority.clone(),
        );
        assert!(
            other
                .execute(
                    first.clone(),
                    Box::pin(async { panic!("denied actor cannot dispatch") })
                )
                .await
                .is_err()
        );
        assert_eq!(port.owner.store.head(&port.stream).expect("head"), before);
        assert!(
            port.execute(
                first.clone(),
                Box::pin(async { panic!("revoked owner cannot dispatch") })
            )
            .await
            .is_err()
        );
        authority.0.store(true, Ordering::SeqCst);
        let pending = port
            .pending_inputs("fixture".into(), "fixture".into())
            .await
            .expect("pending");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].turn_id, second.turn_id);
        port.reject_pending_input(&second.turn_id, "fixture_sender_unavailable")
            .await
            .expect("reject");
        let after = port.owner.store.head(&port.stream).expect("head");
        port.reject_pending_input(&second.turn_id, "fixture_sender_unavailable")
            .await
            .expect("idempotent");
        assert_eq!(port.owner.store.head(&port.stream).expect("head"), after);
        assert!(
            port.pending_inputs("fixture".into(), "fixture".into())
                .await
                .expect("pending")
                .is_empty()
        );
        let mut next = input();
        next.turn_id = TurnId::new().to_string();
        assert_eq!(
            port.execute(next, Box::pin(async { Ok("new work".into()) }))
                .await
                .expect("unblocked"),
            "new work"
        );
        let history = port.owner.stream_history(&port.stream).expect("history");
        assert_eq!(
            history
                .iter()
                .filter(|event| event.body.kind() == EventKind::TurnCancelled)
                .count(),
            2
        );
    }
}

#[tokio::test]
async fn child_initialization_failure_settles_parent_without_a_child_handle() {
    struct InitializationFailureStore {
        inner: Arc<dyn SessionEventStore>,
        fail_after_commit: bool,
        fail_settlement: bool,
    }

    impl SessionEventStore for InitializationFailureStore {
        fn append_batch(&self, request: AppendRequest) -> Result<AppendReceipt, AppendError> {
            if request
                .events
                .iter()
                .any(|event| event.body.kind() == EventKind::DerivedSessionStarted)
            {
                if self.fail_after_commit {
                    self.inner.append_batch(request)?;
                }
                return Err(AppendError::StorageUnavailable(
                    "fixture child initialization write failed".into(),
                ));
            }
            if self.fail_settlement
                && request
                    .idempotency_key
                    .starts_with("child-initialization-failed:")
            {
                return Err(AppendError::StorageUnavailable(
                    "fixture child initialization settlement unavailable".into(),
                ));
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

    for backend in ["sqlite", "jsonl"] {
        for fail_after_commit in [false, true] {
            for fail_settlement in [false, true] {
                let directory = tempfile::TempDir::new().expect("directory");
                let (mut port, _) = session(directory.path(), backend);
                port.owner = Arc::new(SessionCoordinator::new(Arc::new(
                    InitializationFailureStore {
                        inner: port.owner.store.clone(),
                        fail_after_commit,
                        fail_settlement,
                    },
                )));
                let error = port
                    .execute(
                        input(),
                        Box::pin(async {
                            current_provider_admission()
                                .expect("capture")
                                .begin_child(ChildDispatchIntent {
                                    relation: "skill_reflection".into(),
                                    target_agent: "child".into(),
                                    prompt: "prepared input".into(),
                                    execution_mode: ChildExecutionMode::Background,
                                    task_id: None,
                                })
                                .await?;
                            panic!("failed initialization must not return a child handle");
                        }),
                    )
                    .await
                    .expect_err("child initialization failed");
                let diagnostic = format!("{error:#}");
                assert!(diagnostic.contains("fixture child initialization write failed"));
                if fail_settlement {
                    assert!(
                        diagnostic.contains("fixture child initialization settlement unavailable")
                    );
                }
                let history = port
                    .owner
                    .stream_history(&port.stream)
                    .expect("parent history");
                let requests: Vec<_> = history
                    .iter()
                    .filter(|event| event.body.kind() == EventKind::ChildInvocationRequested)
                    .collect();
                assert_eq!(requests.len(), 1);
                let request = requests[0];
                let terminals: Vec<_> = history
                    .iter()
                    .filter(|event| {
                        event.invocation_id == request.invocation_id
                            && zeroclaw_api::session_events::rules::terminal(&event.body.kind())
                    })
                    .collect();
                assert_eq!(terminals.len(), usize::from(!fail_settlement));
                if !fail_settlement {
                    let EventData::InvocationOutcomeUnknown {
                        last_observed_event_id,
                        reason_key,
                        error_ref: Some(error_ref),
                        ..
                    } = &terminals[0].body.data
                    else {
                        panic!("initialization failure must preserve uncertainty and its error");
                    };
                    assert_eq!(last_observed_event_id, &request.event_id);
                    assert_eq!(reason_key, "child_initialization_write_failed");
                    let PayloadRead::Present(payload) = port
                        .owner
                        .store
                        .read_payload(&error_ref.payload_id)
                        .expect("initialization error")
                    else {
                        panic!("initialization error payload missing");
                    };
                    assert_eq!(payload.codec, "session.error-detail/v1");
                    let detail: serde_json::Value =
                        serde_json::from_slice(&payload.body).expect("error JSON");
                    assert_eq!(detail["error_key"], "child_initialization_write_failed");
                    assert!(
                        detail["detail"]
                            .as_str()
                            .expect("diagnostic")
                            .contains("fixture child initialization write failed")
                    );
                }
                let child_stream = SessionStreamId::parse(format!(
                    "child-{}",
                    request.invocation_id.as_ref().expect("invocation")
                ))
                .expect("child stream");
                if fail_after_commit {
                    let child_history = port
                        .owner
                        .stream_history(&child_stream)
                        .expect("committed child history");
                    assert_eq!(
                        child_history
                            .iter()
                            .filter(|event| event.body.kind() == EventKind::DerivedSessionStarted)
                            .count(),
                        1
                    );
                    assert!(!child_history.iter().any(|event| matches!(
                        event.body.kind(),
                        EventKind::TurnAccepted
                            | EventKind::TurnStarted
                            | EventKind::ProviderAttemptStarted
                            | EventKind::ToolEffectStarted
                    )));
                } else {
                    assert_eq!(
                        port.owner.head(&child_stream).expect("child head").sequence,
                        0
                    );
                }
                zeroclaw_api::session_events::rules::HistoryIndex::from_history(&history)
                    .expect("valid parent history");
            }
        }
    }
}

#[tokio::test]
async fn unadmitted_child_interruption_closes_stream_without_inventing_a_turn() {
    for backend in ["sqlite", "jsonl"] {
        for previously_unknown in [false, true] {
            let dir = tempfile::TempDir::new().expect("directory");
            let (port, _) = session(dir.path(), backend);
            let (send, receive) = tokio::sync::oneshot::channel();
            port.execute(
                input(),
                Box::pin(async move {
                    let child = current_provider_admission()
                        .expect("capture")
                        .begin_child(ChildDispatchIntent {
                            relation: "skill_reflection".into(),
                            target_agent: "child".into(),
                            prompt: "prepared input".into(),
                            execution_mode: ChildExecutionMode::Background,
                            task_id: None,
                        })
                        .await?;
                    send.send(child)
                        .map_err(|_| anyhow::Error::msg("receiver lost"))?;
                    Ok("parent finished".into())
                }),
            )
            .await
            .expect("parent committed");
            let child = receive.await.expect("prepared child");
            if previously_unknown {
                port.owner
                    .recover_interrupted_stream(
                        port.stream.clone(),
                        "prepared-child-parent-loss".into(),
                        Arc::new(ElicitationOwnerLost),
                    )
                    .await
                    .expect("parent unknown");
            }
            child
                .interrupted("fixture_prepared_child_stopped")
                .await
                .expect("cancel prepared child");
            let settled = port.owner.head(&port.stream).expect("head");
            child
                .interrupted("fixture_prepared_child_stopped")
                .await
                .expect("idempotent interruption");
            assert_eq!(port.owner.head(&port.stream).expect("head"), settled);
            assert!(
                child
                    .execute(Box::pin(async {
                        panic!("closed child must not poll its engine")
                    }))
                    .await
                    .is_err()
            );
            let history = port
                .owner
                .stream_history(&port.stream)
                .expect("parent history");
            let request = history
                .iter()
                .find(|event| event.body.kind() == EventKind::ChildInvocationRequested)
                .expect("request");
            let child_stream = SessionStreamId::parse(format!(
                "child-{}",
                request.invocation_id.as_ref().expect("invocation")
            ))
            .expect("child stream");
            let child_history = port
                .owner
                .stream_history(&child_stream)
                .expect("child history");
            assert_eq!(
                child_history
                    .iter()
                    .filter(|event| event.body.kind() == EventKind::SessionClosed)
                    .count(),
                1
            );
            assert!(!child_history.iter().any(|event| matches!(
                event.body.kind(),
                EventKind::TurnAccepted
                    | EventKind::TurnStarted
                    | EventKind::TurnCancelled
                    | EventKind::ProviderAttemptStarted
                    | EventKind::ToolEffectStarted
            )));
            let terminal = history
                .iter()
                .filter(|event| {
                    event.invocation_id == request.invocation_id
                        && zeroclaw_api::session_events::rules::terminal(&event.body.kind())
                })
                .collect::<Vec<_>>();
            assert_eq!(terminal.len(), 1);
            assert_eq!(
                terminal[0].body.kind(),
                if previously_unknown {
                    EventKind::InvocationOutcomeUnknown
                } else {
                    EventKind::InvocationCancelled
                }
            );
            assert!(
                !history
                    .iter()
                    .any(|event| event.body.kind() == EventKind::OutcomeReconciled)
            );
            if let EventData::InvocationCancelled {
                verification_evidence_ref,
                ..
            } = &terminal[0].body.data
            {
                let PayloadRead::Present(evidence) = port
                    .owner
                    .store
                    .read_payload(&verification_evidence_ref.payload_id)
                    .expect("evidence")
                else {
                    panic!("cancellation evidence absent")
                };
                let evidence: serde_json::Value =
                    serde_json::from_slice(&evidence.body).expect("evidence JSON");
                assert_eq!(
                    evidence["source"],
                    "host_child_future_stopped_before_admission"
                );
                assert_eq!(
                    evidence["derived_event_id"],
                    serde_json::to_value(&child_history[0].event_id).expect("root ID")
                );
                let cursor: EventCursor =
                    serde_json::from_value(evidence["child_closed_cursor"].clone())
                        .expect("closed cursor");
                assert_eq!(cursor.stream_id, child_stream);
                assert_eq!(
                    cursor.sequence,
                    child_history.last().expect("closed").sequence
                );
            }
        }
    }
}

#[tokio::test]
async fn stopped_child_without_effect_is_cancelled_and_settles_parent() {
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::TempDir::new().expect("directory");
        let (port, _) = session(dir.path(), backend);
        port.execute(
            input(),
            Box::pin(async {
                let child = current_provider_admission()
                    .expect("capture")
                    .begin_child(ChildDispatchIntent {
                        relation: "delegate".into(),
                        target_agent: "child".into(),
                        prompt: "child input".into(),
                        execution_mode: ChildExecutionMode::Sync,
                        task_id: None,
                    })
                    .await?;
                let ready = Arc::new(tokio::sync::Notify::new());
                let notified = ready.clone();
                let mut future = Box::pin(child.execute(Box::pin(async move {
                    notified.notify_one();
                    std::future::pending::<anyhow::Result<String>>().await
                })));
                tokio::select! {
                    ()=ready.notified()=>{},
                    result=&mut future=>panic!("unexpected result {result:?}"),
                }
                drop(future);
                child.interrupted("fixture_child_owner_stopped").await?;
                Ok("parent observed cancellation".into())
            }),
        )
        .await
        .expect("parent");
        let history = port.owner.stream_history(&port.stream).expect("history");
        let terminal = history
            .iter()
            .find_map(|event| match &event.body.data {
                EventData::ChildInvocationCompleted {
                    child_terminal_cursor,
                    ..
                } => Some(child_terminal_cursor),
                _ => None,
            })
            .expect("child settled");
        let child = port
            .owner
            .stream_history(&terminal.stream_id)
            .expect("child history");
        assert!(child.iter().any(|event| matches!(&event.body.data, EventData::TurnCancelled {reason_key, ..} if reason_key == "child_execution_owner_lost_before_effect")));
        assert!(!child.iter().any(|event| matches!(
            event.body.data,
            EventData::RecoveryObserved {
                disposition: RecoveryDisposition::Requeued,
                ..
            }
        )));
    }
}

#[tokio::test]
async fn legally_erased_recovered_inputs_are_cancelled_without_blocking_later_work() {
    for backend in ["sqlite", "jsonl"] {
        for erase_file in [false, true] {
            for confirm in [false, true] {
                let dir = tempfile::TempDir::new().expect("directory");
                let (port, authority) = session(dir.path(), backend);
                let mut first = input();
                let bytes =
                    PayloadRecord::new(PayloadId::new(), "session.bytes/v1".into(), vec![1, 2, 3])
                        .expect("file bytes");
                first.parts.push(EngineInputPart::File(EngineFileInput {
                    file_id: "erased-recovery-file".into(),
                    registration: serde_json::json!({"name":"erased.bin"}),
                    representation: bytes.clone(),
                    policy_evidence: serde_json::json!({"source":"fixture"}),
                }));
                let mut second = input();
                second.turn_id = TurnId::new().to_string();
                for input in [first.clone(), second.clone()] {
                    let command = port.input_command(input).await.expect("command");
                    port.owner
                        .admit_user_input(
                            &command,
                            &port.actor,
                            &port.actor,
                            port.authority.clone(),
                        )
                        .await
                        .expect("admit")
                        .detach_for_recovery();
                }
                port.owner
                    .recover_interrupted_stream(
                        port.stream.clone(),
                        "erased-recovery".into(),
                        Arc::new(ElicitationOwnerLost),
                    )
                    .await
                    .expect("recover");
                let history = port.owner.stream_history(&port.stream).expect("history");
                let input_ref = history
                    .iter()
                    .find_map(|event| match &event.body.data {
                        EventData::TurnAccepted { input_ref, .. }
                            if event.turn_id.as_ref().map(TurnId::as_str)
                                == Some(first.turn_id.as_str()) =>
                        {
                            Some(input_ref)
                        }
                        _ => None,
                    })
                    .expect("input");
                let PayloadRead::Present(content) = port
                    .owner
                    .store
                    .read_payload(&input_ref.payload_id)
                    .expect("content")
                else {
                    panic!("present")
                };
                let content: ContentPayload =
                    serde_json::from_slice(&content.body).expect("content");
                let RecordedContentPart::Text { text_ref } = &content.parts[0] else {
                    panic!("text")
                };
                let target = if erase_file {
                    StoredPayloadRef::from(&bytes)
                } else {
                    text_ref.clone()
                };
                let intent = SessionEvent::typed(
                    port.stream.clone(),
                    EventData::PayloadErasureRequested {
                        targets: vec![target],
                        reason_category: "operator_requested".into(),
                    },
                    port.actor.clone(),
                )
                .expect("intent");
                let intent_id = intent.event_id.clone();
                port.owner
                    .admit_erasure(
                        PrincipalId::shared_operator(),
                        authority.clone(),
                        AppendRequest {
                            stream_id: port.stream.clone(),
                            expected_head: port.owner.store.head(&port.stream).expect("head"),
                            idempotency_key: "erase-queued-input".into(),
                            events: vec![intent],
                            payloads: vec![],
                            erasure_reference_commitment: None,
                        },
                    )
                    .await
                    .expect("erase");
                if confirm {
                    port.owner
                        .append(AppendRequest {
                            stream_id: port.stream.clone(),
                            expected_head: port.owner.store.head(&port.stream).expect("head"),
                            idempotency_key: "confirm-erased-queued-input".into(),
                            events: vec![
                                SessionEvent::typed(
                                    port.stream.clone(),
                                    EventData::PayloadRedacted {
                                        erasure_requested_event_id: intent_id,
                                    },
                                    port.actor.clone(),
                                )
                                .expect("redacted"),
                            ],
                            payloads: vec![],
                            erasure_reference_commitment: None,
                        })
                        .expect("confirm");
                }
                let pending = port
                    .pending_inputs("fixture".into(), "fixture".into())
                    .await
                    .expect("pending survives erased input");
                assert_eq!(pending.len(), 1);
                assert_eq!(pending[0].turn_id, second.turn_id);
                port.execute(
                    pending[0].clone(),
                    Box::pin(async { Ok("remaining answer".into()) }),
                )
                .await
                .expect("remaining input executes");
                let history = port.owner.stream_history(&port.stream).expect("history");
                assert!(history.iter().any(|event| matches!(&event.body.data, EventData::TurnCancelled {reason_key, ..} if reason_key == "recovered_input_payload_erased")));
                assert_eq!(
                    history
                        .iter()
                        .filter(|event| event.body.kind() == EventKind::TurnStarted)
                        .count(),
                    1
                );
            }
        }
    }
}

#[tokio::test]
async fn dropped_admission_waiter_settles_a_later_worker_commit() {
    struct PausedAdmission {
        entered: Arc<tokio::sync::Notify>,
        release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl SessionAccessPolicy for PausedAdmission {
        fn authorize(
            &self,
            _: &PrincipalId,
            _: &SessionStreamId,
            _: SessionAccess,
        ) -> Result<(), SessionError> {
            self.entered.notify_one();
            self.release
                .lock()
                .expect("release lock")
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("release admission worker");
            Ok(())
        }
    }
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::TempDir::new().expect("directory");
        let (port, _) = session(dir.path(), backend);
        let pending = input();
        let command = port.input_command(pending.clone()).await.expect("command");
        let entered = Arc::new(tokio::sync::Notify::new());
        let (release, receiver) = std::sync::mpsc::channel();
        let authority = Arc::new(PausedAdmission {
            entered: entered.clone(),
            release: std::sync::Mutex::new(receiver),
        });
        let owner = port.owner.clone();
        let actor = port.actor.clone();
        let waiter = ::zeroclaw_spawn::spawn!(async move {
            owner
                .admit_user_input(&command, &actor, &actor, authority)
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
            .await
            .expect("admission worker running");
        waiter.abort();
        assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
        release
            .send(())
            .expect("worker may commit after waiter drops");
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while port
                .owner
                .store
                .lookup_receipt(&port.stream, &format!("turn-outcome:{}", pending.turn_id))
                .expect("outcome")
                .is_none()
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("worker result drop settles the unstarted turn");
        let history = port.owner.stream_history(&port.stream).expect("history");
        assert_eq!(
            history
                .iter()
                .filter(|event| event.body.kind() == EventKind::TurnAccepted)
                .count(),
            1
        );
        assert_eq!(
            history
                .iter()
                .filter(|event| event.body.kind() == EventKind::TurnCancelled)
                .count(),
            1
        );
        assert!(
            !history
                .iter()
                .any(|event| event.body.kind() == EventKind::TurnStarted)
        );
        assert!(history.iter().any(|event| matches!(&event.body.data, EventData::TurnCancelled {reason_key, ..} if reason_key == "turn_waiter_dropped_before_start")));
        assert_eq!(
            port.execute(input(), Box::pin(async { Ok("next input".into()) }))
                .await
                .expect("later input executes"),
            "next input"
        );
    }
}

#[tokio::test]
async fn dropping_duplicate_queued_waiter_does_not_cancel_the_original() {
    for backend in ["sqlite", "jsonl"] {
        for recovered in [false, true] {
            let dir = tempfile::TempDir::new().expect("directory");
            let (port, _) = session(dir.path(), backend);
            let input = input();
            if recovered {
                let command = port.input_command(input.clone()).await.expect("command");
                port.owner
                    .admit_user_input(&command, &port.actor, &port.actor, port.authority.clone())
                    .await
                    .expect("admit")
                    .detach_for_recovery();
                port.owner
                    .recover_interrupted_stream(
                        port.stream.clone(),
                        "duplicate-waiter-recovery".into(),
                        Arc::new(ElicitationOwnerLost),
                    )
                    .await
                    .expect("recover");
            }
            let lane = port.owner.foreground_lock(&port.stream);
            let busy = lane.lock().await;
            let original_port = port.clone();
            let original_input = input.clone();
            let original = ::zeroclaw_spawn::spawn!(async move {
                original_port
                    .execute(
                        original_input,
                        Box::pin(async { Ok("original result".into()) }),
                    )
                    .await
            });
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while Arc::strong_count(&lane) < 2 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("original owns a foreground waiter");
            let duplicate_port = port.clone();
            let duplicate_input = input.clone();
            let duplicate = ::zeroclaw_spawn::spawn!(async move {
                duplicate_port
                    .execute(
                        duplicate_input,
                        Box::pin(async { panic!("duplicate executor must never poll") }),
                    )
                    .await
            });
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while Arc::strong_count(&lane) < 3 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("duplicate owns a foreground waiter");
            duplicate.abort();
            assert!(duplicate.await.expect_err("aborted").is_cancelled());
            // Give drop settlement its own run before releasing the original.
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            assert!(
                port.owner
                    .store
                    .lookup_receipt(&port.stream, &format!("turn-outcome:{}", input.turn_id))
                    .expect("outcome")
                    .is_none()
            );
            drop(busy);
            assert_eq!(
                tokio::time::timeout(std::time::Duration::from_secs(2), original)
                    .await
                    .expect("original completes")
                    .expect("task")
                    .expect("result"),
                "original result"
            );
            let history = port.owner.stream_history(&port.stream).expect("history");
            assert!(
                !history
                    .iter()
                    .any(|event| event.body.kind() == EventKind::TurnCancelled)
            );
            assert_eq!(
                history
                    .iter()
                    .filter(|event| event.body.kind() == EventKind::TurnStarted)
                    .count(),
                1
            );
        }
    }
}
