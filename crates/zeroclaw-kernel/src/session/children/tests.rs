use super::*;
use crate::session::query::{SessionAccess, SessionAccessPolicy};
use zeroclaw_api::principal::PrincipalId;
use zeroclaw_api::session_events::capture::{EngineInputPart, current_provider_admission};

struct Authority;
impl SessionAccessPolicy for Authority {
    fn authorize(
        &self,
        _: &PrincipalId,
        _: &SessionStreamId,
        _: SessionAccess,
    ) -> Result<(), SessionError> {
        Ok(())
    }
}
impl PermissionResolver for Authority {
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
    ) -> Result<Arc<dyn EngineSessionAuthority>, SessionError> {
        Ok(Arc::new(Self))
    }
}
impl EngineSessionAuthority for Authority {
    fn execution_binding(&self, _: &EngineTurnInput) -> Result<PayloadRecord, SessionError> {
        self.maintenance_binding()
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

struct PausedChildStore {
    inner: Arc<dyn SessionEventStore>,
    entered: Arc<tokio::sync::Notify>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    after_commit: bool,
}
impl PausedChildStore {
    fn pause(&self) {
        self.entered.notify_one();
        self.release
            .lock()
            .expect("release lock")
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("release child initializer");
    }
}
impl SessionEventStore for PausedChildStore {
    fn append_batch(&self, request: AppendRequest) -> Result<AppendReceipt, AppendError> {
        let child = request
            .events
            .iter()
            .any(|event| event.body.kind() == EventKind::DerivedSessionStarted);
        if child && !self.after_commit {
            self.pause();
        }
        let result = self.inner.append_batch(request);
        if child && self.after_commit {
            self.pause();
        }
        result
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

fn session(store: Arc<dyn SessionEventStore>) -> EngineSession {
    let owner = Arc::new(SessionCoordinator::new(store));
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
    let context = PayloadRecord::json(
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
            result_ref: StoredPayloadRef::from(&context),
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
            payloads: vec![context],
            erasure_reference_commitment: None,
        })
        .expect("initialize");
    EngineSession::new(
        owner,
        stream,
        branch,
        PrincipalId::shared_operator(),
        Arc::new(Authority),
    )
}

#[tokio::test]
async fn dropped_child_initializer_waiter_settles_a_later_worker_result() {
    for backend in ["sqlite", "jsonl"] {
        for after_commit in [false, true] {
            let directory = tempfile::TempDir::new().expect("directory");
            let entered = Arc::new(tokio::sync::Notify::new());
            let (release, receiver) = std::sync::mpsc::channel();
            let port = session(Arc::new(PausedChildStore {
                inner: zeroclaw_infra::session_events::open_event_store(directory.path(), backend)
                    .expect("store"),
                entered: entered.clone(),
                release: std::sync::Mutex::new(receiver),
                after_commit,
            }));
            let input = EngineTurnInput {
                execution_workspace: None,
                turn_id: TurnId::new().to_string(),
                agent_alias: Some("fixture".into()),
                ingress: zeroclaw_api::ingress::IngressContext::agent_direct(),
                reply_route: None,
                parts: vec![EngineInputPart::Text("parent input".into())],
                provider_ref: "fixture".into(),
                model: "fixture".into(),
            };
            port.execute(
                input,
                Box::pin(async move {
                    let admission = current_provider_admission().expect("capture");
                    let mut prepared = Box::pin(admission.begin_child(ChildDispatchIntent {
                        relation: "skill_reflection".into(),
                        target_agent: "child".into(),
                        prompt: "prepared input".into(),
                        execution_mode: ChildExecutionMode::Background,
                        task_id: None,
                    }));
                    tokio::select! {
                        ()=entered.notified()=>{},
                        _=&mut prepared=>panic!("initializer must be held in its worker"),
                    }
                    drop(prepared);
                    Ok("parent finishes before the child initializer".into())
                }),
            )
            .await
            .expect("parent commits without waiting for a discarded initializer");
            release.send(()).expect("release initializer");
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    if port
                        .owner
                        .stream_history(&port.stream)
                        .expect("history")
                        .iter()
                        .any(|event| event.body.kind() == EventKind::InvocationCancelled)
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("discarded worker result closes its prepared child");
            let history = port.owner.stream_history(&port.stream).expect("history");
            let requested = history
                .iter()
                .find(|event| event.body.kind() == EventKind::ChildInvocationRequested)
                .expect("request");
            let terminals = history
                .iter()
                .filter(|event| {
                    event.invocation_id == requested.invocation_id
                        && zeroclaw_api::session_events::rules::terminal(&event.body.kind())
                })
                .collect::<Vec<_>>();
            assert_eq!(terminals.len(), 1);
            assert_eq!(terminals[0].body.kind(), EventKind::InvocationCancelled);
            let committed = history
                .iter()
                .find(|event| event.body.kind() == EventKind::TurnCommitted)
                .expect("parent terminal");
            assert!(committed.sequence < terminals[0].sequence);
            let child = SessionStreamId::parse(format!(
                "child-{}",
                requested.invocation_id.as_ref().expect("invocation")
            ))
            .expect("child stream");
            let child_history = port.owner.stream_history(&child).expect("child history");
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
                    | EventKind::ProviderAttemptStarted
                    | EventKind::ToolEffectStarted
            )));
        }
    }
}
