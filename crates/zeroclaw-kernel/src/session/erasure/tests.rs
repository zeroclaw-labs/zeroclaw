use super::*;
use crate::session::query::{SessionAccess, SessionAccessPolicy};
use zeroclaw_api::principal::PrincipalId;
use zeroclaw_api::session_events::*;
use zeroclaw_infra::session_events::open_event_store;

struct Policy(bool);
impl SessionAccessPolicy for Policy {
    fn authorize(
        &self,
        _: &PrincipalId,
        _: &SessionStreamId,
        _: SessionAccess,
    ) -> Result<(), SessionError> {
        if self.0 {
            Ok(())
        } else {
            Err(SessionError::PermissionDenied("fixture denied".into()))
        }
    }
}

struct Fixture {
    directory: tempfile::TempDir,
    owner: Arc<SessionCoordinator>,
    store: Arc<dyn SessionEventStore>,
    stream: SessionStreamId,
    message: MessageId,
    target: StoredPayloadRef,
    request: AppendRequest,
}

fn fixture(backend: &str) -> Fixture {
    let directory = tempfile::tempdir().expect("directory");
    let store = open_event_store(directory.path(), backend).expect("store");
    let owner = Arc::new(SessionCoordinator::new(store.clone()));
    let stream = SessionStreamId::new();
    let branch = BranchId::new();
    let turn = TurnId::new();
    let message = MessageId::new();
    let actor = EventActor::Principal {
        principal_id: PrincipalId::shared_operator().0,
    };
    let text = PayloadRecord::new(
        PayloadId::new(),
        "session.text/v1".into(),
        b"erasable user content".to_vec(),
    )
    .expect("text");
    let target = StoredPayloadRef::from(&text);
    let content = PayloadRecord::json(
        "session.content/v1",
        &ContentPayload {
            message_id: message.clone(),
            parts: vec![RecordedContentPart::Text {
                text_ref: target.clone(),
            }],
        },
    )
    .expect("content");
    let ingress =
        PayloadRecord::json("session.evidence/v1", &serde_json::json!({})).expect("ingress");
    let mut root = SessionEvent::typed(
        stream.clone(),
        EventData::SessionStarted {
            session_id: SessionId::new(),
            owning_agent: "fixture".into(),
            capture_mode: CaptureMode::Native,
        },
        actor.clone(),
    )
    .expect("root");
    root.branch_id = Some(branch.clone());
    let mut accepted = SessionEvent::typed(
        stream.clone(),
        EventData::TurnAccepted {
            input_ref: StoredPayloadRef::from(&content),
            ingress_ref: StoredPayloadRef::from(&ingress),
            reply_route_ref: None,
        },
        actor.clone(),
    )
    .expect("accepted");
    accepted.branch_id = Some(branch.clone());
    accepted.turn_id = Some(turn.clone());
    let mut recorded = SessionEvent::typed(
        stream.clone(),
        EventData::UserContentRecorded {
            accepted_event_id: accepted.event_id.clone(),
            content_ref: StoredPayloadRef::from(&content),
        },
        actor.clone(),
    )
    .expect("message");
    recorded.branch_id = Some(branch);
    recorded.turn_id = Some(turn);
    recorded.causation_id = Some(accepted.event_id.clone());
    owner
        .append(AppendRequest {
            stream_id: stream.clone(),
            expected_head: EventCursor::beginning(stream.clone()),
            idempotency_key: "fixture".into(),
            events: vec![root, accepted, recorded],
            payloads: vec![text, content, ingress],
            erasure_reference_commitment: None,
        })
        .expect("initialize");
    let mut intent = SessionEvent::typed(
        stream.clone(),
        EventData::PayloadErasureRequested {
            targets: vec![target.clone()],
            reason_category: "operator_requested".into(),
        },
        actor,
    )
    .expect("intent");
    intent.event_id = EventId::parse("e".repeat(256)).expect("maximum-length intent identity");
    let request = AppendRequest {
        stream_id: stream.clone(),
        expected_head: store.head(&stream).expect("head"),
        idempotency_key: "erase-request".into(),
        events: vec![intent],
        payloads: vec![],
        erasure_reference_commitment: None,
    };
    Fixture {
        directory,
        owner,
        store,
        stream,
        message,
        target,
        request,
    }
}

#[tokio::test]
async fn erasure_request_completes_cleanup_and_retries_keep_the_intent_receipt() {
    for backend in ["sqlite", "jsonl"] {
        let f = fixture(backend);
        let receipt = f
            .owner
            .request_erasure(
                PrincipalId::shared_operator(),
                Arc::new(Policy(true)),
                f.request.clone(),
            )
            .await
            .expect("complete erasure");
        assert_eq!(
            receipt.event_ids,
            vec![f.request.events[0].event_id.clone()]
        );
        assert_eq!(receipt.last.sequence, f.request.expected_head.sequence + 1);
        let head = f.store.head(&f.stream).expect("completed head");
        assert_eq!(head.sequence, receipt.last.sequence + 1);
        assert!(matches!(
            f.store
                .read_payload(&f.target.payload_id)
                .expect("redacted"),
            PayloadRead::Redacted
        ));
        let retry = f
            .owner
            .request_erasure(
                PrincipalId::shared_operator(),
                Arc::new(Policy(true)),
                f.request,
            )
            .await
            .expect("same request retry");
        assert_eq!(receipt, retry);
        f.owner
            .resume_pending_erasures(f.stream.clone())
            .await
            .expect("repeat cleanup");
        assert_eq!(f.store.head(&f.stream).expect("stable head"), head);
        let messages = f
            .owner
            .query(PrincipalId::shared_operator(), Arc::new(Policy(true)))
            .messages(f.stream)
            .await
            .expect("identity survives");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message_id, f.message);
        assert_eq!(messages[0].role, "user");
        assert!(messages[0].text.is_empty());
        assert_eq!(
            messages[0].unavailable_parts[0].disposition,
            payloads::PayloadDisposition::Redacted
        );
    }
}

#[tokio::test]
async fn erasure_intent_only_reopen_resumes_once_and_unauthorized_request_never_deletes() {
    for backend in ["sqlite", "jsonl"] {
        let f = fixture(backend);
        assert!(matches!(
            f.owner
                .request_erasure(
                    PrincipalId::shared_operator(),
                    Arc::new(Policy(false)),
                    f.request.clone(),
                )
                .await,
            Err(SessionError::PermissionDenied(_))
        ));
        f.owner
            .resume_pending_erasures(f.stream.clone())
            .await
            .expect("no authorized work");
        assert_eq!(
            f.store.head(&f.stream).expect("unchanged head"),
            f.request.expected_head
        );
        assert!(matches!(
            f.store.read_payload(&f.target.payload_id).expect("present"),
            PayloadRead::Present(_)
        ));
        f.owner
            .admit_erasure(
                PrincipalId::shared_operator(),
                Arc::new(Policy(true)),
                f.request,
            )
            .await
            .expect("durable intent before process loss");
        let pending = f.store.head(&f.stream).expect("pending head");
        let Fixture {
            directory,
            owner,
            store,
            stream,
            target,
            ..
        } = f;
        drop(owner);
        drop(store);
        let store = open_event_store(directory.path(), backend).expect("reopen pending");
        let owner = Arc::new(SessionCoordinator::new(store.clone()));
        assert!(matches!(
            store.read_payload(&target.payload_id).expect("frozen"),
            PayloadRead::ErasurePending
        ));
        owner
            .resume_pending_erasures(stream.clone())
            .await
            .expect("resume");
        let completed = store.head(&stream).expect("completed head");
        assert_eq!(completed.sequence, pending.sequence + 1);
        owner
            .resume_pending_erasures(stream.clone())
            .await
            .expect("idempotent resume");
        assert_eq!(store.head(&stream).expect("stable head"), completed);
        assert!(matches!(
            store.read_payload(&target.payload_id).expect("removed"),
            PayloadRead::Redacted
        ));
    }
}

#[tokio::test]
async fn jsonl_physical_erasure_failure_is_typed_and_the_same_intent_can_resume() {
    let f = fixture("jsonl");
    let path = f
        .directory
        .path()
        .join("sessions/session-events-jsonl/payloads")
        .join(format!("{}.bin", f.target.payload_id));
    let backup = path.with_extension("blocked");
    std::fs::rename(&path, &backup).expect("preserve fixture bytes");
    std::fs::create_dir(&path).expect("block physical removal");
    let error = f
        .owner
        .request_erasure(
            PrincipalId::shared_operator(),
            Arc::new(Policy(true)),
            f.request.clone(),
        )
        .await
        .expect_err("physical deletion failure propagates");
    assert!(matches!(
        error,
        SessionError::Append(AppendError::StorageUnavailable(_))
    ));
    assert_eq!(
        f.store.head(&f.stream).expect("intent committed").sequence,
        f.request.expected_head.sequence + 1
    );
    assert!(matches!(
        f.store.read_payload(&f.target.payload_id).expect("pending"),
        PayloadRead::ErasurePending
    ));
    std::fs::remove_dir(&path).expect("remove transient obstruction");
    std::fs::rename(&backup, &path).expect("restore fixture object");
    f.owner
        .resume_pending_erasures(f.stream.clone())
        .await
        .expect("resume same authorization");
    assert!(!path.exists());
    assert!(matches!(
        f.store
            .read_payload(&f.target.payload_id)
            .expect("redacted"),
        PayloadRead::Redacted
    ));
}
