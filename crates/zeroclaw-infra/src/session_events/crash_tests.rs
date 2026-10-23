//! Subprocess interruption at real storage publication boundaries.
use super::*;
use zeroclaw_api::session_events::*;

pub(super) fn crash_boundary(point: &str) {
    if std::env::var("ZEROCLAW_SESSION_CRASH_POINT")
        .ok()
        .as_deref()
        == Some(point)
    {
        if std::env::var("ZEROCLAW_SESSION_CRASH_ACTION").as_deref() == Ok("power_loss") {
            use std::io::Write;
            println!("ZEROCLAW_POWER_LOSS_BOUNDARY={point}");
            std::io::stdout().flush().expect("publish VM boundary");
            loop {
                std::thread::park();
            }
        }
        // Exit without unwinding any transaction or publication guard.
        std::process::exit(86);
    }
}

/// Guest-side fixture for the opt-in QEMU machine-interruption harness. The
/// ordinary suite has no VM dependency; the host kills the guest at a flushed
/// serial marker, discarding its kernel's dirty pages, then boots recovery.
#[test]
fn power_loss_worker() {
    let Ok(stage) = std::env::var("ZEROCLAW_SESSION_POWER_LOSS_STAGE") else {
        return;
    };
    let directory = std::env::var("ZEROCLAW_SESSION_CRASH_DIRECTORY").expect("directory");
    let directory = Path::new(&directory);
    let backend = std::env::var("ZEROCLAW_SESSION_CRASH_BACKEND").expect("backend");
    if stage == "prepare" {
        fs::create_dir_all(directory).expect("fixture directory");
        let stream = SessionStreamId::new();
        let store = open_event_store(directory, &backend).expect("store");
        store
            .append_batch(super::tests::request(&stream, "root", 0))
            .expect("root");
        let payload = PayloadRecord::new(
            PayloadId::new(),
            "session.text/v1".into(),
            b"durable fixture".to_vec(),
        )
        .expect("payload");
        let events = ["title", "description"]
            .into_iter()
            .map(|field| {
                SessionEvent::typed(
                    stream.clone(),
                    EventData::SessionMetadataChanged {
                        field: field.into(),
                        new_value_ref: StoredPayloadRef::from(&payload),
                    },
                    EventActor::default(),
                )
                .expect("metadata")
            })
            .collect();
        let request = AppendRequest {
            stream_id: stream.clone(),
            expected_head: store.head(&stream).expect("head"),
            idempotency_key: "frozen-power-loss-batch".into(),
            events,
            payloads: vec![payload],
            erasure_reference_commitment: None,
        };
        let path = directory.join("request.json");
        fs::write(&path, canonical_json(&request).expect("encoding")).expect("fixture");
        fs::File::open(path)
            .expect("fixture file")
            .sync_all()
            .expect("fixture durable");
        jsonl::sync_directory(directory).expect("fixture directory durable");
    } else {
        let request: AppendRequest =
            serde_json::from_slice(&fs::read(directory.join("request.json")).expect("request"))
                .expect("decode");
        let store = open_event_store(directory, &backend).expect("reopen");
        if stage == "append" {
            store.append_batch(request).expect("append");
            panic!("requested machine boundary was not reached");
        }
        assert_eq!(stage, "recover");
        let visible = std::env::var("ZEROCLAW_SESSION_EXPECT_VISIBLE").as_deref() == Ok("true");
        assert_eq!(
            store
                .head(&request.stream_id)
                .expect("recovered head")
                .sequence,
            if visible { 3 } else { 1 }
        );
        assert_eq!(
            store
                .lookup_receipt(&request.stream_id, &request.idempotency_key)
                .expect("receipt")
                .is_some(),
            visible
        );
        let receipt = store
            .append_batch(request.clone())
            .expect("retry frozen request");
        assert_eq!(receipt.first.sequence, 2);
        assert_eq!(receipt.last.sequence, 3);
        assert_eq!(
            store.append_batch(request.clone()).expect("second retry"),
            receipt
        );
        assert_eq!(
            store.head(&request.stream_id).expect("final head").sequence,
            3
        );
        let page = store
            .read_page(
                &request.stream_id,
                &EventCursor::beginning(request.stream_id.clone()),
                None,
                10,
                MAX_PAGE_BYTES,
            )
            .expect("complete page");
        assert_eq!(page.events.len(), 3);
        for payload in request.payloads {
            assert_eq!(
                store
                    .read_payload(&payload.payload_id)
                    .expect("durable payload"),
                PayloadRead::Present(payload)
            );
        }
    }
    println!("ZEROCLAW_POWER_LOSS_STAGE_OK={stage}");
}

#[test]
fn crash_worker() {
    let Ok(directory) = std::env::var("ZEROCLAW_SESSION_CRASH_DIRECTORY") else {
        return;
    };
    let backend = std::env::var("ZEROCLAW_SESSION_CRASH_BACKEND").expect("backend");
    let bytes = fs::read(Path::new(&directory).join("request.json")).expect("request bytes");
    let request: AppendRequest = serde_json::from_slice(&bytes).expect("frozen request");
    let store = open_event_store(Path::new(&directory), &backend).expect("child open");
    store.append_batch(request).expect("child append");
    panic!("requested storage boundary was not reached");
}

#[test]
fn process_interruption_is_atomic_and_receipt_retry_never_duplicates() {
    for (backend, point, visible) in [
        ("sqlite", "sqlite_before_commit", false),
        ("sqlite", "sqlite_after_commit", true),
        ("jsonl", "jsonl_segment_published", false),
        ("jsonl", "jsonl_commit_published", false),
        ("jsonl", "jsonl_root_published", true),
    ] {
        let directory = tempfile::TempDir::new().expect("directory");
        let stream = SessionStreamId::new();
        let store = open_event_store(directory.path(), backend).expect("store");
        store
            .append_batch(super::tests::request(&stream, "root", 0))
            .expect("root");
        let payload = PayloadRecord::new(
            PayloadId::new(),
            "session.text/v1".into(),
            b"durable fixture".to_vec(),
        )
        .expect("payload");
        let events = ["title", "description"]
            .into_iter()
            .map(|field| {
                SessionEvent::typed(
                    stream.clone(),
                    EventData::SessionMetadataChanged {
                        field: field.into(),
                        new_value_ref: StoredPayloadRef::from(&payload),
                    },
                    EventActor::default(),
                )
                .expect("metadata")
            })
            .collect();
        let request = AppendRequest {
            stream_id: stream.clone(),
            expected_head: store.head(&stream).expect("head"),
            idempotency_key: "frozen-crash-batch".into(),
            events,
            payloads: vec![payload.clone()],
            erasure_reference_commitment: None,
        };
        fs::write(
            directory.path().join("request.json"),
            canonical_json(&request).expect("encoding"),
        )
        .expect("request file");
        drop(store);
        let child = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "session_events::crash_tests::crash_worker",
                "--nocapture",
            ])
            .env("ZEROCLAW_SESSION_CRASH_DIRECTORY", directory.path())
            .env("ZEROCLAW_SESSION_CRASH_BACKEND", backend)
            .env("ZEROCLAW_SESSION_CRASH_POINT", point)
            .output()
            .expect("child process");
        assert_eq!(
            child.status.code(),
            Some(86),
            "{backend}/{point}: {}",
            String::from_utf8_lossy(&child.stderr)
        );
        let recovered =
            open_event_store(directory.path(), backend).expect("reopen interrupted writer");
        let head = recovered.head(&stream).expect("recovered head");
        assert_eq!(
            head.sequence,
            if visible { 3 } else { 1 },
            "{backend}/{point}"
        );
        assert_eq!(
            recovered
                .lookup_receipt(&stream, &request.idempotency_key)
                .expect("receipt lookup")
                .is_some(),
            visible
        );
        let receipt = recovered
            .append_batch(request.clone())
            .expect("verify or publish original frozen request");
        assert_eq!(receipt.first.sequence, 2);
        assert_eq!(receipt.last.sequence, 3);
        assert_eq!(recovered.append_batch(request).expect("retry"), receipt);
        assert_eq!(recovered.head(&stream).expect("final head").sequence, 3);
        assert_eq!(
            recovered
                .read_payload(&payload.payload_id)
                .expect("durable payload"),
            PayloadRead::Present(payload)
        );
    }
}
