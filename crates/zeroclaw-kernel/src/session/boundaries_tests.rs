use super::query::{PayloadSelector, SessionAccess, SessionAccessPolicy, SubscriptionUpdate};
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use zeroclaw_api::principal::PrincipalId;
use zeroclaw_api::session_events::*;
use zeroclaw_infra::session_events::open_event_store;

struct Policy {
    allowed: AtomicBool,
    denied_scope: Option<SessionStreamId>,
}
impl SessionAccessPolicy for Policy {
    fn authorize(
        &self,
        _principal: &PrincipalId,
        stream: &SessionStreamId,
        _access: SessionAccess,
    ) -> Result<(), SessionError> {
        if !self.allowed.load(Ordering::SeqCst) || self.denied_scope.as_ref() == Some(stream) {
            return Err(SessionError::PermissionDenied(
                "test policy denies scope".into(),
            ));
        }
        Ok(())
    }
}
fn root(stream: &SessionStreamId) -> SessionEvent {
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
    root.branch_id = Some(BranchId::new());
    root
}
fn batch(
    store: &dyn SessionEventStore,
    stream: &SessionStreamId,
    events: Vec<SessionEvent>,
    payloads: Vec<PayloadRecord>,
) -> AppendRequest {
    AppendRequest {
        stream_id: stream.clone(),
        expected_head: store.head(stream).expect("head"),
        idempotency_key: EventId::new().to_string(),
        events,
        payloads,
        erasure_reference_commitment: None,
    }
}
fn metadata(stream: &SessionStreamId, reference: StoredPayloadRef) -> SessionEvent {
    SessionEvent::typed(
        stream.clone(),
        EventData::SessionMetadataChanged {
            field: "title".into(),
            new_value_ref: reference,
        },
        EventActor::default(),
    )
    .expect("metadata")
}

#[tokio::test]
async fn child_settlement_requires_the_bound_turn_after_reopen() {
    fn fact(
        stream: &SessionStreamId,
        branch: &BranchId,
        turn: Option<&TurnId>,
        data: EventData,
    ) -> SessionEvent {
        let mut event =
            SessionEvent::typed(stream.clone(), data, EventActor::default()).expect("event");
        event.branch_id = Some(branch.clone());
        event.turn_id = turn.cloned();
        event
    }
    fn start(
        store: &dyn SessionEventStore,
        stream: &SessionStreamId,
        branch: &BranchId,
        turn: &TurnId,
    ) {
        let record =
            PayloadRecord::json("session.evidence/v1", &serde_json::json!({})).expect("binding");
        let reference = StoredPayloadRef::from(&record);
        let accepted = fact(
            stream,
            branch,
            Some(turn),
            EventData::TurnAccepted {
                input_ref: reference.clone(),
                ingress_ref: reference.clone(),
                reply_route_ref: None,
            },
        );
        let started = fact(
            stream,
            branch,
            Some(turn),
            EventData::TurnStarted {
                accepted_event_id: accepted.event_id.clone(),
                execution_binding_ref: reference,
            },
        );
        store
            .append_batch(batch(store, stream, vec![accepted, started], vec![record]))
            .expect("start");
    }
    fn finish(
        store: &dyn SessionEventStore,
        stream: &SessionStreamId,
        branch: &BranchId,
        turn: &TurnId,
    ) -> SessionEvent {
        let record = PayloadRecord::json(
            "session.turn-completion/v1",
            &TurnCompletion::Success {
                result_event_ids: vec![],
            },
        )
        .expect("completion");
        let event = fact(
            stream,
            branch,
            Some(turn),
            EventData::TurnCommitted {
                completion_ref: StoredPayloadRef::from(&record),
            },
        );
        store
            .append_batch(batch(store, stream, vec![event.clone()], vec![record]))
            .expect("finish");
        store
            .read_event(&event.event_id)
            .expect("read")
            .expect("committed")
    }
    for backend in ["sqlite", "jsonl"] {
        for reconcile in [false, true] {
            let directory = tempfile::TempDir::new().expect("directory");
            let mut store = open_event_store(directory.path(), backend).expect("store");
            let parent = SessionStreamId::new();
            let parent_root = root(&parent);
            let branch = parent_root.branch_id.clone().expect("branch");
            store
                .append_batch(batch(&*store, &parent, vec![parent_root], vec![]))
                .expect("root");
            let parent_turn = TurnId::new();
            start(&*store, &parent, &branch, &parent_turn);
            let evidence = PayloadRecord::json("session.evidence/v1", &serde_json::json!({}))
                .expect("evidence");
            let reference = StoredPayloadRef::from(&evidence);
            let invocation = InvocationId::new();
            let mut requested = fact(
                &parent,
                &branch,
                Some(&parent_turn),
                EventData::ChildInvocationRequested {
                    relation: "delegate".into(),
                    execution_mode: ChildExecutionMode::Sync,
                    target_agent_ref: reference.clone(),
                    input_projection_ref: reference.clone(),
                    policy_evidence_ref: reference,
                    task_id: None,
                },
            );
            requested.invocation_id = Some(invocation.clone());
            store
                .append_batch(batch(
                    &*store,
                    &parent,
                    vec![requested.clone()],
                    vec![evidence],
                ))
                .expect("request");
            let child = SessionStreamId::new();
            let child_branch = BranchId::new();
            let bound_turn = TurnId::new();
            let mut derived = fact(
                &child,
                &child_branch,
                None,
                EventData::DerivedSessionStarted {
                    execution_turn_id: Some(bound_turn.clone()),
                },
            );
            derived.causation_id = Some(requested.event_id.clone());
            let mut malformed = derived.clone();
            malformed.body.data = EventData::DerivedSessionStarted {
                execution_turn_id: Some(TurnId::from("")),
            };
            assert!(
                store
                    .append_batch(batch(&*store, &child, vec![malformed], vec![]))
                    .is_err()
            );
            let legacy: EventData = serde_json::from_value(
                serde_json::json!({"kind":"derived_session_started","fields":{}}),
            )
            .expect("legacy root remains decodable");
            let mut unbound = derived.clone();
            unbound.body.data = legacy;
            assert!(matches!(
                store.append_batch(batch(&*store, &child, vec![unbound], vec![])),
                Err(AppendError::InvalidReference(_))
            ));
            store
                .append_batch(batch(&*store, &child, vec![derived], vec![]))
                .expect("derive");
            start(&*store, &child, &child_branch, &bound_turn);
            let other_turn = TurnId::new();
            start(&*store, &child, &child_branch, &other_turn);
            let wrong_terminal = finish(&*store, &child, &child_branch, &other_turn);
            let unknown = if reconcile {
                let mut event = fact(
                    &parent,
                    &branch,
                    Some(&parent_turn),
                    EventData::InvocationOutcomeUnknown {
                        last_observed_event_id: requested.event_id.clone(),
                        reason_key: "owner_lost".into(),
                        observed_partial_ref: None,
                        error_ref: None,
                    },
                );
                event.invocation_id = Some(invocation.clone());
                store
                    .append_batch(batch(&*store, &parent, vec![event.clone()], vec![]))
                    .expect("unknown");
                Some(event.event_id)
            } else {
                None
            };
            drop(store);
            store = open_event_store(directory.path(), backend).expect("reopen");
            let settlement = |terminal: &SessionEvent, wrong_resolution: bool| {
                let cursor = EventCursor {
                    stream_id: child.clone(),
                    sequence: terminal.sequence,
                };
                let (data, payloads) = if let Some(unknown_event_id) = &unknown {
                    let proof = PayloadRecord::json(
                        "session.evidence/v1",
                        &ChildTerminalEvidence {
                            child_terminal_cursor: cursor,
                            terminal_event_id: terminal.event_id.clone(),
                        },
                    )
                    .expect("proof");
                    (
                        EventData::OutcomeReconciled {
                            unknown_event_id: unknown_event_id.clone(),
                            resolution: if wrong_resolution {
                                Reconciliation::Cancelled
                            } else {
                                Reconciliation::Completed { result_ref: None }
                            },
                            verification_evidence_ref: StoredPayloadRef::from(&proof),
                        },
                        vec![proof],
                    )
                } else {
                    (
                        EventData::ChildInvocationCompleted {
                            child_terminal_cursor: cursor,
                            consumed_result_ref: None,
                        },
                        vec![],
                    )
                };
                let mut event = fact(&parent, &branch, Some(&parent_turn), data);
                event.invocation_id = Some(invocation.clone());
                event.causation_id = Some(requested.event_id.clone());
                (event, payloads)
            };
            let head = store.head(&parent).expect("head");
            let (event, payloads) = settlement(&wrong_terminal, false);
            assert!(matches!(
                store.append_batch(batch(&*store, &parent, vec![event], payloads)),
                Err(AppendError::InvalidReference(_))
            ));
            assert_eq!(store.head(&parent).expect("unchanged"), head);
            let proper_terminal = finish(&*store, &child, &child_branch, &bound_turn);
            if reconcile {
                let (event, payloads) = settlement(&proper_terminal, true);
                assert!(matches!(
                    store.append_batch(batch(&*store, &parent, vec![event], payloads)),
                    Err(AppendError::InvalidReference(_))
                ));
            }
            let (event, payloads) = settlement(&proper_terminal, false);
            store
                .append_batch(batch(&*store, &parent, vec![event], payloads))
                .expect("bound settlement");
            let owner = Arc::new(SessionCoordinator::new(store));
            let policy = Arc::new(Policy {
                allowed: AtomicBool::new(true),
                denied_scope: None,
            });
            let projection = owner
                .query(PrincipalId::shared_operator(), policy)
                .project_history(parent)
                .await
                .expect("history");
            assert_eq!(
                projection.invocations[&invocation].status,
                projection::InvocationStatus::Completed
            );
        }
    }
}

#[tokio::test]
async fn erased_middle_part_preserves_other_text_without_inventing_search_terms() {
    for backend in ["sqlite", "jsonl"] {
        let directory = tempfile::TempDir::new().expect("directory");
        let store = open_event_store(directory.path(), backend).expect("store");
        let stream = SessionStreamId::new();
        let root = root(&stream);
        let branch = root.branch_id.clone().expect("branch");
        let actor = EventActor::Principal {
            principal_id: PrincipalId::shared_operator().0,
        };
        let control =
            PayloadRecord::json("session.control/v1", &serde_json::json!({})).expect("control");
        let ingress =
            PayloadRecord::json("session.ingress/v1", &serde_json::json!({})).expect("ingress");
        let mut admitted = SessionEvent::typed(
            stream.clone(),
            EventData::ControlInputRecorded {
                control_ref: StoredPayloadRef::from(&control),
                expected_target_ref: TargetRef::Branch {
                    branch_id: branch.clone(),
                },
                ingress_ref: StoredPayloadRef::from(&ingress),
            },
            actor.clone(),
        )
        .expect("admitted");
        admitted.branch_id = Some(branch.clone());
        let mut leaves = ["left", " private ", "right"]
            .into_iter()
            .map(|text| {
                PayloadRecord::new(
                    PayloadId::new(),
                    "session.text/v1".into(),
                    text.as_bytes().to_vec(),
                )
                .expect("leaf")
            })
            .collect::<Vec<_>>();
        let target = StoredPayloadRef::from(&leaves[1]);
        let id = MessageId::new();
        let content = PayloadRecord::json(
            "session.content/v1",
            &ContentPayload {
                message_id: id.clone(),
                parts: leaves
                    .iter()
                    .map(|record| RecordedContentPart::Text {
                        text_ref: StoredPayloadRef::from(record),
                    })
                    .collect(),
            },
        )
        .expect("content");
        let mut message = SessionEvent::typed(
            stream.clone(),
            EventData::AssistantContentRecorded {
                content_ref: StoredPayloadRef::from(&content),
                provenance_refs: vec![],
            },
            actor.clone(),
        )
        .expect("message");
        message.branch_id = Some(branch);
        message.causation_id = Some(admitted.event_id.clone());
        leaves.extend([control, ingress, content]);
        store
            .append_batch(batch(
                &*store,
                &stream,
                vec![root, admitted, message],
                leaves,
            ))
            .expect("append");
        let owner = Arc::new(SessionCoordinator::new(store.clone()).with_search(Arc::new(
            zeroclaw_infra::session_events::search::FullTextSearch,
        )));
        let policy = Arc::new(Policy {
            allowed: AtomicBool::new(true),
            denied_scope: None,
        });
        let query = owner.query(PrincipalId::shared_operator(), policy.clone());
        let intent = SessionEvent::typed(
            stream.clone(),
            EventData::PayloadErasureRequested {
                targets: vec![target],
                reason_category: "operator_requested".into(),
            },
            actor,
        )
        .expect("intent");
        owner
            .admit_erasure(
                PrincipalId::shared_operator(),
                policy,
                batch(&*store, &stream, vec![intent], vec![]),
            )
            .await
            .expect("erase");
        let messages = query.messages(stream.clone()).await.expect("history");
        assert_eq!(messages[0].message_id, id);
        assert_eq!(messages[0].text, "left\nright");
        assert_eq!(messages[0].unavailable_parts[0].part_index, 1);
        assert!(
            query
                .search_messages(stream.clone(), "leftright", 10)
                .await
                .expect("search")
                .is_empty()
        );
        assert_eq!(
            query
                .search_messages(stream, "right", 10)
                .await
                .expect("remaining search")[0]
                .message_id,
            id
        );
    }
}

#[tokio::test]
async fn read_authorization_is_current_and_payload_paths_are_bounded() {
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::TempDir::new().expect("dir");
        let store = open_event_store(dir.path(), backend).expect("store");
        let stream = SessionStreamId::new();
        let title = PayloadRecord::new(
            PayloadId::new(),
            "session.text/v1".into(),
            b"private title".to_vec(),
        )
        .expect("title");
        store
            .append_batch(batch(
                &*store,
                &stream,
                vec![
                    root(&stream),
                    metadata(&stream, StoredPayloadRef::from(&title)),
                ],
                vec![title.clone()],
            ))
            .expect("append");
        let coordinator = Arc::new(SessionCoordinator::new(store));
        let policy = Arc::new(Policy {
            allowed: AtomicBool::new(true),
            denied_scope: None,
        });
        let query = coordinator.query(PrincipalId::shared_operator(), policy.clone());
        let selector = PayloadSelector {
            event_sequence: 2,
            reference_path: vec![0],
        };
        assert!(matches!(
            query
                .read_payload(stream.clone(), selector.clone(), 3)
                .await,
            Err(SessionError::Read(ReadError::TooLarge))
        ));
        assert_eq!(
            query
                .read_payload(stream.clone(), selector.clone(), 128)
                .await
                .expect("read"),
            PayloadRead::Present(title)
        );
        assert!(matches!(
            query
                .read_payload(
                    stream.clone(),
                    PayloadSelector {
                        event_sequence: 2,
                        reference_path: vec![1]
                    },
                    128
                )
                .await,
            Err(SessionError::Read(ReadError::AccessDenied(_)))
        ));
        policy.allowed.store(false, Ordering::SeqCst);
        assert!(matches!(
            query.read_payload(stream.clone(), selector, 128).await,
            Err(SessionError::PermissionDenied(_))
        ));
        assert!(matches!(
            query
                .read_page(
                    stream.clone(),
                    EventCursor::beginning(stream),
                    None,
                    10,
                    4096
                )
                .await,
            Err(SessionError::PermissionDenied(_))
        ));
    }
}

#[tokio::test]
async fn shared_leaf_erasure_requires_every_scope_and_freezes_indirect_reads() {
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::TempDir::new().expect("dir");
        let store = open_event_store(dir.path(), backend).expect("store");
        let a = SessionStreamId::new();
        let b = SessionStreamId::new();
        let leaf = PayloadRecord::new(
            PayloadId::new(),
            "session.text/v1".into(),
            b"shared private content".to_vec(),
        )
        .expect("leaf");
        let leaf_ref = StoredPayloadRef::from(&leaf);
        let ancestor = PayloadRecord::json(
            "session.evidence/v1",
            &serde_json::json!({"content_ref":leaf_ref}),
        )
        .expect("ancestor");
        let ancestor_ref = StoredPayloadRef::from(&ancestor);
        store
            .append_batch(batch(
                &*store,
                &a,
                vec![root(&a), metadata(&a, ancestor_ref.clone())],
                vec![leaf, ancestor],
            ))
            .expect("a");
        store
            .append_batch(batch(
                &*store,
                &b,
                vec![root(&b), metadata(&b, leaf_ref.clone())],
                vec![],
            ))
            .expect("b");
        let intent = SessionEvent::typed(
            a.clone(),
            EventData::PayloadErasureRequested {
                targets: vec![leaf_ref],
                reason_category: "operator_requested".into(),
            },
            EventActor::Principal {
                principal_id: PrincipalId::SHARED_OPERATOR.into(),
            },
        )
        .expect("intent");
        let request = batch(&*store, &a, vec![intent], vec![]);
        let coordinator = Arc::new(SessionCoordinator::new(Arc::clone(&store)));
        let deny = Arc::new(Policy {
            allowed: AtomicBool::new(true),
            denied_scope: Some(b),
        });
        assert!(matches!(
            coordinator
                .admit_erasure(PrincipalId::shared_operator(), deny, request.clone())
                .await,
            Err(SessionError::PermissionDenied(_))
        ));
        assert_eq!(store.head(&a).expect("unchanged").sequence, 2);
        let allow = Arc::new(Policy {
            allowed: AtomicBool::new(true),
            denied_scope: None,
        });
        coordinator
            .admit_erasure(PrincipalId::shared_operator(), allow.clone(), request)
            .await
            .expect("authorized erase");
        let query = coordinator.query(PrincipalId::shared_operator(), allow);
        assert_eq!(
            query
                .read_payload(
                    a.clone(),
                    PayloadSelector {
                        event_sequence: 2,
                        reference_path: vec![0, 0]
                    },
                    4096
                )
                .await
                .expect("pending"),
            PayloadRead::ErasurePending
        );
        assert!(matches!(
            store.append_batch(batch(&*store, &a, vec![metadata(&a, ancestor_ref)], vec![])),
            Err(AppendError::InvalidReference(_))
        ));
    }
}

#[tokio::test]
async fn subscription_uses_history_and_requires_acknowledgement() {
    let dir = tempfile::TempDir::new().expect("dir");
    let store = open_event_store(dir.path(), "sqlite").expect("store");
    let stream = SessionStreamId::new();
    let coordinator = Arc::new(SessionCoordinator::new(Arc::clone(&store)));
    let policy = Arc::new(Policy {
        allowed: AtomicBool::new(true),
        denied_scope: None,
    });
    let mut subscription = coordinator
        .query(PrincipalId::shared_operator(), policy.clone())
        .subscribe(EventCursor::beginning(stream.clone()));
    assert!(
        subscription
            .acknowledge(EventCursor {
                stream_id: stream.clone(),
                sequence: 1
            })
            .is_err()
    );
    coordinator
        .append_async(batch(&*store, &stream, vec![root(&stream)], vec![]))
        .await
        .expect("publish after subscribe");
    let SubscriptionUpdate::Page(first) = subscription.next(10, 4096).await.expect("first") else {
        panic!("expected page")
    };
    let SubscriptionUpdate::Page(repeated) = subscription
        .next(10, 4096)
        .await
        .expect("repeat before ack")
    else {
        panic!("expected page")
    };
    assert_eq!(first, repeated);
    assert!(
        subscription
            .acknowledge(EventCursor {
                stream_id: stream.clone(),
                sequence: 2
            })
            .is_err()
    );
    subscription.acknowledge(first.through).expect("ack");
    policy.allowed.store(false, Ordering::SeqCst);
    assert!(matches!(
        subscription.next(10, 4096).await,
        Err(SessionError::PermissionDenied(_))
    ));
}

#[tokio::test]
async fn ordinary_history_permission_does_not_disclose_reasoning_leaves() {
    for backend in ["sqlite", "jsonl"] {
        let dir = tempfile::tempdir().expect("directory");
        let store = open_event_store(dir.path(), backend).expect("store");
        let stream = SessionStreamId::new();
        let leaf = PayloadRecord::new(
            PayloadId::new(),
            "session.text/v1".into(),
            b"private thought".to_vec(),
        )
        .expect("leaf");
        let response = PayloadRecord::json(
            "session.response/v1",
            &ProviderResponsePayload {
                blocks: vec![ResponseBlock::Reasoning {
                    representation_kind: "text".into(),
                    payload_ref: StoredPayloadRef::from(&leaf),
                    completion: ContentCompletion::Complete,
                    replay_requirement: "none".into(),
                }],
                completion: ContentCompletion::Complete,
                finish_evidence_ref: None,
                usage_ref: None,
                reasoning_absence: None,
            },
        )
        .expect("response");
        let event = metadata(&stream, StoredPayloadRef::from(&response));
        let owner = Arc::new(SessionCoordinator::new(store.clone()));
        owner
            .append(batch(
                store.as_ref(),
                &stream,
                vec![root(&stream), event],
                vec![leaf, response],
            ))
            .expect("append");
        let query = owner.query(
            PrincipalId::shared_operator(),
            Arc::new(Policy {
                allowed: AtomicBool::new(true),
                denied_scope: None,
            }),
        );
        assert!(
            query
                .read_payload(
                    stream.clone(),
                    PayloadSelector {
                        event_sequence: 2,
                        reference_path: vec![0]
                    },
                    4096
                )
                .await
                .is_ok()
        );
        assert!(matches!(
            query
                .read_payload(
                    stream.clone(),
                    PayloadSelector {
                        event_sequence: 2,
                        reference_path: vec![0, 0]
                    },
                    4096
                )
                .await,
            Err(SessionError::PermissionDenied(_))
        ));
    }
}

struct FutureResponseStore {
    event: SessionEvent,
    response: PayloadRecord,
    text: PayloadRecord,
}
impl SessionEventStore for FutureResponseStore {
    fn append_batch(&self, _: AppendRequest) -> Result<AppendReceipt, AppendError> {
        Err(AppendError::WriterFenced("read-only fixture".into()))
    }
    fn read_event(&self, id: &EventId) -> Result<Option<SessionEvent>, ReadError> {
        Ok((id == &self.event.event_id).then(|| self.event.clone()))
    }
    fn read_page(
        &self,
        stream: &SessionStreamId,
        after: &EventCursor,
        through: Option<&EventCursor>,
        _: usize,
        _: usize,
    ) -> Result<EventPage, ReadError> {
        Ok(EventPage {
            stream_id: stream.clone(),
            after: after.clone(),
            through: through.cloned().unwrap_or(self.head(stream)?),
            events: vec![self.event.clone()],
            raw_events: vec![self.event.canonical_bytes().expect("fixture event")],
            has_more: false,
        })
    }
    fn head(&self, stream: &SessionStreamId) -> Result<EventCursor, ReadError> {
        Ok(EventCursor {
            stream_id: stream.clone(),
            sequence: self.event.sequence,
        })
    }
    fn lookup_receipt(
        &self,
        _: &SessionStreamId,
        _: &str,
    ) -> Result<Option<AppendReceipt>, ReadError> {
        Ok(None)
    }
    fn read_payload_bounded(&self, id: &PayloadId, limit: usize) -> Result<PayloadRead, ReadError> {
        let record = [&self.response, &self.text]
            .into_iter()
            .find(|record| &record.payload_id == id)
            .ok_or_else(|| ReadError::PayloadMissing(id.to_string()))?;
        if record.body.len() > limit {
            return Err(ReadError::TooLarge);
        }
        Ok(PayloadRead::Present(record.clone()))
    }
    fn erasure_scope(
        &self,
        _: &[StoredPayloadRef],
    ) -> Result<(Vec<SessionStreamId>, Digest), AppendError> {
        Err(AppendError::UnresolvedReachability(
            "future response fixture".into(),
        ))
    }
}

#[tokio::test]
async fn future_response_blocks_do_not_hide_later_text_or_grant_payload_access() {
    let stream = SessionStreamId::new();
    let text = PayloadRecord::new(
        PayloadId::new(),
        "session.text/v1".into(),
        b"after".to_vec(),
    )
    .expect("text");
    let reference = StoredPayloadRef::from(&text);
    let response = PayloadRecord::json(
        "session.response/v1",
        &serde_json::json!({
            "blocks":[
                {"kind":"future_audio", "raw_ref":reference},
                {"kind":"text", "text_ref":reference}
            ],
            "completion":"complete"
        }),
    )
    .expect("response");
    let mut event = metadata(&stream, StoredPayloadRef::from(&response));
    event.sequence = 2;
    let store = Arc::new(FutureResponseStore {
        event,
        response,
        text,
    });
    let owner = Arc::new(SessionCoordinator::new(store));
    let query = owner.query(
        PrincipalId::shared_operator(),
        Arc::new(Policy {
            allowed: AtomicBool::new(true),
            denied_scope: None,
        }),
    );
    let read = |path| PayloadSelector {
        event_sequence: 2,
        reference_path: path,
    };
    let PayloadRead::Present(text) = query
        .read_payload(stream.clone(), read(vec![0, 1]), 4096)
        .await
        .expect("known block after unknown")
    else {
        panic!("text present")
    };
    assert_eq!(text.body, b"after");
    assert!(matches!(
        query
            .read_payload(stream.clone(), read(vec![0, 0]), 4096)
            .await,
        Err(SessionError::Read(ReadError::AccessDenied(_)))
    ));
    assert!(matches!(
        query.read_payload(stream, read(vec![0]), 4096).await,
        Err(SessionError::PermissionDenied(_))
    ));
}
