use super::*;

impl SqliteEventStore {
    // Derived erasure and admission decisions must use the same committed
    // chain evidence as paged event reads, within their caller's transaction.
    pub(super) fn verified_history(conn: &Connection) -> Result<Vec<SessionEvent>, ReadError> {
        let verify_tail = |stream: &SessionStreamId, sequence: u64, hash: &Digest| {
            let (head, expected_hash) = Self::verified_head(conn, stream)?;
            if head != sequence || expected_hash != *hash {
                return Err(ReadError::Corrupt("history tail digest mismatch".into()));
            }
            Ok(())
        };
        let mut statement = conn
            .prepare("SELECT stream_id,sequence,event_id,length(event_json),event_json FROM session_events ORDER BY stream_id,sequence")
            .map_err(read_error)?;
        let mut rows = statement.query([]).map_err(read_error)?;
        let mut current: Option<(SessionStreamId, u64, Digest)> = None;
        let mut stream_count = 0u64;
        let mut history = Vec::new();
        while let Some(row) = rows.next().map_err(read_error)? {
            let stream_id: String = row.get(0).map_err(read_error)?;
            let sequence: u64 = row.get(1).map_err(read_error)?;
            let event_id: String = row.get(2).map_err(read_error)?;
            let byte_len: usize = row.get(3).map_err(read_error)?;
            if byte_len > MAX_PAGE_BYTES {
                return Err(ReadError::Corrupt("event exceeds readable size".into()));
            }
            let raw: Vec<u8> = row.get(4).map_err(read_error)?;
            let event = parse_event(&raw)?;
            if current
                .as_ref()
                .is_none_or(|(stream, _, _)| stream.as_str() != stream_id)
            {
                if let Some((stream, last, hash)) = current.take() {
                    verify_tail(&stream, last, &hash)?;
                }
                let stream = SessionStreamId::parse(&stream_id)
                    .map_err(|error| ReadError::Corrupt(error.to_string()))?;
                current = Some((stream, 0, Digest::zero()));
                stream_count += 1;
            }
            let (stream, last, hash) = current
                .as_mut()
                .ok_or_else(|| ReadError::Corrupt("history stream missing".into()))?;
            let expected = last
                .checked_add(1)
                .ok_or_else(|| ReadError::Corrupt("history sequence overflow".into()))?;
            if sequence != expected
                || event.sequence != sequence
                || event.stream_id != *stream
                || event.event_id.as_str() != event_id
                || event.previous_hash != *hash
            {
                return Err(ReadError::Corrupt(
                    "history identity or chain mismatch".into(),
                ));
            }
            *last = sequence;
            *hash = hash_raw(hash, &raw);
            history.push(event);
        }
        if let Some((stream, sequence, hash)) = current {
            verify_tail(&stream, sequence, &hash)?;
        }
        let indexed_streams: u64 = conn
            .query_row(
                "SELECT COUNT(*) FROM session_event_stream_heads",
                [],
                |row| row.get(0),
            )
            .map_err(read_error)?;
        if indexed_streams != stream_count {
            return Err(ReadError::Corrupt("orphaned history head index".into()));
        }
        Ok(history)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::session_events::{EventActor, EventData, StoredPayloadRef};

    #[test]
    fn corrupt_erasure_history_cannot_hide_content_or_authorize_append() {
        for confirmed in [false, true] {
            for reopened in [false, true] {
                let directory = tempfile::TempDir::new().expect("directory");
                let mut store = SqliteEventStore::open(directory.path()).expect("store");
                let stream = SessionStreamId::new();
                let payload = PayloadRecord::new(
                    PayloadId::new(),
                    "session.text/v1".into(),
                    b"intact content".to_vec(),
                )
                .expect("payload");
                let reference = StoredPayloadRef::from(&payload);
                let mut request = super::super::tests::request(&stream, "fixture", 0);
                for _ in 0..3 {
                    request.events.push(
                        SessionEvent::typed(
                            stream.clone(),
                            EventData::SessionMetadataChanged {
                                field: "title".into(),
                                new_value_ref: reference.clone(),
                            },
                            EventActor::default(),
                        )
                        .expect("metadata"),
                    );
                }
                request.payloads.push(payload.clone());
                store.append_batch(request).expect("fixture committed");
                assert_eq!(
                    store.read_payload(&payload.payload_id).expect("content"),
                    PayloadRead::Present(payload)
                );
                store
                    .erasure_scope(std::slice::from_ref(&reference))
                    .expect("valid erasure scope");

                let mut next = super::super::tests::request(&stream, "after-tamper", 4);
                let other = PayloadRecord::new(
                    PayloadId::new(),
                    "session.text/v1".into(),
                    b"new content".to_vec(),
                )
                .expect("new payload");
                next.events[0] = SessionEvent::typed(
                    stream.clone(),
                    EventData::SessionMetadataChanged {
                        field: "title".into(),
                        new_value_ref: StoredPayloadRef::from(&other),
                    },
                    EventActor::default(),
                )
                .expect("new event");
                next.payloads.push(other);

                let external =
                    Connection::open(directory.path().join("sessions/session-events.db"))
                        .expect("external connection");
                let raw: Vec<u8> = external
                    .query_row(
                        "SELECT event_json FROM session_events WHERE stream_id=?1 AND sequence=2",
                        params![stream.as_str()],
                        |row| row.get(0),
                    )
                    .expect("middle event");
                let mut intent = parse_event(&raw).expect("valid event");
                intent.body.data = EventData::PayloadErasureRequested {
                    targets: vec![reference.clone()],
                    reason_category: "forged".into(),
                };
                external
                    .execute(
                        "UPDATE session_events SET event_json=?1 WHERE stream_id=?2 AND sequence=2",
                        params![
                            intent.canonical_bytes().expect("forged intent"),
                            stream.as_str()
                        ],
                    )
                    .expect("tamper middle event");
                if confirmed {
                    let raw: Vec<u8> = external
                        .query_row(
                            "SELECT event_json FROM session_events WHERE stream_id=?1 AND sequence=3",
                            params![stream.as_str()],
                            |row| row.get(0),
                        )
                        .expect("next middle event");
                    let mut confirmation = parse_event(&raw).expect("valid event");
                    confirmation.body.data = EventData::PayloadRedacted {
                        erasure_requested_event_id: intent.event_id,
                    };
                    external
                        .execute(
                            "UPDATE session_events SET event_json=?1 WHERE stream_id=?2 AND sequence=3",
                            params![confirmation.canonical_bytes().expect("forged confirmation"), stream.as_str()],
                        )
                        .expect("tamper confirmation");
                }
                drop(external);
                if reopened {
                    drop(store);
                    store = SqliteEventStore::open(directory.path()).expect("reopen");
                }

                assert!(matches!(
                    store.read_page(
                        &stream,
                        &EventCursor::beginning(stream.clone()),
                        None,
                        10,
                        MAX_PAGE_BYTES,
                    ),
                    Err(ReadError::Corrupt(_))
                ));
                assert!(matches!(
                    store.read_payload(&reference.payload_id),
                    Err(ReadError::Corrupt(_))
                ));
                assert!(matches!(
                    store.erasure_scope(std::slice::from_ref(&reference)),
                    Err(AppendError::Corrupt(_))
                ));
                assert!(matches!(
                    store.append_batch(next),
                    Err(AppendError::Corrupt(_))
                ));
                assert_eq!(store.head(&stream).expect("unchanged tail").sequence, 4);
                assert!(
                    store
                        .lookup_receipt(&stream, "after-tamper")
                        .expect("no new receipt")
                        .is_none()
                );
            }
        }
    }
}
