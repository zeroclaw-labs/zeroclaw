use super::*;
use crate::session::query::{PayloadSelector, SessionAccess, SessionAccessPolicy};
use zeroclaw_api::principal::PrincipalId;

impl SessionAccessPolicy for Authority {
    fn authorize(
        &self,
        _: &PrincipalId,
        _: &SessionStreamId,
        _: SessionAccess,
    ) -> Result<(), SessionError> {
        if self.0.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(SessionError::PermissionDenied("fixture live deny".into()))
        }
    }
}

fn recorded_reply(
    store: &dyn SessionEventStore,
    reply_id: &EventId,
) -> (SessionEvent, ApprovalReplyPayload) {
    let event = store
        .read_event(reply_id)
        .expect("read reply")
        .expect("reply retained");
    let EventData::ControlInputRecorded { control_ref, .. } = &event.body.data else {
        panic!("approval control input");
    };
    let PayloadRead::Present(control) = store
        .read_payload(&control_ref.payload_id)
        .expect("read reply payload")
    else {
        panic!("reply structure retained");
    };
    let reply = serde_json::from_slice(&control.body).expect("reply codec");
    (event, reply)
}

fn replacement_resolution(h: &Harness, reply: &SessionEvent) -> AppendRequest {
    let request = h
        .store
        .read_event(reply.causation_id.as_ref().expect("request cause"))
        .expect("request read")
        .expect("request");
    let EventData::ApprovalRequested { request_id, .. } = request.body.data else {
        panic!("approval request");
    };
    let evidence = PayloadRecord::json(
        "session.evidence/v1",
        &serde_json::json!({"decision":"replace","operator_reply_available":true}),
    )
    .expect("resolution evidence");
    let resolution = PayloadRecord::json(
        "session.approval-resolution/v1",
        &ApprovalResolution::Denied {
            reason_key: "operator_replaced".into(),
            verification_evidence_ref: StoredPayloadRef::from(&evidence),
        },
    )
    .expect("resolution");
    let mut event = SessionEvent::typed(
        h.stream.clone(),
        EventData::ApprovalResolved {
            request_id,
            resolution_ref: StoredPayloadRef::from(&resolution),
        },
        EventActor::default(),
    )
    .expect("resolution event");
    event.branch_id = reply.branch_id.clone();
    event.turn_id = reply.turn_id.clone();
    event.invocation_id = reply.invocation_id.clone();
    event.causation_id = Some(reply.event_id.clone());
    AppendRequest {
        stream_id: h.stream.clone(),
        expected_head: h.store.head(&h.stream).expect("head"),
        idempotency_key: format!("direct-replacement-resolution:{}", event.event_id),
        events: vec![event],
        payloads: vec![evidence, resolution],
        erasure_reference_commitment: None,
    }
}

#[tokio::test]
async fn new_replacement_reply_requires_content_but_legacy_history_is_readable_and_not_adoptable() {
    for backend in ["sqlite", "jsonl"] {
        let h = Harness::new(backend);
        let port = h.prepare(false).await;
        port.request_approval(Default::default())
            .await
            .expect("request");
        let reply_id = port
            .record_approval_reply(
                EventActor::default(),
                ChannelApprovalResponse::DenyWithEdit {
                    replacement: "complete replacement".into(),
                },
                serde_json::json!({}),
            )
            .await
            .expect("complete reply");
        let (mut old_reply, _) = recorded_reply(h.store.as_ref(), &reply_id);
        let old_control = PayloadRecord::json(
            "session.approval-reply/v1",
            &serde_json::json!({"decision":"replace"}),
        )
        .expect("old reply payload");
        let EventData::ControlInputRecorded { control_ref, .. } = &mut old_reply.body.data else {
            unreachable!();
        };
        *control_ref = StoredPayloadRef::from(&old_control);
        let mut incoming = old_reply.clone();
        incoming.event_id = EventId::new();
        incoming.sequence = 0;
        incoming.committed_at = 0;
        let error = h
            .store
            .append_batch(AppendRequest {
                stream_id: h.stream.clone(),
                expected_head: h.store.head(&h.stream).expect("head"),
                idempotency_key: "new-incomplete-replacement".into(),
                events: vec![incoming],
                payloads: vec![old_control.clone()],
                erasure_reference_commitment: None,
            })
            .expect_err("new replacement cannot omit content");
        assert!(
            matches!(error, AppendError::InvalidReference(_)),
            "{error:?}"
        );

        let mut history = h.events();
        let mut existing = std::collections::HashMap::new();
        let mut pending: Vec<_> = history
            .iter()
            .flat_map(|event| event.body.data.stored_refs().expect("event references"))
            .collect();
        while let Some(reference) = pending.pop() {
            if existing.contains_key(&reference.payload_id) {
                continue;
            }
            let PayloadRead::Present(record) = h
                .store
                .read_payload(&reference.payload_id)
                .expect("fixture payload")
            else {
                panic!("fixture content present");
            };
            let row = payloads::StoredPayload {
                reference,
                body: Some(record.body),
            };
            pending.extend(payloads::nested_refs(&row).expect("fixture graph"));
            existing.insert(row.reference.payload_id.clone(), row);
        }
        *history
            .iter_mut()
            .find(|event| event.event_id == reply_id)
            .expect("reply in history") = old_reply.clone();
        existing.insert(
            old_control.payload_id.clone(),
            payloads::StoredPayload {
                reference: StoredPayloadRef::from(&old_control),
                body: Some(old_control.body),
            },
        );
        let facts = rules::HistoryIndex::from_history(&history).expect("old history lifecycle");
        let payloads = payloads::PayloadIndex::from_index(
            &facts,
            &history,
            &existing.into_values().collect::<Vec<_>>(),
            &|_| Ok(None),
        )
        .expect("old reply reconstructs without inventing content");
        assert!(
            payloads
                .plan(&facts, &replacement_resolution(&h, &old_reply))
                .is_err(),
            "new resolution cannot adopt a legacy replacement with unknown body"
        );
    }
}

#[tokio::test]
async fn replacement_from_legacy_adapter_records_body_without_fabricating_identity() {
    for backend in ["sqlite", "jsonl"] {
        let h = Harness::new(backend);
        let port = h.prepare(false).await;
        port.request_approval(Default::default())
            .await
            .expect("request");
        port.resolve_approval(
            ChannelApprovalResponse::DenyWithEdit {
                replacement: "legacy adapter edited input".into(),
            },
            "operator_replaced",
            None,
            serde_json::json!({"decision":"replace","operator_reply_available":true}),
        )
        .await
        .expect("legacy adapter replacement captured before resolution");
        let resolution = h
            .events()
            .into_iter()
            .find(|event| event.body.kind() == EventKind::ApprovalResolved)
            .expect("resolution");
        let (event, reply) = recorded_reply(
            h.store.as_ref(),
            resolution.causation_id.as_ref().expect("original reply"),
        );
        assert!(
            matches!(event.actor, EventActor::System { component } if component == "approval_identity_unavailable")
        );
        let PayloadRead::Present(content) = h
            .store
            .read_payload(&reply.replacement_ref.expect("replacement ref").payload_id)
            .expect("content")
        else {
            panic!("legacy replacement retained");
        };
        assert_eq!(content.body, b"legacy adapter edited input");
    }
}

#[tokio::test]
async fn replacement_replies_retain_all_original_content_and_selected_identity_after_reopen() {
    for backend in ["sqlite", "jsonl"] {
        let h = Harness::new(backend);
        let port = h.prepare(false).await;
        port.request_approval(Default::default())
            .await
            .expect("request");
        let first = port
            .record_approval_reply(
                EventActor::ChannelResponder {
                    channel: "fixture.first".into(),
                    responder_id: "fixture-first-responder".into(),
                },
                ChannelApprovalResponse::DenyWithEdit {
                    replacement: "first proposed replacement".into(),
                },
                serde_json::json!({"transport":"fixture"}),
            )
            .await
            .expect("unselected reply retained");
        let actor = EventActor::ChannelResponder {
            channel: "fixture.second".into(),
            responder_id: "fixture-second-responder".into(),
        };
        let replacement = "updated request api_key=fixture-secret-value";
        let response = ChannelApprovalResponse::DenyWithEdit {
            replacement: replacement.into(),
        };
        let selected = port
            .record_approval_reply(
                actor.clone(),
                response.clone(),
                serde_json::json!({"transport":"fixture"}),
            )
            .await
            .expect("selected reply retained");
        let head = h.store.head(&h.stream).expect("head");
        let evidence = serde_json::json!({"decision":"replace","operator_reply_available":true});
        assert!(
            port.resolve_approval(
                ChannelApprovalResponse::DenyWithEdit {
                    replacement: "different replacement".into(),
                },
                "operator_replaced",
                Some(selected.clone()),
                evidence.clone(),
            )
            .await
            .is_err(),
            "same decision enum cannot adopt a different replacement body"
        );
        assert_eq!(h.store.head(&h.stream).expect("unchanged head"), head);
        port.resolve_approval(
            response.clone(),
            "operator_replaced",
            Some(selected.clone()),
            evidence.clone(),
        )
        .await
        .expect("matching original response resolves");
        let resolved_head = h.store.head(&h.stream).expect("resolved head");
        port.resolve_approval(
            response,
            "operator_replaced",
            Some(selected.clone()),
            evidence,
        )
        .await
        .expect("exact replacement resolution retry");
        assert_eq!(h.store.head(&h.stream).expect("retry head"), resolved_head);
        assert!(
            port.start().await.is_err(),
            "replacement never authorizes execution"
        );
        let resolved = h
            .events()
            .into_iter()
            .find(|event| event.body.kind() == EventKind::ApprovalResolved)
            .expect("resolution");
        assert_eq!(resolved.causation_id, Some(selected.clone()));
        let EventData::ApprovalResolved { resolution_ref, .. } = &resolved.body.data else {
            unreachable!();
        };
        let PayloadRead::Present(resolution) = h
            .store
            .read_payload(&resolution_ref.payload_id)
            .expect("resolution payload")
        else {
            panic!("resolution retained");
        };
        assert!(
            payloads::nested_refs(&payloads::StoredPayload {
                reference: StoredPayloadRef::from(&resolution),
                body: Some(resolution.body),
            })
            .expect("resolution graph")
            .iter()
            .all(|reference| reference.codec != "session.text/v1")
        );

        let stream = h.stream.clone();
        drop(port);
        let Harness {
            _dir, store, owner, ..
        } = h;
        drop(owner);
        drop(store);
        let reopened = open_event_store(_dir.path(), backend).expect("reopen");
        assert_eq!(
            reopened.head(&stream).expect("reopened head"),
            resolved_head
        );
        for (id, text, expected_actor) in [
            (first, "first proposed replacement".to_owned(), None),
            (selected, replacement.to_owned(), Some(actor)),
        ] {
            let (event, reply) = recorded_reply(reopened.as_ref(), &id);
            assert_eq!(reply.decision, ApprovalReplyDecision::Replace);
            if let Some(actor) = expected_actor {
                assert_eq!(event.actor, actor);
            }
            let reference = reply.replacement_ref.expect("replacement reference");
            let PayloadRead::Present(content) = reopened
                .read_payload(&reference.payload_id)
                .expect("replacement read")
            else {
                panic!("selected and unselected bodies retained");
            };
            assert_eq!(StoredPayloadRef::from(&content), reference);
            assert_eq!(content.body, text.as_bytes());
        }
    }
}

#[tokio::test]
async fn replacement_uses_live_payload_authorization_and_erasure_blocks_adoption_after_reopen() {
    for backend in ["sqlite", "jsonl"] {
        let h = Harness::new(backend);
        let port = h.prepare(false).await;
        port.request_approval(Default::default())
            .await
            .expect("request");
        let response = ChannelApprovalResponse::DenyWithEdit {
            replacement: "erasable replacement body".into(),
        };
        let principal = PrincipalId::shared_operator();
        let actor = EventActor::Principal {
            principal_id: principal.0.clone(),
        };
        let id = port
            .record_approval_reply(actor.clone(), response.clone(), serde_json::json!({}))
            .await
            .expect("reply");
        let (reply_event, reply) = recorded_reply(h.store.as_ref(), &id);
        let target = reply.replacement_ref.expect("replacement reference");
        let query = h.owner.query(principal.clone(), h.authority.clone());
        let selector = PayloadSelector {
            event_sequence: reply_event.sequence,
            reference_path: vec![0, 0],
        };
        assert!(matches!(
            query.read_payload(h.stream.clone(), selector.clone()).await,
            Ok(PayloadRead::Present(_))
        ));
        h.authority.0.store(false, Ordering::SeqCst);
        assert!(matches!(
            query.read_payload(h.stream.clone(), selector.clone()).await,
            Err(SessionError::PermissionDenied(_))
        ));
        h.authority.0.store(true, Ordering::SeqCst);
        let intent = SessionEvent::typed(
            h.stream.clone(),
            EventData::PayloadErasureRequested {
                targets: vec![target.clone()],
                reason_category: "operator_requested".into(),
            },
            actor.clone(),
        )
        .expect("erasure intent");
        let intent_id = intent.event_id.clone();
        h.owner
            .admit_erasure(
                principal,
                h.authority.clone(),
                AppendRequest {
                    stream_id: h.stream.clone(),
                    expected_head: h.store.head(&h.stream).expect("head"),
                    idempotency_key: "erase-approval-replacement".into(),
                    events: vec![intent],
                    payloads: vec![],
                    erasure_reference_commitment: None,
                },
            )
            .await
            .expect("authorized erasure includes approval leaf");
        let head = h.store.head(&h.stream).expect("pending erasure head");
        assert!(
            matches!(
                h.store
                    .append_batch(replacement_resolution(&h, &reply_event)),
                Err(AppendError::InvalidReference(_))
            ),
            "storage independently rejects a replacement frozen by erasure"
        );
        assert!(matches!(
            query.read_payload(h.stream.clone(), selector).await,
            Ok(PayloadRead::ErasurePending)
        ));
        assert!(
            port.resolve_approval(
                response,
                "operator_replaced",
                Some(id.clone()),
                serde_json::json!({"decision":"replace","operator_reply_available":true}),
            )
            .await
            .is_err()
        );
        assert_eq!(h.store.head(&h.stream).expect("unchanged head"), head);
        h.owner
            .append(AppendRequest {
                stream_id: h.stream.clone(),
                expected_head: head,
                idempotency_key: "confirm-approval-replacement-erasure".into(),
                events: vec![
                    SessionEvent::typed(
                        h.stream.clone(),
                        EventData::PayloadRedacted {
                            erasure_requested_event_id: intent_id,
                        },
                        actor,
                    )
                    .expect("erasure confirmation"),
                ],
                payloads: vec![],
                erasure_reference_commitment: None,
            })
            .expect("erase leaf");
        let stream = h.stream.clone();
        drop(port);
        drop(query);
        let Harness {
            _dir, store, owner, ..
        } = h;
        drop(owner);
        drop(store);
        let reopened = open_event_store(_dir.path(), backend).expect("reopen erased reply");
        assert!(matches!(
            reopened
                .read_payload(&target.payload_id)
                .expect("redacted leaf"),
            PayloadRead::Redacted
        ));
        let (_, reply) = recorded_reply(reopened.as_ref(), &id);
        assert_eq!(reply.replacement_ref, Some(target));
        assert_eq!(reply.decision, ApprovalReplyDecision::Replace);
        assert!(
            !reopened
                .read_page(&stream, &EventCursor::beginning(stream.clone()), None, 100)
                .expect("reopened history")
                .events
                .iter()
                .any(|event| event.body.kind() == EventKind::ApprovalResolved)
        );
    }
}
