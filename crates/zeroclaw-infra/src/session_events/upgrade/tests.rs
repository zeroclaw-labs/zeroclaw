use super::*;

#[test]
fn sqlite_publication_supports_fresh_reopen_and_legacy_upgrade() {
    use crate::session_backend::SessionBackend;
    for legacy in [false, true] {
        let directory = tempfile::tempdir().expect("directory");
        if legacy {
            let old = crate::session_events::upgrade::legacy_sqlite::SqliteSessionBackend::new(
                directory.path(),
            )
            .expect("legacy");
            old.append(
                "gw_legacy",
                &zeroclaw_api::model_provider::ChatMessage::user("retained input"),
            )
            .expect("append");
        }
        upgrade_installation(directory.path(), NativeBackend::Sqlite, |_| {
            "fixture".into()
        })
        .expect("publish");
        upgrade_installation(directory.path(), NativeBackend::Sqlite, |_| {
            "fixture".into()
        })
        .expect("reopen");
        let guard =
            InstallationGuard::acquire(&directory.path().join("sessions")).expect("manifest");
        assert_eq!(guard.active().expect("ready").0, NativeBackend::Sqlite);
        if legacy {
            assert!(
                directory
                    .path()
                    .join("sessions/sessions.db.backup")
                    .is_file()
            );
            assert_eq!(
                installation::session_ids(directory.path(), NativeBackend::Sqlite)
                    .expect("families")
                    .len(),
                1
            );
        }
    }
}
use crate::{
    session_events::upgrade::legacy_sqlite::SqliteSessionBackend, session_store::SessionStore,
};
use zeroclaw_api::{
    model_provider::ChatMessage,
    session_events::{EventData, StoredPayloadRef},
};

#[test]
fn sqlite_message_timestamps_survive_upgrade_to_each_native_backend() {
    for target in [NativeBackend::Sqlite, NativeBackend::Jsonl] {
        let directory = tempfile::tempdir().expect("directory");
        let source = SqliteSessionBackend::new(directory.path()).expect("source");
        source
            .append("gw_clocks", &ChatMessage::user("first"))
            .expect("first");
        source
            .append("gw_clocks", &ChatMessage::assistant("second"))
            .expect("second");
        let conn =
            rusqlite::Connection::open(directory.path().join("sessions/sessions.db")).expect("db");
        conn.execute("UPDATE sessions SET created_at=CASE role WHEN 'user' THEN '2020-01-02T03:04:05Z' ELSE '2021-02-03T04:05:06Z' END", []).expect("clocks");
        drop(conn);
        drop(source);
        upgrade_installation(directory.path(), target, |_| "fixture".into()).expect("upgrade");
        let store = open_event_store(directory.path(), target.name()).expect("store");
        let stream = session_id("gw_clocks").expect("stream");
        let page = store
            .read_page(&stream, &EventCursor::beginning(stream.clone()), None, 100)
            .expect("events");
        let times = page
            .events
            .iter()
            .filter_map(|event| match &event.body.data {
                EventData::ContextProjectionCreated {
                    reason_key,
                    result_ref,
                    ..
                } if reason_key == "legacy_import" => {
                    let PayloadRead::Present(payload) =
                        store.read_payload(&result_ref.payload_id).expect("payload")
                    else {
                        panic!("context missing")
                    };
                    let value: serde_json::Value =
                        serde_json::from_slice(&payload.body).expect("context");
                    Some(
                        value["legacy_messages"]
                            .as_array()
                            .expect("messages")
                            .iter()
                            .map(|entry| entry["created_at"].as_i64().expect("time"))
                            .collect::<Vec<_>>(),
                    )
                }
                _ => None,
            })
            .flatten()
            .collect::<Vec<_>>();
        assert_eq!(times, vec![1_577_934_245_000, 1_612_325_106_000]);
    }
}

#[test]
fn legacy_tool_metadata_survives_storage_upgrade_and_context_reconstruction() {
    use zeroclaw_api::model_provider::ConversationMessage;
    use zeroclaw_api::session_events::context_read;
    for sqlite in [false, true] {
        for target in [NativeBackend::Sqlite, NativeBackend::Jsonl] {
            let directory = tempfile::tempdir().unwrap();
            let source = old_source(directory.path(), sqlite, 1);
            let reasoning = r#"{"thinking":"literal [IMAGE:/reason]","signature":"opaque"}"#;
            let arguments = r#"{"text":"[IMAGE:/argument]"}"#;
            let output = r#"{"content":"literal tool JSON","tool_call_id":"not-identity"}"#;
            source
                .append(
                    "gw_fixture",
                    &ChatMessage::assistant(
                        serde_json::json!({
                            "content":"checking",
                            "tool_calls":[{"id":"call-1","name":"shell","arguments":arguments}],
                            "reasoning_content":reasoning,
                        })
                        .to_string(),
                    ),
                )
                .unwrap();
            source
                .append(
                    "gw_fixture",
                    &ChatMessage {
                        role: "tool".into(),
                        content: serde_json::json!({
                            "tool_call_id":"call-1","name":"shell","success":false,"content":output,
                        })
                        .to_string(),
                        attachments: Vec::new(),
                        tool_calls: Vec::new(),
                        tool_result: None,
                        reasoning_content: None,
                    },
                )
                .unwrap();
            drop(source);
            upgrade_installation(directory.path(), target, |_| "fixture".into()).unwrap();
            let store = open_event_store(directory.path(), target.name()).unwrap();
            let stream = session_id("gw_fixture").unwrap();
            let page = store
                .read_page(&stream, &EventCursor::beginning(stream.clone()), None, 100)
                .unwrap();
            let projection = page.events.iter().find(|event| matches!(
                &event.body.data,
                EventData::ContextProjectionCreated { reason_key, .. } if reason_key == "legacy_import"
            )).unwrap();
            let messages: Vec<_> = context_read::context_refs(store.as_ref(), projection)
                .unwrap()
                .iter()
                .filter_map(|reference| {
                    context_read::resolve_message(store.as_ref(), &stream, reference).unwrap()
                })
                .collect();
            assert_eq!(messages.len(), 3);
            let ConversationMessage::AssistantToolCalls {
                text,
                tool_calls,
                reasoning_content,
            } = &messages[1]
            else {
                panic!("typed assistant metadata missing");
            };
            assert_eq!(text.as_deref(), Some("checking"));
            assert_eq!(tool_calls[0].arguments, arguments);
            assert_eq!(reasoning_content.as_deref(), Some(reasoning));
            let ConversationMessage::ToolResults(results) = &messages[2] else {
                panic!("typed tool identity missing");
            };
            assert_eq!(results[0].tool_call_id, "call-1");
            assert_eq!(results[0].content, output);
            assert_eq!(results[0].success, Some(false));
        }
    }
}

fn old_source(directory: &Path, sqlite: bool, count: usize) -> Box<dyn SessionBackend> {
    let source: Box<dyn SessionBackend> = if sqlite {
        Box::new(SqliteSessionBackend::new(directory).expect("SQLite source"))
    } else {
        Box::new(SessionStore::new(directory).expect("JSONL source"))
    };
    let messages = (0..count)
        .map(|index| ChatMessage::user(format!("legacy message {index}")))
        .collect::<Vec<_>>();
    source
        .rewrite_messages("gw_fixture", &messages)
        .expect("source history");
    source
        .set_session_trim_breadcrumb("gw_fixture", true)
        .expect("trim provenance");
    if sqlite {
        source
            .set_session_agent_alias("gw_fixture", "fixture")
            .expect("attribution");
    }
    source
}

#[test]
fn acp_sessions_migrate_metadata_typed_history_context_and_checkpoints_to_standard_store() {
    use crate::session_repository::{SessionKillTransition, SessionRepository, SessionRestore};
    use zeroclaw_api::model_provider::{ConversationMessage, ToolCall, ToolResultMessage};
    use zeroclaw_api::session_events::context_read;
    for target in [NativeBackend::Sqlite, NativeBackend::Jsonl] {
        let directory = tempfile::tempdir().unwrap();
        let old = SessionRepository::legacy_fixture(directory.path()).unwrap();
        old.create_session_with_interaction_surface(
            "live",
            "acp",
            "fixture",
            "/workspace",
            Some("cli"),
            Some("owner"),
        )
        .unwrap();
        let mut history: Vec<_> = (0..130)
            .map(|index| ConversationMessage::Chat(ChatMessage::user(format!("message {index}"))))
            .collect();
        history.push(ConversationMessage::AssistantToolCalls {
            text: Some("call".into()),
            tool_calls: vec![ToolCall {
                id: "call-1".into(),
                name: "read".into(),
                arguments: r#"{"token":"fixture-secret"}"#.into(),
                extra_content: None,
            }],
            reasoning_content: Some("retained reasoning token=fixture-secret".into()),
        });
        history.push(ConversationMessage::ToolResults(vec![ToolResultMessage {
            success: None,
            attachments: Vec::new(),
            tool_call_id: "call-1".into(),
            content: "result api_key=fixture-secret".into(),
            tool_name: "read".into(),
        }]));
        old.append_turn("live", &history).unwrap();
        let retained = history[128..].to_vec();
        old.persist_retained_context_seed("live", &retained, true)
            .unwrap();
        old.set_latest_context_input_tokens("live", 123).unwrap();
        let plan: Vec<zeroclaw_api::plan::PlanEntry> =
            serde_json::from_value(serde_json::json!([{"content":"audit","status":"in_progress"}]))
                .unwrap();
        old.set_plan("live", &plan).unwrap();
        old.append_event(
            "live",
            zeroclaw_log::Action::Start,
            zeroclaw_log::EventOutcome::Success,
            Some("protocol event"),
        )
        .unwrap();
        let pending = ConversationMessage::Chat(ChatMessage::assistant("partial checkpoint"));
        old.begin_turn_checkpoint("live", "interrupted-turn", std::slice::from_ref(&pending))
            .unwrap();
        let before = old.load_session("live").unwrap().unwrap();
        old.create_session("closed", "acp", "fixture", "/closed", None)
            .unwrap();
        old.mark_session_killed("closed").unwrap();
        upgrade_installation(directory.path(), target, |_| "fixture".into()).unwrap();
        let sessions = directory.path().join("sessions");
        assert!(!sessions.join("acp-sessions.db").exists());
        assert!(sessions.join("acp-sessions.db.backup").is_file());
        let backup = fs::read(sessions.join("acp-sessions.db.backup")).unwrap();
        let current = SessionRepository::new(directory.path()).unwrap();
        let after = current.load_session("live").unwrap().unwrap();
        assert_eq!(after.principal_id, before.principal_id);
        assert_eq!(after.workspace_dir, "/workspace");
        assert_eq!(after.interaction_surface.as_deref(), Some("cli"));
        assert_eq!(after.latest_context_input_tokens, 123);
        assert_eq!(
            after.created_at.timestamp_millis(),
            before.created_at.timestamp_millis()
        );
        assert_eq!(
            after.last_activity.timestamp_millis(),
            before.last_activity.timestamp_millis()
        );
        assert_eq!(
            serde_json::to_value(after.messages).unwrap(),
            serde_json::to_value(before.retained_context.as_ref().unwrap()).unwrap()
        );
        assert_eq!(
            serde_json::to_value(current.get_plan("live").unwrap()).unwrap(),
            serde_json::to_value(plan).unwrap()
        );
        assert!(matches!(
            current.load_session_for_restore("closed").unwrap(),
            SessionRestore::Killed
        ));
        assert_eq!(current.list_sessions().unwrap().len(), 1);
        let stream = session_id("live").unwrap();
        let store = open_event_store(directory.path(), target.name()).unwrap();
        let root = store
            .read_page(&stream, &EventCursor::beginning(stream.clone()), None, 100)
            .unwrap();
        let mut recovered = Vec::new();
        let mut checkpoint_found = false;
        for event in &root.events {
            match &event.body.data {
                EventData::ContextProjectionCreated {
                    reason_key,
                    result_ref,
                    ..
                } if reason_key == "host_seed" => {
                    let PayloadRead::Present(body) =
                        store.read_payload(&result_ref.payload_id).unwrap()
                    else {
                        panic!("context");
                    };
                    let body: serde_json::Value = serde_json::from_slice(&body.body).unwrap();
                    let current: StoredPayloadRef =
                        serde_json::from_value(body["messages_ref"].clone()).unwrap();
                    for item in context_read::context_refs(store.as_ref(), event).unwrap() {
                        if matches!(&item.source, zeroclaw_api::session_events::ContextMessageSource::Conversation {payload_ref,..} if payload_ref == &current)
                        {
                            recovered.push(
                                context_read::resolve_message(store.as_ref(), &stream, &item)
                                    .unwrap()
                                    .unwrap(),
                            );
                        }
                    }
                }
                EventData::LegacyHistoryImported {
                    source_kind,
                    import_receipt_ref,
                    ..
                } if source_kind == "legacy-acp-checkpoint" => {
                    let PayloadRead::Present(body) =
                        store.read_payload(&import_receipt_ref.payload_id).unwrap()
                    else {
                        panic!("checkpoint");
                    };
                    let body: serde_json::Value = serde_json::from_slice(&body.body).unwrap();
                    assert_eq!(body["turn_id"], "interrupted-turn");
                    let refs: Vec<zeroclaw_api::session_events::MessageRef> =
                        serde_json::from_value(body["message_refs"].clone()).unwrap();
                    let recovered =
                        context_read::resolve_message(store.as_ref(), &stream, &refs[0])
                            .unwrap()
                            .unwrap();
                    assert_eq!(
                        serde_json::to_value(recovered).unwrap(),
                        serde_json::to_value(&pending).unwrap()
                    );
                    checkpoint_found = true;
                }
                _ => {}
            }
        }
        assert!(checkpoint_found);
        assert_eq!(
            serde_json::to_value(recovered).unwrap(),
            serde_json::to_value(&history).unwrap()
        );
        let head = store.head(&stream).unwrap();
        upgrade_installation(directory.path(), target, |_| "fixture".into()).unwrap();
        assert_eq!(store.head(&stream).unwrap(), head);
        old.touch_session("live").unwrap();
        old.set_latest_context_input_tokens("live", 456).unwrap();
        assert_eq!(
            current
                .load_session("live")
                .unwrap()
                .unwrap()
                .latest_context_input_tokens,
            456
        );
        assert!(old.append_turn("live", &[pending]).is_err());
        assert_eq!(
            fs::read(sessions.join("acp-sessions.db.backup")).unwrap(),
            backup
        );
        assert!(!sessions.join("acp-sessions.db").exists());
        assert_eq!(
            current
                .mark_session_killed_atomic_for_owner("live", Some("other"))
                .unwrap(),
            SessionKillTransition::Missing
        );
        assert_eq!(
            current
                .mark_session_killed_atomic_for_owner("live", Some("owner"))
                .unwrap(),
            SessionKillTransition::Marked
        );
        assert!(!current.delete_session_owned("live", "other").unwrap());
        assert!(current.delete_session_owned("live", "owner").unwrap());
        assert!(!current.contains_session("live").unwrap());
    }
}

fn imported_count(directory: &Path, target: NativeBackend) -> usize {
    let catalog = installation::session_ids(directory, target).expect("catalog");
    let stream = catalog
        .get("gw_fixture")
        .expect("read-visible imported family");
    let store = open_event_store(directory, target.name()).expect("native store");
    let receipt = store
        .lookup_receipt(stream, &format!("legacy-family-import:{stream}"))
        .expect("receipt")
        .expect("complete import");
    let event = store
        .read_event(&receipt.event_ids[0])
        .expect("event")
        .expect("projection");
    let EventData::ContextProjectionCreated { result_ref, .. } = event.body.data else {
        panic!("projection kind");
    };
    let PayloadRead::Present(context) =
        store.read_payload(&result_ref.payload_id).expect("payload")
    else {
        panic!("context present");
    };
    assert_eq!(StoredPayloadRef::from(&context), result_ref);
    let value: serde_json::Value = serde_json::from_slice(&context.body).expect("context");
    assert_eq!(value["history_has_trim_breadcrumb"], true);
    value["legacy_messages"].as_array().expect("messages").len()
}

#[test]
fn all_legacy_sessions_upgrade_in_bounded_batches_and_only_new_format_remains() {
    for sqlite in [false, true] {
        for target in [NativeBackend::Sqlite, NativeBackend::Jsonl] {
            let directory = tempfile::tempdir().expect("directory");
            let old = old_source(directory.path(), sqlite, 5001);
            upgrade_installation(directory.path(), target, |_| "fixture".into())
                .expect("upgrade beyond old batch limit");
            assert_eq!(imported_count(directory.path(), target), 5001);
            assert!(
                old.append("gw_fixture", &ChatMessage::user("late old writer"))
                    .is_err()
            );
            assert!(
                old.append("another", &ChatMessage::user("new legacy family"))
                    .is_err()
            );
            let sessions = directory.path().join("sessions");
            assert!(!sessions.join("sessions.db").exists());
            assert!(!sessions.join("gw_fixture.jsonl").exists());
            assert!(!sessions.join("gw_fixture.trim_breadcrumb").exists());
            assert!(
                sessions
                    .join(if sqlite {
                        "sessions.db.backup"
                    } else {
                        "gw_fixture.jsonl.backup"
                    })
                    .is_file()
            );
            assert!(
                SqliteSessionBackend::new(directory.path()).is_err(),
                "old constructors cannot recreate an archived database"
            );
            assert!(!sessions.join("sessions.db").exists());
            let manifest = fs::read(sessions.join("session-format.json")).expect("manifest");
            assert!(manifest.len() < 1024);
            assert!(!String::from_utf8_lossy(&manifest).contains("gw_fixture"));
            upgrade_installation(directory.path(), target, |_| "fixture".into())
                .expect("idempotent startup");
            assert_eq!(imported_count(directory.path(), target), 5001);
        }
    }
}

#[test]
fn installation_upgrade_crash_worker() {
    let Ok(directory) = std::env::var("ZEROCLAW_UPGRADE_DIRECTORY") else {
        return;
    };
    let target = NativeBackend::parse(&std::env::var("ZEROCLAW_UPGRADE_TARGET").expect("target"))
        .expect("backend");
    upgrade_installation(Path::new(&directory), target, |_| "fixture".into())
        .expect("worker upgrade");
    panic!("requested interruption was not reached");
}

#[test]
fn acp_upgrade_resumes_metadata_history_chunks_and_archived_source() {
    use crate::session_repository::SessionRepository;
    use zeroclaw_api::model_provider::ConversationMessage;
    for target in [NativeBackend::Sqlite, NativeBackend::Jsonl] {
        for point in [
            "legacy_acp_metadata",
            "legacy_acp_chunk",
            "installation_archived",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let old = SessionRepository::legacy_fixture(directory.path()).unwrap();
            old.create_session("interrupted", "acp", "fixture", "/workspace", Some("owner"))
                .unwrap();
            let messages: Vec<_> = (0..130)
                .map(|index| ConversationMessage::Chat(ChatMessage::user(format!("row {index}"))))
                .collect();
            old.append_turn("interrupted", &messages).unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "session_events::upgrade::tests::installation_upgrade_crash_worker",
                    "--nocapture",
                ])
                .env("ZEROCLAW_UPGRADE_DIRECTORY", directory.path())
                .env("ZEROCLAW_UPGRADE_TARGET", target.name())
                .env("ZEROCLAW_SESSION_CRASH_POINT", point)
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(86),
                "{target:?}/{point}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(old.append_turn("interrupted", &messages[..1]).is_err());
            upgrade_installation(directory.path(), target, |_| "fixture".into()).unwrap();
            let restored = SessionRepository::new(directory.path())
                .unwrap()
                .load_session("interrupted")
                .unwrap()
                .unwrap();
            assert_eq!(
                serde_json::to_value(restored.messages).unwrap(),
                serde_json::to_value(&messages).unwrap()
            );
            assert_eq!(restored.principal_id.as_deref(), Some("owner"));
            assert_eq!(restored.workspace_dir, "/workspace");
            assert!(!directory.path().join("sessions/acp-sessions.db").exists());
            assert!(
                directory
                    .path()
                    .join("sessions/acp-sessions.db.backup")
                    .is_file()
            );
        }
    }
}

#[test]
fn colliding_acp_and_transcript_ids_leave_both_sources_available() {
    use crate::session_repository::SessionRepository;
    for target in [NativeBackend::Sqlite, NativeBackend::Jsonl] {
        let directory = tempfile::tempdir().expect("directory");
        let old = old_source(directory.path(), true, 1);
        drop(old);
        let acp = SessionRepository::legacy_fixture(directory.path()).expect("ACP source");
        acp.create_session("gw_fixture", "acp", "fixture", "/workspace", None)
            .expect("colliding source");
        drop(acp);
        assert!(matches!(
            upgrade_installation(directory.path(), target, |_| "fixture".into()),
            Err(AppendError::InvalidReference(message))
                if message == "ACP session ID collides with another imported session"
        ));
        let sessions = directory.path().join("sessions");
        assert!(sessions.join("sessions.db").is_file());
        assert!(sessions.join("acp-sessions.db").is_file());
        assert!(!sessions.join("sessions.db.backup").exists());
        assert!(!sessions.join("acp-sessions.db.backup").exists());
        let guard = InstallationGuard::acquire(&sessions).expect("installation");
        assert!(matches!(guard.phase(), Some(FormatPhase::Upgrading { .. })));
    }
}

#[test]
fn acp_registration_completes_an_existing_native_root_with_exact_owner_once() {
    use crate::session_repository::SessionRepository;
    use zeroclaw_api::session_events::{CaptureMode, EventActor};
    for target in [NativeBackend::Sqlite, NativeBackend::Jsonl] {
        let directory = tempfile::tempdir().unwrap();
        upgrade_installation(directory.path(), target, |_| "fixture".into()).unwrap();
        let stream = session_id("registration").unwrap();
        let mut root = SessionEvent::typed(
            stream.clone(),
            EventData::SessionStarted {
                owning_agent: "fixture".into(),
                capture_mode: CaptureMode::Native,
            },
            EventActor::Principal {
                principal_id: "owner".into(),
            },
        )
        .unwrap();
        root.branch_id = Some(BranchId::parse(format!("branch-{stream}")).unwrap());
        let store = open_event_store(directory.path(), target.name()).unwrap();
        store
            .append_batch(AppendRequest {
                session_id: stream.clone(),
                expected_head: EventCursor::beginning(stream.clone()),
                idempotency_key: "root".into(),
                events: vec![root],
                payloads: vec![],
                erasure_reference_commitment: None,
            })
            .unwrap();
        let protocol = SessionRepository::new(directory.path()).unwrap();
        let root = protocol.load_session("registration").unwrap().unwrap();
        assert_eq!(root.agent_alias, "fixture");
        assert_eq!(root.principal_id.as_deref(), Some("owner"));
        assert!(root.workspace_dir.is_empty());
        assert!(
            protocol
                .create_session(
                    "registration",
                    "acp",
                    "fixture",
                    "/workspace",
                    Some("other")
                )
                .is_err()
        );
        assert!(
            protocol
                .create_session(
                    "registration",
                    "acp",
                    "other-agent",
                    "/workspace",
                    Some("owner")
                )
                .is_err()
        );
        protocol
            .create_session_with_interaction_surface(
                "registration",
                "acp",
                "fixture",
                "/workspace",
                Some("cli"),
                Some("owner"),
            )
            .unwrap();
        let data = protocol.load_session("registration").unwrap().unwrap();
        assert_eq!(data.principal_id.as_deref(), Some("owner"));
        assert_eq!(data.workspace_dir, "/workspace");
        assert_eq!(data.interaction_surface.as_deref(), Some("cli"));
        protocol
            .create_session(
                "registration",
                "acp",
                "fixture",
                "/workspace",
                Some("owner"),
            )
            .expect("idempotent reopen");
        assert!(!directory.path().join("sessions/acp-sessions.db").exists());
    }
}

#[test]
fn interrupted_upgrade_resumes_chunks_and_partial_backup_renames() {
    for target in [NativeBackend::Sqlite, NativeBackend::Jsonl] {
        for point in [
            "legacy_import_root",
            "legacy_import_chunk",
            "installation_imported",
            "installation_source_archived",
            "installation_archived",
        ] {
            let directory = tempfile::tempdir().expect("directory");
            let old = old_source(directory.path(), false, 5);
            let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
                .args([
                    "--exact",
                    "session_events::upgrade::tests::installation_upgrade_crash_worker",
                    "--nocapture",
                ])
                .env("ZEROCLAW_UPGRADE_DIRECTORY", directory.path())
                .env("ZEROCLAW_UPGRADE_TARGET", target.name())
                .env("ZEROCLAW_SESSION_CRASH_POINT", point)
                .output()
                .expect("worker");
            assert_eq!(
                output.status.code(),
                Some(86),
                "{target:?}/{point}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                old.append("gw_fixture", &ChatMessage::user("late"))
                    .is_err()
            );
            upgrade_installation(directory.path(), target, |_| "fixture".into()).expect("resume");
            assert_eq!(imported_count(directory.path(), target), 5);
            assert!(
                directory
                    .path()
                    .join("sessions/gw_fixture.jsonl.backup")
                    .exists()
            );
        }
    }
}

#[test]
fn native_sqlite_reopens_without_rewriting_events_and_future_schema_is_refused() {
    let directory = tempfile::tempdir().expect("directory");
    let store = SqliteEventStore::open(directory.path()).expect("store");
    let stream = SessionId::new();
    store
        .append_batch(super::super::tests::request(&stream, "root", 0))
        .expect("root");
    let before = store
        .read_page(&stream, &EventCursor::beginning(stream.clone()), None, 10)
        .expect("before")
        .raw_events;
    drop(store);
    let path = directory.path().join("sessions/session-events.db");
    let store = SqliteEventStore::open(directory.path()).expect("reopened store");
    assert_eq!(
        before,
        store
            .read_page(&stream, &EventCursor::beginning(stream.clone()), None, 10)
            .expect("after")
            .raw_events
    );
    let conn = Connection::open(path).expect("schema");
    assert_eq!(
        conn.pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))
            .expect("version"),
        1
    );
    conn.pragma_update(None, "user_version", 2)
        .expect("future version");
    drop(store);
    assert!(matches!(
        SqliteEventStore::open(directory.path()),
        Err(AppendError::UnsupportedSchema(_))
    ));
}

#[test]
fn legacy_history_larger_than_a_commit_and_a_single_fragment_is_preserved() {
    for target in [NativeBackend::Sqlite, NativeBackend::Jsonl] {
        let directory = tempfile::tempdir().expect("directory");
        let source = SessionStore::new(directory.path()).expect("source");
        let text = "a".repeat(65 * 1024 * 1024);
        source
            .append("large", &ChatMessage::user(text.clone()))
            .expect("large source");
        upgrade_installation(directory.path(), target, |_| "fixture".into())
            .expect("bounded upgrade");
        let store = open_event_store(directory.path(), target.name()).expect("store");
        let stream = session_id("large").expect("stream");
        let receipt = store
            .lookup_receipt(&stream, &format!("legacy-family-import:{stream}"))
            .expect("receipt")
            .expect("complete");
        let event = store
            .read_event(&receipt.event_ids[0])
            .expect("event")
            .expect("projection");
        let EventData::ContextProjectionCreated { result_ref, .. } = event.body.data else {
            panic!("projection");
        };
        let PayloadRead::Present(context) =
            store.read_payload(&result_ref.payload_id).expect("context")
        else {
            panic!("context");
        };
        let context: serde_json::Value = serde_json::from_slice(&context.body).expect("structure");
        let content_ref: StoredPayloadRef =
            serde_json::from_value(context["legacy_messages"][0]["content_ref"].clone())
                .expect("content reference");
        let PayloadRead::Present(content) = store
            .read_payload(&content_ref.payload_id)
            .expect("content")
        else {
            panic!("content");
        };
        let content: zeroclaw_api::session_events::ContentPayload =
            serde_json::from_slice(&content.body).expect("content structure");
        let mut restored = Vec::new();
        for part in content.parts {
            let zeroclaw_api::session_events::RecordedContentPart::Text { text_ref } = part else {
                panic!("text fragment");
            };
            let PayloadRead::Present(fragment) =
                store.read_payload(&text_ref.payload_id).expect("fragment")
            else {
                panic!("fragment");
            };
            restored.extend(fragment.body);
        }
        assert_eq!(restored, text.as_bytes());
        assert!(!directory.path().join("sessions/large.jsonl").exists());
        assert!(
            directory
                .path()
                .join("sessions/large.jsonl.backup")
                .exists()
        );
    }
}

#[test]
fn conflicting_sources_and_backup_collisions_leave_the_installation_fenced() {
    let directory = tempfile::tempdir().expect("directory");
    let jsonl = SessionStore::new(directory.path()).expect("JSONL");
    jsonl
        .append("conflict", &ChatMessage::user("JSONL history"))
        .expect("source");
    let sqlite = SqliteSessionBackend::new(directory.path()).expect("SQLite");
    sqlite
        .append("conflict", &ChatMessage::user("different SQLite history"))
        .expect("source");
    assert!(
        upgrade_installation(directory.path(), NativeBackend::Sqlite, |_| "fixture"
            .into())
        .is_err()
    );
    assert!(directory.path().join("sessions/conflict.jsonl").exists());
    assert!(directory.path().join("sessions/sessions.db").exists());
    assert!(
        InstallationGuard::acquire(&directory.path().join("sessions"))
            .expect("guard")
            .active()
            .is_err()
    );
    let directory = tempfile::tempdir().expect("directory");
    old_source(directory.path(), false, 3);
    let backup = directory.path().join("sessions/gw_fixture.jsonl.backup");
    fs::write(&backup, b"existing backup").expect("backup");
    assert!(
        upgrade_installation(directory.path(), NativeBackend::Jsonl, |_| "fixture".into()).is_err()
    );
    assert_eq!(
        fs::read(&backup).expect("untouched backup"),
        b"existing backup"
    );
    fs::rename(backup, directory.path().join("saved.backup")).expect("resolve collision");
    upgrade_installation(directory.path(), NativeBackend::Jsonl, |_| "fixture".into())
        .expect("resume");
    assert_eq!(imported_count(directory.path(), NativeBackend::Jsonl), 3);
}
