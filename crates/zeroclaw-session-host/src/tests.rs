use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use zeroclaw_api::model_provider::{ChatMessage, ConversationMessage, ToolCall, ToolResultMessage};
use zeroclaw_kernel::session::query::{PayloadSelector, SessionAccess, SessionAccessPolicy};
use zeroclaw_kernel::session::{EffectIntent, PermissionResolver, SessionError};

struct Authority(AtomicBool);

#[tokio::test]
async fn ttl_expiry_reopens_same_family_with_history() {
    for backend in ["sqlite", "jsonl"] {
        let directory = tempfile::tempdir().expect("directory");
        let mut config = zeroclaw_config::schema::Config {
            data_dir: directory.path().into(),
            ..Default::default()
        };
        config.channels.session_backend = backend.into();
        let live = zeroclaw_config::live::LiveConfig::new(config);
        let hub = NativeSessionHub::new(live.handle());
        let authority = Arc::new(Authority(AtomicBool::new(true)));
        let port = hub
            .open(
                "gw_expired".into(),
                "fixture".into(),
                PrincipalId::shared_operator(),
                authority.clone(),
                ContextSnapshot {
                    messages: vec![ConversationMessage::Chat(ChatMessage::user("old history"))],
                    history_has_trim_breadcrumb: Some(false),
                },
            )
            .await
            .expect("old session");
        drop(port);
        assert_eq!(hub.archive_stale("gw_", i64::MAX).await.expect("expire"), 1);
        let resumed = hub
            .open(
                "gw_expired".into(),
                "fixture".into(),
                PrincipalId::shared_operator(),
                authority.clone(),
                ContextSnapshot {
                    messages: vec![],
                    history_has_trim_breadcrumb: Some(false),
                },
            )
            .await
            .expect("resume session");
        let restored = resumed
            .restore_context()
            .await
            .expect("context")
            .expect("history");
        assert!(
            matches!(&restored.messages[0], ConversationMessage::Chat(chat) if chat.content == "old history")
        );
        assert_eq!(
            hub.families().await.expect("one logical family"),
            vec!["gw_expired"]
        );
        drop(resumed);
        drop(hub);
        let reopened = NativeSessionHub::new(live.handle());
        let empty = ContextSnapshot {
            messages: vec![],
            history_has_trim_breadcrumb: Some(false),
        };
        let resumed = reopened
            .open(
                "gw_expired".into(),
                "fixture".into(),
                PrincipalId::shared_operator(),
                authority.clone(),
                empty.clone(),
            )
            .await
            .expect("resume after restart");
        assert!(
            matches!(&resumed.restore_context().await.expect("restored").expect("context").messages[0], ConversationMessage::Chat(chat) if chat.content == "old history")
        );
        resumed
            .close("explicit_close")
            .await
            .expect("explicit close");
        drop(resumed);
        assert!(
            reopened
                .open(
                    "gw_expired".into(),
                    "fixture".into(),
                    PrincipalId::shared_operator(),
                    authority.clone(),
                    empty.clone()
                )
                .await
                .is_err()
        );
        let ordinary = reopened
            .open(
                "gw_resume-project".into(),
                "fixture".into(),
                PrincipalId::shared_operator(),
                authority.clone(),
                empty,
            )
            .await
            .expect("client chosen id is ordinary");
        drop(ordinary);
    }
}

#[test]
fn historical_owner_resolution_uses_routing_and_preserves_ambiguity() {
    let mut config = zeroclaw_config::schema::Config::default();
    assert_eq!(legacy_session_owner(&config, None), "legacy-unattributed");
    config.agents.insert("alpha".into(), Default::default());
    assert_eq!(legacy_session_owner(&config, None), "alpha");
    config.agents.insert(
        "beta".into(),
        zeroclaw_config::schema::AliasedAgentConfig {
            channels: vec!["matrix.fixture".into()],
            ..Default::default()
        },
    );
    assert_eq!(legacy_session_owner(&config, None), "legacy-unattributed");
    assert_eq!(
        legacy_session_owner(&config, Some("matrix.fixture")),
        "beta"
    );
    config.agents.get_mut("beta").expect("beta").enabled = false;
    assert_eq!(
        legacy_session_owner(&config, Some("matrix.fixture")),
        "alpha"
    );
}

#[tokio::test]
async fn native_ready_history_queries_finish_pending_acp_migration() {
    use zeroclaw_infra::session_repository::SessionRepository;
    for backend in [NativeBackend::Sqlite, NativeBackend::Jsonl] {
        let directory = tempfile::tempdir().expect("directory");
        let source = SessionRepository::legacy_fixture(directory.path()).expect("legacy ACP");
        source
            .create_session("pending", "fixture", "/workspace", Some("owner"))
            .expect("legacy session");
        source
            .append_turn(
                "pending",
                &[ConversationMessage::Chat(ChatMessage::user("legacy input"))],
            )
            .expect("legacy history");
        drop(source);
        open_event_store(directory.path(), backend.name()).expect("earlier native store");
        // Earlier native installations marked ready while retaining ACP's DB.
        std::fs::write(
            directory.path().join("sessions/session-format.json"),
            serde_json::to_vec(&serde_json::json!({
                "version":1,"generation":1,"phase":"ready","backend":backend.name()
            }))
            .expect("earlier manifest bytes"),
        )
        .expect("earlier ready installation");
        let mut config = zeroclaw_config::schema::Config {
            data_dir: directory.path().into(),
            ..Default::default()
        };
        config.channels.session_backend = backend.name().into();
        let live = zeroclaw_config::live::LiveConfig::new(config);
        let hub = NativeSessionHub::new(live.handle());
        assert_eq!(
            hub.families().await.expect("cold history query"),
            vec!["acp_pending".to_owned()]
        );
        let descriptor = hub
            .describe("acp_pending".into())
            .await
            .expect("descriptor")
            .expect("migrated family");
        assert_eq!(descriptor.owning_agent, "fixture");
        assert_eq!(descriptor.owner.as_deref(), Some("owner"));
        assert!(!directory.path().join("sessions/acp-sessions.db").exists());
        assert!(
            directory
                .path()
                .join("sessions/acp-sessions.db.backup")
                .is_file()
        );
    }
}

#[tokio::test]
async fn stream_resolution_waits_for_installation_without_blocking_reactor() {
    let directory = tempfile::tempdir().expect("directory");
    let config = zeroclaw_config::schema::Config {
        data_dir: directory.path().into(),
        ..Default::default()
    };
    let live = zeroclaw_config::live::LiveConfig::new(config);
    let hub = NativeSessionHub::new(live.handle());
    hub.families().await.expect("ready installation");
    let sessions = directory.path().join("sessions");
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = InstallationGuard::acquire(&sessions).expect("installation lock");
        locked_tx.send(()).expect("announce lock");
        // A regression must release the lock even if the sole reactor blocks.
        let _ = release_rx.recv_timeout(std::time::Duration::from_secs(2));
    });
    locked_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("lock acquired");
    let resolution = hub.stream_id("pending");
    tokio::pin!(resolution);
    let started = std::time::Instant::now();
    tokio::select! {
        result = &mut resolution => panic!("resolution bypassed the held installation lock: {result:?}"),
        _ = tokio::time::sleep(std::time::Duration::from_millis(30)) => {}
    }
    let reactor_delay = started.elapsed();
    let _ = release_tx.send(());
    holder.join().expect("lock holder");
    assert_eq!(
        resolution.await.expect("resolved stream"),
        NativeSessionHub::stream_id("pending").expect("new identity")
    );
    assert!(
        reactor_delay < std::time::Duration::from_secs(1),
        "installation wait blocked the reactor for {reactor_delay:?}"
    );
}

#[tokio::test]
async fn waiting_for_one_family_does_not_block_another_family() {
    let directory = tempfile::tempdir().expect("directory");
    let config = zeroclaw_config::schema::Config {
        data_dir: directory.path().into(),
        ..Default::default()
    };
    let live = zeroclaw_config::live::LiveConfig::new(config);
    let hub = NativeSessionHub::new(live.handle());
    let authority = Arc::new(Authority(AtomicBool::new(true)));
    let initial = ContextSnapshot {
        messages: vec![],
        history_has_trim_breadcrumb: Some(false),
    };
    let first = hub
        .open(
            "first".into(),
            "fixture".into(),
            PrincipalId::shared_operator(),
            authority.clone(),
            initial.clone(),
        )
        .await
        .expect("first");
    let slot = hub
        .owners
        .lock()
        .await
        .get(&(directory.path().into(), "first".into()))
        .cloned()
        .expect("slot");
    let held = slot.lock().await;
    let waiting = hub.open(
        "first".into(),
        "fixture".into(),
        PrincipalId::shared_operator(),
        authority.clone(),
        initial.clone(),
    );
    tokio::pin!(waiting);
    let other = tokio::select! {
        _ = &mut waiting => panic!("same-family initialization must wait"),
        result = tokio::time::timeout(std::time::Duration::from_secs(5), hub.open("second".into(), "fixture".into(), PrincipalId::shared_operator(), authority, initial)) => result.expect("independent family is not blocked").expect("second"),
    };
    drop(held);
    let resumed = waiting.await.expect("same owner reused");
    drop((first, other, resumed));
}

#[tokio::test]
async fn upgrade_is_read_visible_and_retention_preserves_history_and_imported_activity() {
    use zeroclaw_infra::session_backend::SessionBackend;
    for backend in ["sqlite", "jsonl"] {
        let directory = tempfile::tempdir().expect("directory");
        let legacy = zeroclaw_infra::session_sqlite::SqliteSessionBackend::new(directory.path())
            .expect("legacy");
        for family in ["gw_expired", "channel_retained"] {
            legacy
                .append(family, &ChatMessage::user("historical input"))
                .expect("history");
            legacy
                .set_session_agent_alias(family, "fixture")
                .expect("alias");
        }
        let original = legacy.get_session_metadata("gw_expired").expect("metadata");
        std::thread::sleep(std::time::Duration::from_millis(5));
        let mut config = zeroclaw_config::schema::Config {
            data_dir: directory.path().into(),
            ..Default::default()
        };
        config.channels.session_backend = backend.into();
        let live = zeroclaw_config::live::LiveConfig::new(config);
        let hub = NativeSessionHub::new(live.handle());
        assert_eq!(
            hub.families().await.expect("upgrade and enumerate").len(),
            2
        );
        let authority = Arc::new(Authority(AtomicBool::new(true)));
        let query = hub
            .query(
                "gw_expired".into(),
                PrincipalId::shared_operator(),
                authority.clone(),
            )
            .await
            .expect("query")
            .expect("imported family");
        let summary = query
            .query
            .summary(query.stream.clone())
            .await
            .expect("summary");
        assert_eq!(summary.created_at, original.created_at.timestamp_millis());
        assert_eq!(
            summary.last_activity,
            original.last_activity.timestamp_millis()
        );
        assert_eq!(
            hub.archive_stale("gw_", summary.last_activity + 1)
                .await
                .expect("retention"),
            1
        );
        let successor = "gw_expired".to_owned();
        let continuation = hub
            .open(
                successor.clone(),
                "fixture".into(),
                PrincipalId::shared_operator(),
                authority.clone(),
                ContextSnapshot {
                    messages: vec![],
                    history_has_trim_breadcrumb: Some(false),
                },
            )
            .await
            .expect("open migrated TTL continuation");
        drop(continuation);
        let query = hub
            .query(
                "gw_expired".into(),
                PrincipalId::shared_operator(),
                authority.clone(),
            )
            .await
            .expect("query")
            .expect("archived family");
        assert!(
            !query
                .query
                .summary(query.stream.clone())
                .await
                .expect("TTL keeps session open")
                .closed
        );
        assert_eq!(
            query
                .query
                .message_page(query.stream, None, 20)
                .await
                .expect("preserved history")
                .messages[0]
                .text,
            "historical input"
        );
        let retained = hub
            .query(
                "channel_retained".into(),
                PrincipalId::shared_operator(),
                authority.clone(),
            )
            .await
            .expect("query")
            .expect("retained");
        assert!(
            !retained
                .query
                .summary(retained.stream)
                .await
                .expect("summary")
                .closed
        );
        let port = hub
            .open(
                "gw_active".into(),
                "fixture".into(),
                PrincipalId::shared_operator(),
                authority,
                ContextSnapshot {
                    messages: vec![],
                    history_has_trim_breadcrumb: Some(false),
                },
            )
            .await
            .expect("active owner");
        assert_eq!(
            hub.archive_stale("gw_active", i64::MAX)
                .await
                .expect("active owner skipped"),
            0
        );
        drop(port);
        assert_eq!(
            hub.archive_stale("gw_active", i64::MAX)
                .await
                .expect("idle owner released"),
            1
        );
        assert!(
            legacy
                .append("new_legacy", &ChatMessage::user("late"))
                .is_err()
        );
        assert!(!directory.path().join("sessions/sessions.db").exists());
    }
}

#[tokio::test]
async fn imported_message_identities_survive_first_context_replacement_and_reader_revocation() {
    use zeroclaw_infra::session_backend::SessionBackend;
    for backend in ["sqlite", "jsonl"] {
        let directory = tempfile::tempdir().expect("directory");
        let legacy = zeroclaw_infra::session_sqlite::SqliteSessionBackend::new(directory.path())
            .expect("legacy");
        for index in 0..8 {
            legacy
                .append("imported", &ChatMessage::user(format!("message {index}")))
                .expect("history");
        }
        legacy
            .set_session_agent_alias("imported", "fixture")
            .expect("alias");
        let mut config = zeroclaw_config::schema::Config {
            data_dir: directory.path().into(),
            ..Default::default()
        };
        config.channels.session_backend = backend.into();
        let live = zeroclaw_config::live::LiveConfig::new(config);
        let hub = NativeSessionHub::new(live.handle());
        let authority = Arc::new(Authority(AtomicBool::new(true)));
        let port = hub
            .open(
                "imported".into(),
                "fixture".into(),
                PrincipalId::shared_operator(),
                authority.clone(),
                ContextSnapshot {
                    messages: vec![],
                    history_has_trim_breadcrumb: Some(false),
                },
            )
            .await
            .expect("open imported");
        let reader = port.history_reader().expect("native reader");
        let before = reader
            .history("imported", 20)
            .await
            .expect("history")
            .expect("visible");
        let snapshot = port
            .restore_context()
            .await
            .expect("imported context")
            .expect("persisted context");
        assert_eq!(snapshot.messages.len(), 8);
        port.replace_context(snapshot, "imported_context_roundtrip")
            .await
            .expect("replace context");
        let after = reader
            .history("imported", 20)
            .await
            .expect("history")
            .expect("visible");
        assert_eq!(
            before
                .messages
                .iter()
                .map(|message| &message.message_id)
                .collect::<Vec<_>>(),
            after
                .messages
                .iter()
                .map(|message| &message.message_id)
                .collect::<Vec<_>>()
        );
        assert_eq!(reader.current().await.expect("current").message_count, 8);
        authority.0.store(false, Ordering::SeqCst);
        assert!(reader.current().await.is_err());
        assert!(reader.list(20).await.is_err());
        assert!(reader.history("imported", 20).await.is_err());
    }
}
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
            Err(SessionError::PermissionDenied("fixture_revoked".into()))
        }
    }
}
impl PermissionResolver for Authority {
    fn child_authority(
        &self,
        _: &str,
        _: &SessionStreamId,
    ) -> Result<Arc<dyn EngineSessionAuthority>, SessionError> {
        Ok(Arc::new(Authority(AtomicBool::new(
            self.0.load(Ordering::SeqCst),
        ))))
    }
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
}

#[tokio::test]
async fn drained_family_migration_preserves_derived_streams_and_receipts_in_both_directions() {
    use zeroclaw_api::session_events::capture::{
        ChildDispatchIntent, EngineInputPart, EngineTurnInput, current_provider_admission,
    };
    use zeroclaw_infra::session_events::{
        family::FamilyEventStore, migration::migrate_installation,
    };
    for (backend, target) in [
        ("sqlite", NativeBackend::Jsonl),
        ("jsonl", NativeBackend::Sqlite),
    ] {
        let directory = tempfile::tempdir().expect("directory");
        let mut config = zeroclaw_config::schema::Config {
            data_dir: directory.path().to_owned(),
            ..Default::default()
        };
        config.channels.session_backend = backend.into();
        let live = zeroclaw_config::live::LiveConfig::new(config);
        let hub = NativeSessionHub::new(live.handle());
        let authority = Arc::new(Authority(AtomicBool::new(true)));
        let port = hub
            .open(
                "child-fixture".into(),
                "fixture".into(),
                PrincipalId::shared_operator(),
                authority,
                ContextSnapshot {
                    messages: vec![],
                    history_has_trim_breadcrumb: Some(false),
                },
            )
            .await
            .expect("open");
        let input = EngineTurnInput {
            execution_workspace: None,
            turn_id: TurnId::new().to_string(),
            agent_alias: Some("fixture".into()),
            ingress: zeroclaw_api::ingress::IngressContext::agent_direct(),
            reply_route: None,
            parts: vec![EngineInputPart::Text("parent".into())],
            provider_ref: "fixture".into(),
            model: "fixture".into(),
        };
        port.execute(
            input,
            Box::pin(async {
                let child = current_provider_admission()
                    .expect("capture")
                    .begin_child(ChildDispatchIntent {
                        relation: "delegate".into(),
                        target_agent: "child".into(),
                        prompt: "child input".into(),
                        execution_mode: ChildExecutionMode::Sync,
                        task_id: Some("fixture-task".into()),
                    })
                    .await?;
                child
                    .execute(Box::pin(async { Ok("child answer".into()) }))
                    .await?;
                Ok("parent answer".into())
            }),
        )
        .await
        .expect("turn");
        drop(port);
        drop(hub);
        let stream = NativeSessionHub::stream_id("child-fixture").expect("root");
        let source = FamilyEventStore::open(directory.path(), "child-fixture").expect("source");
        let parent = source
            .read_page(&stream, &EventCursor::beginning(stream.clone()), None, 256)
            .expect("parent");
        let terminal = parent
            .events
            .iter()
            .find_map(|event| match &event.body.data {
                EventData::ChildInvocationCompleted {
                    child_terminal_cursor,
                    ..
                } => Some(child_terminal_cursor.clone()),
                _ => None,
            })
            .expect("child terminal");
        let child = source
            .read_page(
                &terminal.stream_id,
                &EventCursor::beginning(terminal.stream_id.clone()),
                None,
                256,
            )
            .expect("child");
        let root_receipt = source
            .lookup_receipt(&stream, &format!("native-family-create:{stream}"))
            .expect("receipt");
        let child_start = child.events.first().expect("derived start");
        assert_eq!(child_start.body.kind(), EventKind::DerivedSessionStarted);
        assert!(
            parent
                .events
                .iter()
                .any(|event| Some(&event.event_id) == child_start.causation_id.as_ref())
        );
        drop(source);
        migrate_installation(directory.path(), target).expect("migration");
        let migrated = FamilyEventStore::open(directory.path(), "child-fixture").expect("target");
        let after = migrated
            .read_page(&stream, &EventCursor::beginning(stream.clone()), None, 256)
            .expect("parent after");
        let child_after = migrated
            .read_page(
                &terminal.stream_id,
                &EventCursor::beginning(terminal.stream_id.clone()),
                None,
                256,
            )
            .expect("child after");
        assert_eq!(parent.raw_events, after.raw_events);
        assert_eq!(child.raw_events, child_after.raw_events);
        assert_eq!(
            root_receipt,
            migrated
                .lookup_receipt(&stream, &format!("native-family-create:{stream}"))
                .expect("target receipt")
        );
    }
}
impl EngineSessionAuthority for Authority {
    fn execution_binding(
        &self,
        _: &zeroclaw_api::session_events::capture::EngineTurnInput,
    ) -> Result<PayloadRecord, SessionError> {
        self.maintenance_binding()
    }
    fn maintenance_binding(&self) -> Result<PayloadRecord, SessionError> {
        PayloadRecord::json(
            "session.execution-binding/v1",
            &serde_json::json!({"source":"fixture"}),
        )
        .map_err(|error| AppendError::from(error).into())
    }
    fn diagnostic(&self, error: &anyhow::Error) -> Option<String> {
        Some(error.to_string())
    }
}

#[tokio::test]
async fn family_startup_resumes_root_and_derived_erasure_without_reexecuting_work() {
    use zeroclaw_api::session_events::capture::{
        ChildDispatchIntent, EngineInputPart, EngineTurnInput, current_provider_admission,
    };
    use zeroclaw_infra::session_events::family::FamilyEventStore;

    for backend in ["sqlite", "jsonl"] {
        let directory = tempfile::tempdir().expect("directory");
        let mut config = zeroclaw_config::schema::Config {
            data_dir: directory.path().to_owned(),
            ..Default::default()
        };
        config.channels.session_backend = backend.into();
        let live = zeroclaw_config::live::LiveConfig::new(config);
        let authority = Arc::new(Authority(AtomicBool::new(true)));
        let hub = NativeSessionHub::new(live.handle());
        let family = "erasure-family-fixture";
        let port = hub
            .open(
                family.into(),
                "fixture".into(),
                PrincipalId::shared_operator(),
                authority.clone(),
                ContextSnapshot {
                    messages: vec![],
                    history_has_trim_breadcrumb: Some(false),
                },
            )
            .await
            .expect("open family");
        port.execute(
            EngineTurnInput {
                execution_workspace: None,
                turn_id: TurnId::new().to_string(),
                agent_alias: Some("fixture".into()),
                ingress: zeroclaw_api::ingress::IngressContext::agent_direct(),
                reply_route: None,
                parts: vec![EngineInputPart::Text("parent".into())],
                provider_ref: "fixture".into(),
                model: "fixture".into(),
            },
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
                child
                    .execute(Box::pin(async { Ok("child answer".into()) }))
                    .await?;
                Ok("parent answer".into())
            }),
        )
        .await
        .expect("complete parent and child");
        drop(port);
        drop(hub);
        let store: Arc<dyn SessionEventStore> =
            Arc::new(FamilyEventStore::open(directory.path(), family).expect("family store"));
        let root = NativeSessionHub::stream_id(family).expect("root stream");
        let before = store
            .read_page(&root, &EventCursor::beginning(root.clone()), None, 256)
            .expect("parent history");
        let child = before
            .events
            .iter()
            .find_map(|event| match &event.body.data {
                EventData::ChildInvocationCompleted {
                    child_terminal_cursor,
                    ..
                } => Some(child_terminal_cursor.stream_id.clone()),
                _ => None,
            })
            .expect("derived stream");
        let owner = SessionCoordinator::new(store.clone());
        let mut pending = Vec::new();
        for stream in [root.clone(), child] {
            let text = PayloadRecord::new(
                PayloadId::new(),
                "session.text/v1".into(),
                b"erasable metadata".to_vec(),
            )
            .expect("text");
            let target = StoredPayloadRef::from(&text);
            let metadata = SessionEvent::typed(
                stream.clone(),
                EventData::SessionMetadataChanged {
                    field: "title".into(),
                    new_value_ref: target.clone(),
                },
                EventActor::default(),
            )
            .expect("metadata");
            let metadata_id = metadata.event_id.clone();
            owner
                .append(AppendRequest {
                    stream_id: stream.clone(),
                    expected_head: store.head(&stream).expect("head"),
                    idempotency_key: format!("erasure-fixture-metadata:{stream}"),
                    events: vec![metadata],
                    payloads: vec![text],
                    erasure_reference_commitment: None,
                })
                .expect("metadata append");
            let (_, commitment) = store
                .erasure_scope(std::slice::from_ref(&target))
                .expect("authorized fixture scope");
            let intent = SessionEvent::typed(
                stream.clone(),
                EventData::PayloadErasureRequested {
                    targets: vec![target.clone()],
                    reason_category: "operator_requested".into(),
                },
                EventActor::Principal {
                    principal_id: PrincipalId::shared_operator().0,
                },
            )
            .expect("intent");
            owner
                .append(AppendRequest {
                    stream_id: stream.clone(),
                    expected_head: store.head(&stream).expect("head"),
                    idempotency_key: format!("erasure-fixture-intent:{stream}"),
                    events: vec![intent],
                    payloads: vec![],
                    erasure_reference_commitment: Some(commitment),
                })
                .expect("durable intent before process loss");
            pending.push((
                stream.clone(),
                metadata_id,
                target,
                store.head(&stream).expect("pending head"),
            ));
        }
        drop(owner);
        let hub = NativeSessionHub::new(live.handle());
        authority.0.store(false, Ordering::SeqCst);
        assert!(
            hub.open(
                family.into(),
                "fixture".into(),
                PrincipalId::shared_operator(),
                authority.clone(),
                ContextSnapshot {
                    messages: vec![],
                    history_has_trim_breadcrumb: None
                }
            )
            .await
            .is_err()
        );
        for (stream, _, target, head) in &pending {
            assert_eq!(
                store.head(stream).expect("unauthorized unchanged head"),
                *head
            );
            assert!(matches!(
                store
                    .read_payload(&target.payload_id)
                    .expect("still pending"),
                PayloadRead::ErasurePending
            ));
        }
        authority.0.store(true, Ordering::SeqCst);
        let port = hub
            .open(
                family.into(),
                "fixture".into(),
                PrincipalId::shared_operator(),
                authority.clone(),
                ContextSnapshot {
                    messages: vec![],
                    history_has_trim_breadcrumb: None,
                },
            )
            .await
            .expect("production startup cleanup");
        for (stream, metadata_id, target, head) in pending {
            assert_eq!(
                store.head(&stream).expect("one confirmation").sequence,
                head.sequence + 1
            );
            assert!(
                store
                    .read_event(&metadata_id)
                    .expect("identity read")
                    .is_some()
            );
            assert!(matches!(
                store.read_payload(&target.payload_id).expect("cleaned"),
                PayloadRead::Redacted
            ));
            if backend == "jsonl" {
                assert!(
                    !directory
                        .path()
                        .join("sessions/session-events-jsonl/payloads")
                        .join(format!("{}.bin", target.payload_id))
                        .exists()
                );
            }
        }
        let after = store
            .read_page(&root, &EventCursor::beginning(root.clone()), None, 256)
            .expect("after cleanup");
        assert_eq!(
            before
                .events
                .iter()
                .filter(|event| event.body.kind() == EventKind::ChildInvocationRequested)
                .count(),
            after
                .events
                .iter()
                .filter(|event| event.body.kind() == EventKind::ChildInvocationRequested)
                .count()
        );
        let head = store.head(&root).expect("completed root head");
        let repeated = hub
            .open(
                family.into(),
                "fixture".into(),
                PrincipalId::shared_operator(),
                authority,
                ContextSnapshot {
                    messages: vec![],
                    history_has_trim_breadcrumb: None,
                },
            )
            .await
            .expect("repeat open");
        assert_eq!(store.head(&root).expect("stable head"), head);
        drop(repeated);
        drop(port);
    }
}

#[tokio::test]
async fn host_seed_preserves_canonical_context_and_stable_public_ids_without_reasoning_disclosure()
{
    for backend in ["sqlite", "jsonl"] {
        let directory = tempfile::TempDir::new().expect("directory");
        let mut config = zeroclaw_config::schema::Config {
            data_dir: directory.path().to_owned(),
            ..Default::default()
        };
        config.channels.session_backend = backend.into();
        let config = zeroclaw_config::live::LiveConfig::new(config);
        let hub = NativeSessionHub::new(config.handle());
        let authority = Arc::new(Authority(AtomicBool::new(true)));
        let principal = PrincipalId::shared_operator();
        let snapshot = ContextSnapshot {
            messages: vec![
                ConversationMessage::Chat(ChatMessage::system("private system")),
                ConversationMessage::Chat(ChatMessage::user("read")),
                ConversationMessage::AssistantToolCalls {
                    text: Some("checking".into()),
                    tool_calls: vec![ToolCall {
                        id: "call-1".into(),
                        name: "file_read".into(),
                        arguments: "{}".into(),
                        extra_content: Some(
                            serde_json::json!({"thought_signature":"private signature"}),
                        ),
                    }],
                    reasoning_content: Some("private reasoning".into()),
                },
                ConversationMessage::ToolResults(vec![ToolResultMessage {
                    success: None,
                    attachments: Vec::new(),
                    tool_call_id: "call-1".into(),
                    tool_name: "file_read".into(),
                    content: "bytes".into(),
                }]),
                ConversationMessage::Chat(ChatMessage::assistant("done")),
            ],
            history_has_trim_breadcrumb: Some(true),
        };
        let port = hub
            .open(
                "seed-fixture".into(),
                "fixture".into(),
                principal.clone(),
                authority.clone(),
                snapshot.clone(),
            )
            .await
            .expect("open");
        let restored = port
            .restore_context()
            .await
            .expect("restore")
            .expect("context");
        assert_eq!(
            canonical_json(&restored).expect("json"),
            canonical_json(&snapshot).expect("json")
        );
        let native = hub
            .query("seed-fixture".into(), principal.clone(), authority.clone())
            .await
            .expect("query")
            .expect("family");
        let first = native
            .query
            .messages(native.stream.clone())
            .await
            .expect("messages");
        assert_eq!(
            first
                .iter()
                .map(|message| message.text.as_str())
                .collect::<Vec<_>>(),
            vec!["read", "done"]
        );
        let last = native
            .query
            .message_page(native.stream.clone(), None, 1)
            .await
            .expect("page");
        assert_eq!(last.messages[0].message_id, first[1].message_id);
        let prior = native
            .query
            .message_page(native.stream.clone(), last.next_cursor, 1)
            .await
            .expect("prior");
        assert_eq!(prior.messages[0].message_id, first[0].message_id);
        let page = native
            .query
            .read_page(
                native.stream.clone(),
                EventCursor::beginning(native.stream.clone()),
                None,
                20,
            )
            .await
            .expect("events");
        let context = page
            .events
            .iter()
            .find(|event| matches!(event.body.data, EventData::ContextProjectionCreated { .. }))
            .expect("projection");
        assert!(
            native
                .query
                .read_payload(
                    native.stream.clone(),
                    PayloadSelector {
                        event_sequence: context.sequence,
                        reference_path: vec![0, 0]
                    }
                )
                .await
                .is_err()
        );
        drop(port);
        drop(native);
        drop(hub);
        let hub = NativeSessionHub::new(config.handle());
        let native = hub
            .query("seed-fixture".into(), principal, authority.clone())
            .await
            .expect("cold query")
            .expect("family");
        let second = native
            .query
            .messages(native.stream.clone())
            .await
            .expect("cold messages");
        assert_eq!(
            first
                .iter()
                .map(|message| &message.message_id)
                .collect::<Vec<_>>(),
            second
                .iter()
                .map(|message| &message.message_id)
                .collect::<Vec<_>>()
        );
        let store: Arc<dyn SessionEventStore> = Arc::new(
            zeroclaw_infra::session_events::family::FamilyEventStore::open(
                directory.path(),
                "seed-fixture",
            )
            .expect("store"),
        );
        let owner = Arc::new(SessionCoordinator::new(store.clone()));
        let events = native
            .query
            .read_page(
                native.stream.clone(),
                EventCursor::beginning(native.stream.clone()),
                None,
                100,
            )
            .await
            .expect("events");
        let context = events
            .events
            .iter()
            .find_map(|event| match &event.body.data {
                EventData::ContextProjectionCreated { result_ref, .. } => Some(result_ref),
                _ => None,
            })
            .expect("context");
        let PayloadRead::Present(context) = store.read_payload(&context.payload_id).expect("read")
        else {
            panic!("present")
        };
        let value: serde_json::Value = serde_json::from_slice(&context.body).expect("context");
        let target: StoredPayloadRef =
            serde_json::from_value(value["messages_ref"].clone()).expect("ref");
        let intent = SessionEvent::typed(
            native.stream.clone(),
            EventData::PayloadErasureRequested {
                targets: vec![target],
                reason_category: "operator_requested".into(),
            },
            EventActor::Principal {
                principal_id: PrincipalId::shared_operator().0,
            },
        )
        .expect("intent");
        owner
            .request_erasure(
                PrincipalId::shared_operator(),
                authority.clone(),
                AppendRequest {
                    stream_id: native.stream.clone(),
                    expected_head: store.head(&native.stream).expect("head"),
                    idempotency_key: "erase-host-seed".into(),
                    events: vec![intent],
                    payloads: vec![],
                    erasure_reference_commitment: None,
                },
            )
            .await
            .expect("erase");
        let descriptor = hub
            .describe("seed-fixture".into())
            .await
            .expect("metadata survives seed erasure")
            .expect("retained family");
        assert_eq!(descriptor.stream, native.stream);
        let catalog = zeroclaw_infra::session_events::catalog::SessionCatalog::open(
            directory.path(),
            &Default::default(),
        )
        .expect("catalog")
        .expect("native installation");
        assert_eq!(
            catalog
                .query()
                .summary(&native.stream)
                .expect("metadata and count survive erasure")
                .expect("session")
                .message_count,
            2
        );
        drop(catalog);
        let erased = native
            .query
            .messages(native.stream.clone())
            .await
            .expect("erased seed history");
        assert_eq!(erased.len(), 2);
        for (original, erased) in first.iter().zip(&erased) {
            assert_eq!(erased.message_id, original.message_id);
            assert_eq!(erased.role, original.role);
            assert!(erased.text.is_empty());
            assert_eq!(
                erased.unavailable_parts[0].disposition,
                payloads::PayloadDisposition::Redacted
            );
        }
        let old_page = native
            .query
            .message_page(native.stream.clone(), prior.next_cursor, 1)
            .await
            .expect("erased seed page");
        assert!(old_page.messages[0].text.is_empty());
        authority.0.store(false, Ordering::SeqCst);
        assert!(native.query.messages(native.stream).await.is_err());
    }
}
