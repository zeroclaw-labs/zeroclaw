//! Reproducible large-history measurements; explicitly opted into because the
//! default fixture creates one million events per backend under the system
//! temporary directory (set TMPDIR explicitly to choose the filesystem).
use super::*;
use zeroclaw_api::session_events::*;

fn disk_bytes(path: &std::path::Path) -> u64 {
    std::fs::read_dir(path)
        .expect("measurement directory")
        .map(|entry| {
            let entry = entry.expect("measurement entry");
            let metadata = entry.metadata().expect("measurement metadata");
            if metadata.is_dir() {
                disk_bytes(&entry.path())
            } else {
                metadata.len()
            }
        })
        .sum()
}
fn memory_kib(field: &str) -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| {
            let tail = line.strip_prefix(field)?;
            tail.split_whitespace().next()?.parse().ok()
        })
}
#[test]
#[ignore = "million-event storage measurement"]
fn million_event_append_read_and_index_reconstruction() {
    use std::time::Instant;
    let count = std::env::var("ZEROCLAW_SESSION_SCALE_EVENTS")
        .ok()
        .map(|value| value.parse::<usize>().expect("event count"))
        .unwrap_or(1_000_000);
    assert!(count >= 1000);
    for backend in ["sqlite", "jsonl"] {
        let directory = tempfile::TempDir::new().expect("directory");
        let stream = SessionStreamId::new();
        let store = open_event_store(directory.path(), backend).expect("store");
        store
            .append_batch(super::tests::request(&stream, "root", 0))
            .expect("root");
        let body = PayloadRecord::new(
            PayloadId::new(),
            "session.text/v1".into(),
            b"fixture title".to_vec(),
        )
        .expect("payload");
        let mut sequence = 1;
        let mut times = Vec::new();
        let start = Instant::now();
        for batch in 0..count.div_ceil(1000) {
            let amount = (count - batch * 1000).min(1000);
            let events = (0..amount)
                .map(|_| {
                    SessionEvent::typed(
                        stream.clone(),
                        EventData::SessionMetadataChanged {
                            field: "title".into(),
                            new_value_ref: StoredPayloadRef::from(&body),
                        },
                        EventActor::default(),
                    )
                    .expect("event")
                })
                .collect();
            let started = Instant::now();
            store
                .append_batch(AppendRequest {
                    stream_id: stream.clone(),
                    expected_head: EventCursor {
                        stream_id: stream.clone(),
                        sequence,
                    },
                    idempotency_key: format!("measurement-{batch}"),
                    events,
                    payloads: if batch == 0 {
                        vec![body.clone()]
                    } else {
                        vec![]
                    },
                    erasure_reference_commitment: None,
                })
                .expect("batch");
            times.push(started.elapsed().as_micros());
            sequence += amount as u64;
            if (batch + 1) % 100 == 0 {
                eprintln!(
                    "backend={backend} committed_events={} elapsed={:?}",
                    sequence - 1,
                    start.elapsed()
                );
            }
        }
        let append = start.elapsed();
        times.sort_unstable();
        let started = Instant::now();
        let through = store.head(&stream).expect("head");
        let mut cursor = EventCursor::beginning(stream.clone());
        let mut read = 0;
        loop {
            let page = store
                .read_page(&stream, &cursor, Some(&through), 1000)
                .expect("page");
            read += page.events.len();
            if let Some(last) = page.events.last() {
                cursor.sequence = last.sequence;
            }
            if !page.has_more {
                break;
            }
        }
        assert_eq!(read, count + 1);
        let read_time = started.elapsed();
        drop(store);
        let started = Instant::now();
        let reopened = open_event_store(directory.path(), backend).expect("reopen");
        assert_eq!(reopened.head(&stream).expect("reconstructed head"), through);
        let head_time = started.elapsed();
        // A new write forces SQLite's lazy validator reconstruction too.
        let started = Instant::now();
        let event = SessionEvent::typed(
            stream.clone(),
            EventData::SessionMetadataChanged {
                field: "title".into(),
                new_value_ref: StoredPayloadRef::from(&body),
            },
            EventActor::default(),
        )
        .expect("event");
        reopened
            .append_batch(AppendRequest {
                stream_id: stream.clone(),
                expected_head: through,
                idempotency_key: "measurement-reopen".into(),
                events: vec![event],
                payloads: vec![],
                erasure_reference_commitment: None,
            })
            .expect("reconstructed append");
        let disk = disk_bytes(directory.path());
        let rss = memory_kib("VmRSS:");
        let peak = memory_kib("VmHWM:");
        eprintln!(
            "backend={backend} disk_bytes={disk} rss_kib={rss:?} process_peak_rss_kib={peak:?} platform={} arch={}",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        eprintln!(
            "backend={backend} events={count} batch_events=1000 append={append:?} batch_p50_us={} batch_p95_us={} read={read_time:?} reopen_head={head_time:?} reopen_append={:?}",
            times[times.len() / 2],
            times[times.len() * 95 / 100],
            started.elapsed()
        );
    }
}

/// Same platform and public legacy APIs. This measures one message per write;
/// native batches include stronger durability and richer event semantics, so
/// these numbers describe costs and must not be presented as equal guarantees.
#[test]
#[ignore = "legacy storage baseline measurement"]
fn legacy_append_read_and_reopen_baseline() {
    use crate::session_backend::SessionBackend;
    use std::time::Instant;
    let count = std::env::var("ZEROCLAW_SESSION_SCALE_EVENTS")
        .ok()
        .map(|value| value.parse::<usize>().expect("count"))
        .unwrap_or(1_000_000);
    for backend in ["sqlite", "jsonl"] {
        let directory = tempfile::TempDir::new().expect("directory");
        let open = || -> Box<dyn SessionBackend> {
            match backend {
                "sqlite" => Box::new(
                    crate::session_sqlite::SqliteSessionBackend::new(directory.path())
                        .expect("SQLite"),
                ),
                _ => Box::new(
                    crate::session_store::SessionStore::new(directory.path()).expect("JSONL"),
                ),
            }
        };
        let store = open();
        let message = zeroclaw_api::model_provider::ChatMessage::user("fixture title");
        let started = Instant::now();
        let mut times = Vec::with_capacity(count);
        for _ in 0..count {
            let batch = Instant::now();
            store.append("baseline", &message).expect("append");
            times.push(batch.elapsed().as_micros());
        }
        let append = started.elapsed();
        times.sort_unstable();
        let started = Instant::now();
        let messages = store.try_load("baseline").expect("read");
        assert_eq!(messages.len(), count);
        let read = started.elapsed();
        let rss = memory_kib("VmRSS:");
        let peak = memory_kib("VmHWM:");
        drop(messages);
        drop(store);
        let started = Instant::now();
        let store = open();
        let reopen = started.elapsed();
        let started = Instant::now();
        store.append("baseline", &message).expect("reopen append");
        eprintln!(
            "legacy_backend={backend} messages={count} write_messages=1 append={append:?} append_p50_us={} append_p95_us={} read={read:?} reopen={reopen:?} reopen_append={:?} disk_bytes={} rss_kib={rss:?} process_peak_rss_kib={peak:?} platform={} arch={}",
            times[times.len() / 2],
            times[times.len() * 95 / 100],
            started.elapsed(),
            disk_bytes(directory.path()),
            std::env::consts::OS,
            std::env::consts::ARCH
        );
    }
}

#[test]
#[ignore = "large payload measurement"]
fn payload_read_peak_and_complete_content() {
    use std::time::Instant;
    for backend in ["sqlite", "jsonl"] {
        let directory = tempfile::TempDir::new().expect("directory");
        let store = open_event_store(directory.path(), backend).expect("store");
        let stream = SessionStreamId::new();
        store
            .append_batch(super::tests::request(&stream, "root", 0))
            .expect("root");
        let payload = PayloadRecord::new(
            PayloadId::new(),
            "session.bytes/v1".into(),
            vec![b'x'; 32 * 1024 * 1024],
        )
        .expect("payload");
        let id = payload.payload_id.clone();
        let event = SessionEvent::typed(
            stream.clone(),
            EventData::SessionMetadataChanged {
                field: "fixture".into(),
                new_value_ref: StoredPayloadRef::from(&payload),
            },
            EventActor::default(),
        )
        .expect("event");
        store
            .append_batch(AppendRequest {
                stream_id: stream.clone(),
                expected_head: store.head(&stream).expect("head"),
                idempotency_key: "payload".into(),
                events: vec![event],
                payloads: vec![payload],
                erasure_reference_commitment: None,
            })
            .expect("append");
        let before = memory_kib("VmRSS:");
        let started = Instant::now();
        let body = store.read_payload(&id).expect("read");
        assert!(matches!(&body,PayloadRead::Present(record) if record.body.len()==32*1024*1024));
        eprintln!(
            "payload_backend={backend} bytes={} read={:?} rss_before_kib={before:?} rss_after_kib={:?} process_peak_rss_kib={:?}",
            32 * 1024 * 1024,
            started.elapsed(),
            memory_kib("VmRSS:"),
            memory_kib("VmHWM:")
        );
    }
}
