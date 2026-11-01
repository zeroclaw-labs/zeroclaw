use super::*;
use zeroclaw_api::session_events::{BranchId, CaptureMode, EventActor, SessionId};

fn seed(
    dir: &Path,
    backend: NativeBackend,
) -> (SessionStreamId, AppendRequest, Arc<dyn SessionEventStore>) {
    let name = match backend {
        NativeBackend::Sqlite => "sqlite",
        NativeBackend::Jsonl => "jsonl",
    };
    super::super::upgrade::upgrade_installation(dir, backend, |_| "fixture".into())
        .expect("format");
    let store = open_event_store(dir, name).expect("store");
    let stream = SessionStreamId::new();
    let branch = BranchId::new();
    let mut event = SessionEvent::typed(
        stream.clone(),
        EventData::SessionStarted {
            session_id: SessionId::new(),
            owning_agent: "fixture".into(),
            capture_mode: CaptureMode::Native,
        },
        EventActor::System {
            component: "fixture".into(),
        },
    )
    .expect("event");
    event.branch_id = Some(branch);
    let (identity, identity_payload) =
        super::super::installation::family_identity(stream.clone(), "fixture").expect("identity");
    let request = AppendRequest {
        stream_id: stream.clone(),
        expected_head: EventCursor::beginning(stream.clone()),
        idempotency_key: "root".into(),
        events: vec![event, identity],
        payloads: vec![identity_payload],
        erasure_reference_commitment: None,
    };
    store.append_batch(request.clone()).expect("root");
    (stream, request, store)
}

#[test]
fn native_roundtrip_preserves_raw_bytes_receipts_and_stale_writer_fence() {
    for (source, target) in [
        (NativeBackend::Sqlite, NativeBackend::Jsonl),
        (NativeBackend::Jsonl, NativeBackend::Sqlite),
    ] {
        let dir = tempfile::TempDir::new().expect("dir");
        let (root, request, store) = seed(dir.path(), source);
        let old = family::FamilyEventStore::open(dir.path(), "fixture").expect("old handle");
        let before = store
            .read_page(&root, &EventCursor::beginning(root.clone()), None, 100)
            .expect("before");
        let receipt = store.lookup_receipt(&root, "root").expect("receipt");
        migrate_installation(dir.path(), target).expect("migrate");
        assert!(
            old.append_batch(request.clone()).is_err(),
            "old writer must stay fenced even for a retry"
        );
        let migrated = family::FamilyEventStore::open(dir.path(), "fixture").expect("target");
        let after = migrated
            .read_page(&root, &EventCursor::beginning(root.clone()), None, 100)
            .expect("after");
        assert_eq!(before.raw_events, after.raw_events);
        assert_eq!(
            receipt,
            migrated
                .lookup_receipt(&root, "root")
                .expect("target receipt")
        );
        assert_eq!(
            migrated.append_batch(request.clone()).expect("retry"),
            receipt.expect("receipt present")
        );
        migrate_installation(dir.path(), target).expect("idempotent migration");
        migrate_installation(dir.path(), source).expect("roundtrip");
        assert!(
            old.append_batch(request.clone()).is_err(),
            "generation fences survive a roundtrip"
        );
        let reopened =
            family::FamilyEventStore::open(dir.path(), "fixture").expect("roundtrip handle");
        assert_eq!(
            before.raw_events,
            reopened
                .read_page(&root, &EventCursor::beginning(root.clone()), None, 100)
                .expect("roundtrip bytes")
                .raw_events
        );
    }
}

#[test]
fn frozen_native_migration_resumes_and_rejects_changed_source() {
    for (source, target) in [
        (NativeBackend::Sqlite, NativeBackend::Jsonl),
        (NativeBackend::Jsonl, NativeBackend::Sqlite),
    ] {
        let dir = tempfile::TempDir::new().expect("dir");
        seed(dir.path(), source);
        let digest = export(dir.path(), source, None)
            .expect("image")
            .digest()
            .expect("digest");
        {
            let mut locator =
                InstallationGuard::acquire(&dir.path().join("sessions")).expect("locator");
            locator
                .begin(FormatPhase::Switching {
                    source,
                    target,
                    image_digest: digest,
                })
                .expect("freeze");
        }
        assert!(family::FamilyEventStore::open(dir.path(), "fixture").is_err());
        migrate_installation(dir.path(), target).expect("resume");
        assert!(family::FamilyEventStore::open(dir.path(), "fixture").is_ok());
    }
}

#[test]
fn migration_never_takes_over_an_active_executor() {
    let dir = tempfile::TempDir::new().expect("dir");
    seed(dir.path(), NativeBackend::Sqlite);
    let _lease = ownership::FamilyExecutionLease::acquire(dir.path(), "fixture").expect("lease");
    assert!(migrate_installation(dir.path(), NativeBackend::Jsonl).is_err());
    assert!(matches!(
        InstallationGuard::acquire(&dir.path().join("sessions"))
            .expect("locator")
            .phase(),
        Some(FormatPhase::Ready {
            backend: NativeBackend::Sqlite,
            ..
        })
    ));
}

#[test]
fn migration_crash_worker() {
    let Ok(directory) = std::env::var("ZEROCLAW_MIGRATION_CRASH_DIRECTORY") else {
        return;
    };
    let target = match std::env::var("ZEROCLAW_MIGRATION_CRASH_TARGET")
        .expect("target")
        .as_str()
    {
        "sqlite" => NativeBackend::Sqlite,
        "jsonl" => NativeBackend::Jsonl,
        _ => panic!("invalid fixture target"),
    };
    migrate_installation(Path::new(&directory), target).expect("child migration");
    panic!("requested migration boundary was not reached");
}

#[test]
fn native_migration_process_interruption_resumes_exact_frozen_image() {
    for (source, target, target_name) in [
        (NativeBackend::Sqlite, NativeBackend::Jsonl, "jsonl"),
        (NativeBackend::Jsonl, NativeBackend::Sqlite, "sqlite"),
    ] {
        for point in [
            "native_migration_frozen",
            "native_migration_imported",
            "native_migration_archived",
            "native_migration_published",
        ] {
            let dir = tempfile::TempDir::new().expect("dir");
            let (root, request, store) = seed(dir.path(), source);
            let before = export(dir.path(), source, None)
                .expect("source")
                .digest()
                .expect("digest");
            let receipt = store
                .lookup_receipt(&root, &request.idempotency_key)
                .expect("receipt");
            drop(store);
            let child = std::process::Command::new(std::env::current_exe().expect("executable"))
                .args([
                    "--exact",
                    "session_events::migration::tests::migration_crash_worker",
                    "--nocapture",
                ])
                .env("ZEROCLAW_MIGRATION_CRASH_DIRECTORY", dir.path())
                .env("ZEROCLAW_MIGRATION_CRASH_TARGET", target_name)
                .env("ZEROCLAW_SESSION_CRASH_POINT", point)
                .output()
                .expect("child");
            assert_eq!(
                child.status.code(),
                Some(86),
                "{source:?}/{point}: {}",
                String::from_utf8_lossy(&child.stderr)
            );
            if point != "native_migration_published" {
                assert!(
                    family::FamilyEventStore::open(dir.path(), "fixture").is_err(),
                    "both writers remain frozen"
                );
            }
            migrate_installation(dir.path(), target).expect("resume");
            assert_eq!(
                before,
                export(dir.path(), target, None)
                    .expect("target")
                    .digest()
                    .expect("digest")
            );
            let migrated =
                family::FamilyEventStore::open(dir.path(), "fixture").expect("target writer");
            assert_eq!(
                receipt,
                migrated
                    .lookup_receipt(&root, &request.idempotency_key)
                    .expect("receipt")
            );
            assert_eq!(
                migrated.append_batch(request).expect("original retry"),
                receipt.expect("present")
            );
        }
    }
}

#[test]
fn frozen_native_migration_rejects_a_changed_source_commitment() {
    let dir = tempfile::TempDir::new().expect("dir");
    let (root, _, store) = seed(dir.path(), NativeBackend::Sqlite);
    let digest = export(dir.path(), NativeBackend::Sqlite, None)
        .expect("image")
        .digest()
        .expect("digest");
    {
        let mut locator =
            InstallationGuard::acquire(&dir.path().join("sessions")).expect("locator");
        locator
            .begin(FormatPhase::Switching {
                source: NativeBackend::Sqlite,
                target: NativeBackend::Jsonl,
                image_digest: digest,
            })
            .expect("freeze");
    }
    // The raw backend bypasses the family writer gate only to model damaged
    // storage. Real executors never receive this backend handle.
    let payload = PayloadRecord::new(
        PayloadId::new(),
        "session.text/v1".into(),
        b"changed".to_vec(),
    )
    .expect("payload");
    let event = SessionEvent::typed(
        root.clone(),
        EventData::SessionMetadataChanged {
            field: "title".into(),
            new_value_ref: StoredPayloadRef::from(&payload),
        },
        EventActor::default(),
    )
    .expect("event");
    store
        .append_batch(AppendRequest {
            stream_id: root.clone(),
            expected_head: store.head(&root).expect("head"),
            idempotency_key: "damage".into(),
            events: vec![event],
            payloads: vec![payload],
            erasure_reference_commitment: None,
        })
        .expect("simulated damage");
    assert!(matches!(
        migrate_installation(dir.path(), NativeBackend::Jsonl),
        Err(AppendError::Corrupt(_))
    ));
    assert!(family::FamilyEventStore::open(dir.path(), "fixture").is_err());
}

#[test]
fn migration_preserves_future_event_bytes_and_its_explicit_payload_references() {
    let dir = tempfile::TempDir::new().expect("dir");
    let (root, _, _) = seed(dir.path(), NativeBackend::Sqlite);
    let mut image = export(dir.path(), NativeBackend::Sqlite, None).expect("image");
    let leaf = PayloadRecord::new(
        PayloadId::new(),
        "session.bytes/v1".into(),
        b"future observation bytes".to_vec(),
    )
    .expect("payload");
    let mut event = SessionEvent::typed(
        root.clone(),
        EventData::SessionMetadataChanged {
            field: "title".into(),
            new_value_ref: StoredPayloadRef::from(&leaf),
        },
        EventActor::default(),
    )
    .expect("event");
    event.sequence = 3;
    event.committed_at = now_millis();
    let mut value = serde_json::to_value(&event).expect("json");
    value["kind"] = serde_json::json!("future_observation");
    value["body"]["fields"]["future_extension"] =
        serde_json::json!({"preserve":"opaque spacing and extension"});
    // Noncanonical formatting is deliberate: transfer must not reserialize.
    let raw = format!(" {} ", serde_json::to_string(&value).expect("future bytes")).into_bytes();
    assert_eq!(
        parse_event(&raw).expect("decode").body.kind(),
        zeroclaw_api::session_events::EventKind::Unknown
    );
    image.batches.push(NativeBatch {
        stream: root.clone(),
        key: "future".into(),
        commitment: Digest::from_bytes(&raw),
        raw: vec![raw.clone()],
    });
    image.payloads.push(NativePayload {
        reference: StoredPayloadRef::from(&leaf),
        body: Some(leaf.body.clone()),
    });
    SqliteEventStore::open(dir.path())
        .expect("source")
        .import_native(&image)
        .expect("future-version source fixture");
    migrate_installation(dir.path(), NativeBackend::Jsonl).expect("migration");
    let store = family::FamilyEventStore::open(dir.path(), "fixture").expect("target");
    let page = store
        .read_page(&root, &EventCursor::beginning(root.clone()), None, 10)
        .expect("page");
    assert_eq!(page.raw_events[2], raw);
    assert_eq!(
        store.read_payload(&leaf.payload_id).expect("payload"),
        PayloadRead::Present(leaf)
    );
}

#[test]
fn installation_migration_preserves_shared_nested_content() {
    for backend in [NativeBackend::Sqlite, NativeBackend::Jsonl] {
        let dir = tempfile::TempDir::new().expect("dir");
        let (root, _, store) = seed(dir.path(), backend);
        let other = SessionStreamId::new();
        let mut start = SessionEvent::typed(
            other.clone(),
            EventData::SessionStarted {
                session_id: SessionId::new(),
                owning_agent: "other".into(),
                capture_mode: CaptureMode::Native,
            },
            EventActor::default(),
        )
        .expect("start");
        start.branch_id = Some(BranchId::new());
        store
            .append_batch(AppendRequest {
                stream_id: other.clone(),
                expected_head: EventCursor::beginning(other.clone()),
                idempotency_key: "other-root".into(),
                events: vec![start],
                payloads: vec![],
                erasure_reference_commitment: None,
            })
            .expect("other root");
        let leaf = PayloadRecord::new(
            PayloadId::new(),
            "session.text/v1".into(),
            b"shared leaf".to_vec(),
        )
        .expect("leaf");
        for (index, stream) in [&root, &other].into_iter().enumerate() {
            let evidence = PayloadRecord::json(
                "session.evidence/v1",
                &serde_json::json!({"nested_ref":StoredPayloadRef::from(&leaf)}),
            )
            .expect("structure");
            let event = SessionEvent::typed(
                stream.clone(),
                EventData::SessionMetadataChanged {
                    field: "description".into(),
                    new_value_ref: StoredPayloadRef::from(&evidence),
                },
                EventActor::default(),
            )
            .expect("event");
            let mut payloads = vec![evidence];
            if index == 0 {
                payloads.push(leaf.clone());
            }
            store
                .append_batch(AppendRequest {
                    stream_id: stream.clone(),
                    expected_head: store.head(stream).expect("head"),
                    idempotency_key: "shared".into(),
                    events: vec![event],
                    payloads,
                    erasure_reference_commitment: None,
                })
                .expect("reference");
        }
        let target = if backend == NativeBackend::Sqlite {
            NativeBackend::Jsonl
        } else {
            NativeBackend::Sqlite
        };
        migrate_installation(dir.path(), target)
            .expect("whole installation preserves shared payloads");
        assert!(family::FamilyEventStore::open(dir.path(), "fixture").is_ok());
    }
}

#[test]
fn migration_commitment_streams_identical_canonical_bytes_without_cloning_payloads() {
    let dir = tempfile::TempDir::new().expect("directory");
    let store = SqliteEventStore::open(dir.path()).expect("store");
    let root = SessionStreamId::new();
    store
        .append_batch(super::super::tests::request(&root, "fixture", 0))
        .expect("root");
    let image = store.export_native(Some(&root)).expect("image");
    let mut sorted = image.clone();
    sorted
        .batches
        .sort_by(|a, b| a.stream.cmp(&b.stream).then_with(|| a.key.cmp(&b.key)));
    sorted
        .payloads
        .sort_by(|a, b| a.reference.payload_id.cmp(&b.reference.payload_id));
    assert_eq!(
        image.digest().expect("streamed digest"),
        Digest::from_bytes(
            &zeroclaw_api::session_events::canonical_json(&sorted).expect("canonical codec")
        )
    );
}

#[test]
fn migration_refuses_target_extra_tail_and_conflicting_receipt() {
    for (source, target) in [
        (NativeBackend::Sqlite, NativeBackend::Jsonl),
        (NativeBackend::Jsonl, NativeBackend::Sqlite),
    ] {
        for extra_tail in [false, true] {
            let directory = tempfile::TempDir::new().expect("directory");
            let (root, _, _) = seed(directory.path(), source);
            let image = export(directory.path(), source, None).expect("source image");
            match target {
                NativeBackend::Sqlite => SqliteEventStore::open(directory.path())
                    .expect("target")
                    .import_native(&image)
                    .expect("seed target"),
                NativeBackend::Jsonl => JsonlEventStore::open(directory.path())
                    .expect("target")
                    .import_native(&image)
                    .expect("seed target"),
            }
            if extra_tail {
                let target_store = open_event_store(
                    directory.path(),
                    match target {
                        NativeBackend::Sqlite => "sqlite",
                        NativeBackend::Jsonl => "jsonl",
                    },
                )
                .expect("target");
                let title = PayloadRecord::new(
                    PayloadId::new(),
                    "session.text/v1".into(),
                    b"target-only tail".to_vec(),
                )
                .expect("payload");
                let event = SessionEvent::typed(
                    root.clone(),
                    EventData::SessionMetadataChanged {
                        field: "title".into(),
                        new_value_ref: StoredPayloadRef::from(&title),
                    },
                    EventActor::default(),
                )
                .expect("event");
                target_store
                    .append_batch(AppendRequest {
                        stream_id: root.clone(),
                        expected_head: target_store.head(&root).expect("head"),
                        idempotency_key: "target-only".into(),
                        events: vec![event],
                        payloads: vec![title],
                        erasure_reference_commitment: None,
                    })
                    .expect("extra tail");
                assert!(migrate_installation(directory.path(), target).is_err());
            } else {
                let mut conflicting = image;
                conflicting.batches[0].commitment =
                    Digest::from_bytes(b"different original receipt");
                let result = match target {
                    NativeBackend::Sqlite => SqliteEventStore::open(directory.path())
                        .expect("target")
                        .import_native(&conflicting),
                    NativeBackend::Jsonl => JsonlEventStore::open(directory.path())
                        .expect("target")
                        .import_native(&conflicting),
                };
                assert!(result.is_err());
            }
        }
    }
}

#[test]
fn migration_retains_erasure_skeleton_and_never_restores_deleted_bytes() {
    for (source, target) in [
        (NativeBackend::Sqlite, NativeBackend::Jsonl),
        (NativeBackend::Jsonl, NativeBackend::Sqlite),
    ] {
        let directory = tempfile::TempDir::new().expect("directory");
        let (root, _, store) = seed(directory.path(), source);
        let leaf = PayloadRecord::new(
            PayloadId::new(),
            "session.text/v1".into(),
            b"fixture erased content".to_vec(),
        )
        .expect("leaf");
        let reference = StoredPayloadRef::from(&leaf);
        let title = SessionEvent::typed(
            root.clone(),
            EventData::SessionMetadataChanged {
                field: "title".into(),
                new_value_ref: reference.clone(),
            },
            EventActor::default(),
        )
        .expect("event");
        store
            .append_batch(AppendRequest {
                stream_id: root.clone(),
                expected_head: store.head(&root).expect("head"),
                idempotency_key: "title".into(),
                events: vec![title],
                payloads: vec![leaf],
                erasure_reference_commitment: None,
            })
            .expect("title");
        let intent = SessionEvent::typed(
            root.clone(),
            EventData::PayloadErasureRequested {
                targets: vec![reference.clone()],
                reason_category: "fixture_erasure".into(),
            },
            EventActor::default(),
        )
        .expect("intent");
        let intent_id = intent.event_id.clone();
        let (_, commitment) = store
            .erasure_scope(std::slice::from_ref(&reference))
            .expect("scope");
        store
            .append_batch(AppendRequest {
                stream_id: root.clone(),
                expected_head: store.head(&root).expect("head"),
                idempotency_key: "erase-intent".into(),
                events: vec![intent],
                payloads: vec![],
                erasure_reference_commitment: Some(commitment),
            })
            .expect("intent");
        let confirmed = SessionEvent::typed(
            root.clone(),
            EventData::PayloadRedacted {
                erasure_requested_event_id: intent_id,
            },
            EventActor::default(),
        )
        .expect("confirm");
        store
            .append_batch(AppendRequest {
                stream_id: root.clone(),
                expected_head: store.head(&root).expect("head"),
                idempotency_key: "erase-confirm".into(),
                events: vec![confirmed],
                payloads: vec![],
                erasure_reference_commitment: None,
            })
            .expect("confirm");
        let before = export(directory.path(), source, None)
            .expect("source")
            .digest()
            .expect("digest");
        migrate_installation(directory.path(), target).expect("migrate");
        let migrated = family::FamilyEventStore::open(directory.path(), "fixture").expect("target");
        assert_eq!(
            migrated.read_payload(&reference.payload_id).expect("read"),
            zeroclaw_api::session_events::PayloadRead::Redacted
        );
        assert_eq!(
            before,
            export(directory.path(), target, None)
                .expect("image")
                .digest()
                .expect("digest")
        );
    }
}
