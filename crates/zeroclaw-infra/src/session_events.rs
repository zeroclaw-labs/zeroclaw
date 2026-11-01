//! Native SQLite and JSONL implementations of the append-only session-event
//! contract.  These backends do not know about live policy or transport
//! adapters; authorization and event admission belong to the coordinator.

use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::fs;
use std::path::Path;
use std::sync::Arc;
use zeroclaw_api::session_events::{
    AppendError, AppendReceipt, AppendRequest, ContractError, Digest, EventCursor, EventId,
    EventPage, MAX_BATCH_BYTES, PayloadId, PayloadRead, PayloadRecord, ReadError, SessionEvent,
    SessionEventStore, SessionStreamId,
};

const MAX_PAGE_EVENTS: usize = 10_000;

fn committed_event_bytes(used: usize, event_len: usize) -> Result<usize, AppendError> {
    // Include JSONL framing in both backends so a successful append always
    // respects the same committed batch budget.
    let next = used
        .checked_add(event_len)
        .and_then(|bytes| bytes.checked_add(1))
        .ok_or_else(|| ContractError::InvalidRequest("committed batch size overflow".into()))?;
    if next > MAX_BATCH_BYTES {
        return Err(
            ContractError::InvalidRequest("committed batch byte limit exceeded".into()).into(),
        );
    }
    Ok(next)
}

pub mod catalog;
pub mod search;

type SqlitePayloadRow = (String, String, Option<Vec<u8>>);

fn content_leaf_sql_list() -> String {
    zeroclaw_api::session_events::payloads::content_leaf_codecs()
        .map(|codec| format!("'{}'", codec.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",")
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn validate_page_limits(max_events: usize) -> Result<(), ReadError> {
    if max_events == 0 || max_events > MAX_PAGE_EVENTS {
        return Err(ReadError::TooLarge);
    }
    Ok(())
}

fn map_sqlite(error: rusqlite::Error) -> AppendError {
    AppendError::StorageUnavailable(error.to_string())
}

fn read_error(error: rusqlite::Error) -> ReadError {
    ReadError::StorageUnavailable(error.to_string())
}

fn parse_event(bytes: &[u8]) -> Result<SessionEvent, ReadError> {
    let event: SessionEvent =
        serde_json::from_slice(bytes).map_err(|error| ReadError::Corrupt(error.to_string()))?;
    if event.schema_version != 1 {
        return Err(ReadError::UnsupportedSchema(
            event.schema_version.to_string(),
        ));
    }
    event
        .validate_recorded()
        .map_err(|error| ReadError::Corrupt(error.to_string()))?;
    Ok(event)
}

/// A completed redaction must also purge old WAL pages. A concurrent raw
/// SQLite reader can delay truncation; report an uncertain acknowledgement
/// and retry the same receipt after that reader releases its snapshot.
fn checkpoint_erasure(conn: &Connection) -> Result<(), AppendError> {
    let (busy, _, _): (i64, i64, i64) = conn
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .map_err(|error| {
            AppendError::CommitOutcomeUnknown(format!("erasure checkpoint failed: {error}"))
        })?;
    if busy != 0 {
        return Err(AppendError::CommitOutcomeUnknown(
            "erasure checkpoint is held by a reader".into(),
        ));
    }
    Ok(())
}

fn event_page(
    stream_id: SessionStreamId,
    after: EventCursor,
    through: EventCursor,
    rows: Vec<(Vec<u8>, SessionEvent)>,
    max_events: usize,
    has_more: bool,
) -> Result<EventPage, ReadError> {
    let mut expected_sequence = after
        .sequence
        .checked_add(1)
        .ok_or_else(|| ReadError::Gap("cursor overflow".to_owned()))?;
    for (_, event) in &rows {
        if event.stream_id != stream_id || event.sequence != expected_sequence {
            return Err(ReadError::Gap(format!(
                "expected contiguous sequence {}, found {}",
                expected_sequence, event.sequence
            )));
        }
        expected_sequence = expected_sequence
            .checked_add(1)
            .ok_or_else(|| ReadError::Gap("cursor overflow".to_owned()))?;
    }
    if !has_more && expected_sequence != through.sequence.saturating_add(1) {
        return Err(ReadError::Gap(format!(
            "page ended at {}, expected {}",
            expected_sequence.saturating_sub(1),
            through.sequence
        )));
    }
    let mut events = Vec::with_capacity(rows.len().min(max_events));
    let mut raw_events = Vec::with_capacity(rows.len().min(max_events));
    for (raw, event) in rows.into_iter().take(max_events) {
        raw_events.push(raw);
        events.push(event);
    }
    Ok(EventPage {
        stream_id,
        after,
        through,
        events,
        raw_events,
        has_more,
    })
}

/// SQLite event backend. The legacy `sessions` tables remain owned by the
/// compatibility SessionBackend; this backend uses a separate database so it
/// cannot accidentally double-write the old transcript.
pub struct SqliteEventStore {
    conn: Mutex<Connection>,
    validation: Mutex<Option<ValidationSnapshot>>,
}

struct ValidationSnapshot {
    data_version: i64,
    history: zeroclaw_api::session_events::rules::HistoryIndex,
    payloads: zeroclaw_api::session_events::payloads::PayloadIndex,
}

impl SqliteEventStore {
    pub fn open(workspace_dir: &Path) -> Result<Self, AppendError> {
        let dir = workspace_dir.join("sessions");
        filesystem::directory(&dir)?;
        let path = dir.join("session-events.db");
        for suffix in ["-wal", "-shm"] {
            let sidecar = dir.join(format!("session-events.db{suffix}"));
            match fs::symlink_metadata(&sidecar) {
                Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
                Ok(_) => {
                    return Err(AppendError::StorageUnavailable(
                        "SQLite sidecar is not a regular file".into(),
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(jsonl::storage(error)),
            }
        }
        let _private_file = filesystem::file(&path, false).map_err(jsonl::storage)?;
        let conn = Connection::open(&path).map_err(map_sqlite)?;
        let schema_version: u32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(map_sqlite)?;
        if schema_version > 1 {
            return Err(AppendError::UnsupportedSchema(format!(
                "session SQLite schema {schema_version}"
            )));
        }
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA secure_delete=ON;",
        )
        .map_err(map_sqlite)?;
        if schema_version == 0 {
            conn.execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE IF NOT EXISTS session_events(
               stream_id TEXT NOT NULL,
               sequence INTEGER NOT NULL CHECK(sequence > 0),
               event_id TEXT NOT NULL UNIQUE,
               event_json BLOB NOT NULL,
               PRIMARY KEY(stream_id, sequence)
             );
             CREATE TABLE IF NOT EXISTS event_payloads(
               payload_id TEXT PRIMARY KEY,
               codec TEXT NOT NULL,
               body BLOB
             );
             CREATE TABLE IF NOT EXISTS session_append_receipts(
               stream_id TEXT NOT NULL,
               idempotency_key TEXT NOT NULL,
               request_commitment TEXT NOT NULL,
               first_seq INTEGER NOT NULL,
               last_seq INTEGER NOT NULL,
               PRIMARY KEY(stream_id, idempotency_key)
             );
             DROP INDEX IF EXISTS session_events_stream_seq;
             CREATE INDEX IF NOT EXISTS session_events_latest_by_branch_kind
               ON session_events(stream_id, json_extract(event_json, '$.branch_id'), json_extract(event_json, '$.kind'), sequence DESC);
             CREATE INDEX IF NOT EXISTS session_events_latest_by_turn_kind
               ON session_events(stream_id, json_extract(event_json, '$.branch_id'), json_extract(event_json, '$.turn_id'), json_extract(event_json, '$.kind'), sequence DESC);
             PRAGMA user_version=1;
             COMMIT;",
        )
        .map_err(map_sqlite)?;
        }
        // SQLite inherits the database mode for newly created sidecars; also
        // repair pre-existing files without following substituted links.
        for suffix in ["-wal", "-shm"] {
            let sidecar = dir.join(format!("session-events.db{suffix}"));
            if sidecar.try_exists().map_err(jsonl::storage)? {
                let file = filesystem::reader(&sidecar).map_err(jsonl::storage)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    file.set_permissions(fs::Permissions::from_mode(0o600))
                        .map_err(jsonl::storage)?;
                }
                #[cfg(not(unix))]
                drop(file);
            }
        }
        // A crash can happen after the redaction transaction commits but
        // before its WAL checkpoint. Reopening resumes physical cleanup from
        // canonical redacted bodies, without a second pending-status table.
        let has_redacted: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM event_payloads WHERE body IS NULL)",
                [],
                |row| row.get(0),
            )
            .map_err(map_sqlite)?;
        if has_redacted {
            checkpoint_erasure(&conn)?;
        }
        Ok(Self {
            conn: Mutex::new(conn),
            validation: Mutex::new(None),
        })
    }

    fn current_head(conn: &Connection, stream: &SessionStreamId) -> Result<u64, ReadError> {
        conn.query_row(
            "SELECT COALESCE(MAX(sequence), 0) FROM session_events WHERE stream_id=?1",
            params![stream.as_str()],
            |row| row.get(0),
        )
        .map_err(read_error)
    }

    fn receipt_range(
        conn: &Connection,
        stream: &SessionStreamId,
        first: u64,
        last: u64,
    ) -> Result<AppendReceipt, ReadError> {
        if first == 0 || last < first {
            return Err(ReadError::Corrupt("receipt range invalid".into()));
        }
        let head = Self::current_head(conn, stream)?;
        if last > head {
            return Err(ReadError::Corrupt("receipt beyond head".into()));
        }
        let mut stmt = conn.prepare("SELECT sequence,event_id,event_json FROM session_events WHERE stream_id=?1 AND sequence>=?2 AND sequence<=?3 ORDER BY sequence").map_err(read_error)?;
        let mut rows = stmt
            .query(params![stream.as_str(), first, last])
            .map_err(read_error)?;
        let mut ids = Vec::new();
        let mut expected = first;
        while let Some(row) = rows.next().map_err(read_error)? {
            let sequence: u64 = row.get(0).map_err(read_error)?;
            let id: String = row.get(1).map_err(read_error)?;
            let raw: Vec<u8> = row.get(2).map_err(read_error)?;
            let event = parse_event(&raw)?;
            if sequence != expected
                || event.sequence != sequence
                || event.event_id.as_str() != id
                || event.stream_id != *stream
            {
                return Err(ReadError::Corrupt("receipt event identity mismatch".into()));
            }
            ids.push(event.event_id);
            expected = expected
                .checked_add(1)
                .ok_or_else(|| ReadError::Corrupt("receipt overflow".into()))?;
        }
        if expected - 1 != last {
            return Err(ReadError::Gap("receipt range missing".into()));
        }
        Ok(AppendReceipt {
            first: EventCursor {
                stream_id: stream.clone(),
                sequence: first,
            },
            last: EventCursor {
                stream_id: stream.clone(),
                sequence: last,
            },
            event_ids: ids,
        })
    }

    fn receipt(
        conn: &Connection,
        request: &AppendRequest,
    ) -> Result<Option<AppendReceipt>, AppendError> {
        let row: Option<(String, u64, u64)> = conn
            .query_row(
                "SELECT request_commitment,first_seq,last_seq FROM session_append_receipts WHERE stream_id=?1 AND idempotency_key=?2",
                params![request.stream_id.as_str(), request.idempotency_key.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(map_sqlite)?;
        let Some((stored, first, last)) = row else {
            return Ok(None);
        };
        if stored != request.commitment()?.as_str() {
            return Err(AppendError::IdempotencyConflict(
                request.idempotency_key.clone(),
            ));
        }
        Self::receipt_range(conn, &request.stream_id, first, last)
            .map(Some)
            .map_err(|error| AppendError::Corrupt(error.to_string()))
    }
}

impl SessionEventStore for SqliteEventStore {
    fn read_latest_event(
        &self,
        stream: &SessionStreamId,
        branch: &zeroclaw_api::session_events::BranchId,
        turn: Option<&zeroclaw_api::session_events::TurnId>,
        kind: &zeroclaw_api::session_events::EventKind,
    ) -> Result<Option<SessionEvent>, ReadError> {
        let kind_tag =
            serde_json::to_value(kind).map_err(|error| ReadError::Corrupt(error.to_string()))?;
        let kind_tag = kind_tag
            .as_str()
            .ok_or_else(|| ReadError::Corrupt("event kind is not a string".into()))?;
        let id: Option<String> = {
            let conn = self.conn.lock();

            let common = "SELECT event_id FROM session_events WHERE stream_id=?1 AND json_extract(event_json, '$.branch_id')=?2 AND json_extract(event_json, '$.kind')=?3 AND json_extract(event_json, '$.body.codec_version')=1";
            if let Some(turn) = turn {
                conn.query_row(&format!("{common} AND json_extract(event_json, '$.turn_id')=?4 ORDER BY sequence DESC LIMIT 1"), params![stream.as_str(),branch.as_str(),kind_tag,turn.as_str()], |row|row.get(0)).optional().map_err(read_error)?
            } else {
                conn.query_row(
                    &format!("{common} ORDER BY sequence DESC LIMIT 1"),
                    params![stream.as_str(), branch.as_str(), kind_tag],
                    |row| row.get(0),
                )
                .optional()
                .map_err(read_error)?
            }
        };
        let Some(id) = id else {
            return Ok(None);
        };
        let id = EventId::parse(id).map_err(|error| ReadError::Corrupt(error.to_string()))?;
        let event = self
            .read_event(&id)?
            .ok_or_else(|| ReadError::Gap("latest event lookup missing".into()))?;
        if event.stream_id != *stream
            || event.branch_id.as_ref() != Some(branch)
            || turn.is_some_and(|turn| event.turn_id.as_ref() != Some(turn))
            || event.body.kind() != *kind
        {
            return Err(ReadError::Corrupt("latest event index mismatch".into()));
        }
        Ok(Some(event))
    }
    fn read_event(&self, event_id: &EventId) -> Result<Option<SessionEvent>, ReadError> {
        let position: Option<(String, u64)> = {
            let conn = self.conn.lock();
            conn.query_row(
                "SELECT stream_id,sequence FROM session_events WHERE event_id=?1",
                params![event_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(read_error)?
        };
        let Some((stream, sequence)) = position else {
            return Ok(None);
        };
        let stream = SessionStreamId::parse(&stream)
            .map_err(|error| ReadError::Corrupt(error.to_string()))?;
        let page = self.read_page(
            &stream,
            &EventCursor {
                stream_id: stream.clone(),
                sequence: sequence
                    .checked_sub(1)
                    .ok_or_else(|| ReadError::Corrupt("invalid event index sequence".into()))?,
            },
            Some(&EventCursor {
                stream_id: stream.clone(),
                sequence,
            }),
            1,
        )?;
        let event = page
            .events
            .into_iter()
            .next()
            .ok_or_else(|| ReadError::Gap("indexed event missing".into()))?;
        if event.event_id != *event_id {
            return Err(ReadError::Corrupt("event lookup index mismatch".into()));
        }
        Ok(Some(event))
    }
    fn append_batch(&self, request: AppendRequest) -> Result<AppendReceipt, AppendError> {
        request.validate()?;
        let commitment = request.commitment()?;
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite)?;
        if let Some(receipt) = Self::receipt(&tx, &request)? {
            let erased = request.events.iter().any(|event| {
                matches!(
                    event.body.data,
                    zeroclaw_api::session_events::EventData::PayloadRedacted { .. }
                )
            });
            drop(tx);
            if erased {
                checkpoint_erasure(&conn)?;
            }
            return Ok(receipt);
        }
        let head_seq = Self::current_head(&tx, &request.stream_id)
            .map_err(|e| AppendError::Corrupt(e.to_string()))?;
        if request.expected_head.sequence != head_seq {
            return Err(AppendError::HeadConflict {
                expected: request.expected_head.sequence,
                current: head_seq,
            });
        }
        let data_version: i64 = tx
            .query_row("PRAGMA data_version", [], |row| row.get(0))
            .map_err(map_sqlite)?;
        let mut cached = self.validation.lock();
        if cached
            .as_ref()
            .is_none_or(|cache| cache.data_version != data_version)
        {
            let history = Self::verified_history(&tx)
                .map_err(|error| AppendError::Corrupt(error.to_string()))?;
            let existing = {
                let leaf_codecs = content_leaf_sql_list();
                let query = format!(
                    "SELECT payload_id, codec, CASE WHEN codec IN ({leaf_codecs}) THEN NULL ELSE body END FROM event_payloads"
                );
                let mut statement = tx.prepare(&query).map_err(map_sqlite)?;
                let rows = statement
                    .query_map([], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<Vec<u8>>>(2)?,
                        ))
                    })
                    .map_err(map_sqlite)?;
                let mut out = Vec::new();
                for row in rows {
                    let (id, codec, body) = row.map_err(map_sqlite)?;
                    out.push(zeroclaw_api::session_events::payloads::StoredPayload {
                        reference: zeroclaw_api::session_events::StoredPayloadRef {
                            payload_id: PayloadId::from(id),
                            codec,
                        },
                        body,
                    });
                }
                out
            };
            let facts = zeroclaw_api::session_events::rules::HistoryIndex::from_history(&history)?;
            let payloads = zeroclaw_api::session_events::payloads::PayloadIndex::from_index(
                &facts,
                &history,
                &existing,
                &|id| {
                    tx.query_row(
                        "SELECT length(body) FROM event_payloads WHERE payload_id=?1",
                        params![id.as_str()],
                        |row| row.get(0),
                    )
                    .map_err(map_sqlite)
                },
            )?;
            *cached = Some(ValidationSnapshot {
                data_version,
                history: facts,
                payloads,
            });
        }
        let cache = cached
            .as_mut()
            .ok_or_else(|| AppendError::Corrupt("validation reconstruction missing".into()))?;
        let history_delta = cache.history.plan(&request)?;
        let payload_delta = cache.payloads.plan(&cache.history, &request)?;
        let deleting = payload_delta.deleting().to_vec();

        let first_seq = head_seq
            .checked_add(1)
            .ok_or_else(|| AppendError::Corrupt("sequence overflow".into()))?;
        let committed_at = now_millis();
        let mut committed_events = Vec::with_capacity(request.events.len());
        // Request validation already bounds this sum; final envelope growth
        // must fit the same budget as the supplied payload bodies.
        let mut committed_bytes: usize = request.payloads.iter().map(|body| body.body.len()).sum();
        for (offset, input) in request.events.iter().enumerate() {
            let mut event = input.clone();
            event.sequence = first_seq
                .checked_add(
                    u64::try_from(offset)
                        .map_err(|_| AppendError::Corrupt("sequence overflow".to_owned()))?,
                )
                .ok_or_else(|| AppendError::Corrupt("sequence overflow".into()))?;
            event.committed_at = committed_at;
            let raw = event.canonical_bytes()?;
            committed_bytes = committed_event_bytes(committed_bytes, raw.len())?;
            tx.execute(
                "INSERT INTO session_events(stream_id, sequence, event_id, event_json)
                 VALUES(?1, ?2, ?3, ?4)",
                params![
                    request.stream_id.as_str(),
                    i64::try_from(event.sequence).map_err(|_| AppendError::Corrupt(
                        "sequence exceeds SQLite integer".to_owned()
                    ))?,
                    event.event_id.as_str(),
                    raw,
                ],
            )
            .map_err(map_sqlite)?;
            committed_events.push(event);
        }
        for payload in &request.payloads {
            let existing: Option<(String, Option<Vec<u8>>)> = tx
                .query_row(
                    "SELECT codec, body FROM event_payloads WHERE payload_id=?1",
                    params![payload.payload_id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(map_sqlite)?;
            if let Some((codec, body)) = existing {
                if codec != payload.codec || body.as_deref() != Some(payload.body.as_slice()) {
                    return Err(AppendError::Contract(ContractError::PayloadConflict(
                        payload.payload_id.to_string(),
                    )));
                }
                continue;
            }
            tx.execute(
                "INSERT INTO event_payloads(payload_id, codec, body)
                 VALUES(?1, ?2, ?3)",
                params![payload.payload_id.as_str(), payload.codec, payload.body],
            )
            .map_err(map_sqlite)?;
        }
        let event_ids: Vec<EventId> = committed_events
            .iter()
            .map(|event| event.event_id.clone())
            .collect();
        let last_seq = first_seq
            .checked_add(
                u64::try_from(committed_events.len() - 1)
                    .map_err(|_| AppendError::Corrupt("sequence overflow".to_owned()))?,
            )
            .ok_or_else(|| AppendError::Corrupt("sequence overflow".into()))?;
        tx.execute("INSERT INTO session_append_receipts(stream_id,idempotency_key,request_commitment,first_seq,last_seq) VALUES(?1,?2,?3,?4,?5)", params![request.stream_id.as_str(),request.idempotency_key,commitment.as_str(),i64::try_from(first_seq).map_err(|_|AppendError::Corrupt("sequence overflow".to_owned()))?,i64::try_from(last_seq).map_err(|_|AppendError::Corrupt("sequence overflow".to_owned()))?]).map_err(map_sqlite)?;
        let erased = !deleting.is_empty();
        for id in deleting {
            tx.execute(
                "UPDATE event_payloads SET body=NULL WHERE payload_id=?1",
                params![id.as_str()],
            )
            .map_err(map_sqlite)?;
        }
        #[cfg(test)]
        crash_tests::crash_boundary("sqlite_before_commit");
        if let Err(error) = tx.commit() {
            *cached = None;
            return Err(AppendError::CommitOutcomeUnknown(error.to_string()));
        }
        cache.history.commit(history_delta, &committed_events);
        cache.payloads.commit(payload_delta);
        if erased {
            checkpoint_erasure(&conn)?;
        }
        #[cfg(test)]
        crash_tests::crash_boundary("sqlite_after_commit");
        Ok(AppendReceipt {
            first: EventCursor {
                stream_id: request.stream_id.clone(),
                sequence: first_seq,
            },
            last: EventCursor {
                stream_id: request.stream_id,
                sequence: last_seq,
            },
            event_ids,
        })
    }

    fn read_page(
        &self,
        stream_id: &SessionStreamId,
        after: &EventCursor,
        through: Option<&EventCursor>,
        max_events: usize,
    ) -> Result<EventPage, ReadError> {
        validate_page_limits(max_events)?;
        if &after.stream_id != stream_id {
            return Err(ReadError::Gap(
                "after cursor belongs to another stream".to_owned(),
            ));
        }
        let mut connection = self.conn.lock();
        let conn = connection.transaction().map_err(read_error)?;
        let head = Self::current_head(&conn, stream_id)?;
        let through_seq = through.map_or(head, |cursor| {
            if &cursor.stream_id == stream_id {
                cursor.sequence
            } else {
                u64::MAX
            }
        });
        if through_seq == u64::MAX || after.sequence > through_seq || through_seq > head {
            return Err(ReadError::Gap("invalid page cursor".to_owned()));
        }
        let mut statement = conn
            .prepare(
                "SELECT event_json,event_id,sequence FROM session_events
             WHERE stream_id=?1 AND sequence>?2 AND sequence<=?3 ORDER BY sequence LIMIT ?4",
            )
            .map_err(read_error)?;
        let mut mapped = statement
            .query(params![
                stream_id.as_str(),
                after.sequence,
                through_seq,
                max_events + 1
            ])
            .map_err(read_error)?;
        let mut rows = Vec::new();
        let mut has_more = false;
        while let Some(row) = mapped.next().map_err(read_error)? {
            if rows.len() == max_events {
                has_more = true;
                break;
            }
            let raw: Vec<u8> = row.get(0).map_err(read_error)?;
            let id: String = row.get(1).map_err(read_error)?;
            let sequence: u64 = row.get(2).map_err(read_error)?;
            let event = parse_event(&raw)?;
            if event.event_id.as_str() != id || event.sequence != sequence {
                return Err(ReadError::Corrupt(
                    "event index disagrees with envelope".into(),
                ));
            }
            rows.push((raw, event));
        }
        event_page(
            stream_id.clone(),
            after.clone(),
            EventCursor {
                stream_id: stream_id.clone(),
                sequence: through_seq,
            },
            rows,
            max_events,
            has_more,
        )
    }

    fn head(&self, stream_id: &SessionStreamId) -> Result<EventCursor, ReadError> {
        let mut connection = self.conn.lock();
        let conn = connection.transaction().map_err(read_error)?;
        let sequence = Self::current_head(&conn, stream_id)?;
        Ok(EventCursor {
            stream_id: stream_id.clone(),
            sequence,
        })
    }

    fn lookup_receipt(
        &self,
        stream: &SessionStreamId,
        key: &str,
    ) -> Result<Option<AppendReceipt>, ReadError> {
        let mut connection = self.conn.lock();
        let conn = connection.transaction().map_err(read_error)?;
        let range:Option<(i64,i64)>=conn.query_row("SELECT first_seq,last_seq FROM session_append_receipts WHERE stream_id=?1 AND idempotency_key=?2",params![stream.as_str(),key],|row|Ok((row.get(0)?,row.get(1)?))).optional().map_err(read_error)?;
        let Some((first, last)) = range else {
            return Ok(None);
        };
        let first = u64::try_from(first)
            .map_err(|_| ReadError::Corrupt("receipt sequence overflow".into()))?;
        let last = u64::try_from(last)
            .map_err(|_| ReadError::Corrupt("receipt sequence overflow".into()))?;
        Self::receipt_range(&conn, stream, first, last).map(Some)
    }

    fn erasure_scope(
        &self,
        targets: &[zeroclaw_api::session_events::StoredPayloadRef],
    ) -> Result<(Vec<SessionStreamId>, Digest), AppendError> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction().map_err(map_sqlite)?;
        let history =
            Self::verified_history(&tx).map_err(|error| AppendError::Corrupt(error.to_string()))?;
        let leaf_codecs = content_leaf_sql_list();
        let query = format!(
            "SELECT payload_id,codec,CASE WHEN codec IN ({leaf_codecs}) THEN NULL ELSE body END FROM event_payloads"
        );
        let mut stmt = tx.prepare(&query).map_err(map_sqlite)?;
        let mut payloads = Vec::new();
        for row in stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<Vec<u8>>>(2)?,
                ))
            })
            .map_err(map_sqlite)?
        {
            let (id, codec, body) = row.map_err(map_sqlite)?;
            payloads.push(zeroclaw_api::session_events::payloads::StoredPayload {
                reference: zeroclaw_api::session_events::StoredPayloadRef {
                    payload_id: id.into(),
                    codec,
                },
                body,
            });
        }
        zeroclaw_api::session_events::payloads::erasure_scope(&history, &payloads, targets)
    }

    fn read_payload(&self, payload_id: &PayloadId) -> Result<PayloadRead, ReadError> {
        let mut connection = self.conn.lock();
        let conn = connection.transaction().map_err(read_error)?;
        let row: Option<SqlitePayloadRow> = conn
            .query_row(
                "SELECT payload_id, codec, body FROM event_payloads WHERE payload_id=?1",
                params![payload_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(read_error)?;
        let Some((id, codec, body)) = row else {
            return Err(ReadError::PayloadMissing(payload_id.to_string()));
        };
        let history = Self::verified_history(&conn)?;
        use zeroclaw_api::session_events::payloads::{PayloadDisposition, disposition};
        match disposition(&history, payload_id) {
            PayloadDisposition::ErasurePending => return Ok(PayloadRead::ErasurePending),
            PayloadDisposition::Redacted => return Ok(PayloadRead::Redacted),
            PayloadDisposition::Present => {}
        }
        let body = body.ok_or_else(|| {
            ReadError::Corrupt("body missing without erasure evidence".to_owned())
        })?;
        let record = PayloadRecord {
            payload_id: PayloadId::from(id),
            codec,
            body,
        };
        record
            .validate()
            .map_err(|error| ReadError::Corrupt(error.to_string()))?;
        Ok(PayloadRead::Present(record))
    }
}

pub mod cli_state;
pub mod family;
mod filesystem;
pub mod import;
pub mod installation;
mod jsonl;
pub mod migration;
pub mod ownership;
mod sqlite_history;
pub mod upgrade;
pub use jsonl::JsonlEventStore;

/// Construct the selected native event backend.  This is deliberately separate
/// from the native installation backend; legacy ChatMessage adapters are kept
/// only for migration compatibility and focused regression fixtures.
pub fn open_event_store(
    workspace_dir: &Path,
    backend: &str,
) -> Result<Arc<dyn SessionEventStore>, AppendError> {
    match backend {
        "sqlite" => Ok(Arc::new(SqliteEventStore::open(workspace_dir)?)),
        "jsonl" => Ok(Arc::new(JsonlEventStore::open(workspace_dir)?)),
        other => Err(AppendError::StorageUnavailable(format!(
            "unsupported event backend `{other}`"
        ))),
    }
}

#[cfg(test)]
mod crash_tests;

#[cfg(test)]
mod scale_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use zeroclaw_api::session_events::{EventActor, EventKind};

    #[test]
    fn latest_typed_lookup_skips_unsupported_body_codecs() {
        use zeroclaw_api::session_events::*;
        let dir = TempDir::new().expect("dir");
        let store = SqliteEventStore::open(dir.path()).expect("store");
        let stream = SessionStreamId::new();
        let mut batch = request(&stream, "fixture", 0);
        let branch = batch.events[0].branch_id.clone().expect("branch");
        let title = PayloadRecord::new(
            PayloadId::new(),
            "session.text/v1".into(),
            b"title".to_vec(),
        )
        .expect("title");
        for _ in 0..2 {
            let mut event = SessionEvent::typed(
                stream.clone(),
                EventData::SessionMetadataChanged {
                    field: "title".into(),
                    new_value_ref: StoredPayloadRef::from(&title),
                },
                EventActor::default(),
            )
            .expect("event");
            event.branch_id = Some(branch.clone());
            batch.events.push(event);
        }
        let previous = batch.events[1].event_id.clone();
        batch.payloads.push(title);
        store.append_batch(batch).expect("append");
        let conn =
            Connection::open(dir.path().join("sessions/session-events.db")).expect("connection");
        conn.execute("UPDATE session_events SET event_json=CAST(json_set(event_json, '$.body.codec_version', 9) AS BLOB) WHERE sequence=3", []).expect("future codec");
        let raw = store
            .read_page(&stream, &EventCursor::beginning(stream.clone()), None, 10)
            .expect("raw page");
        assert_eq!(raw.events[2].body.kind(), EventKind::Unknown);
        assert_eq!(raw.events[2].body.raw_kind(), "session_metadata_changed");
        assert_eq!(
            store
                .read_latest_event(&stream, &branch, None, &EventKind::SessionMetadataChanged)
                .expect("lookup")
                .expect("known event")
                .event_id,
            previous
        );
    }

    #[test]
    fn sqlite_body_projection_uses_shared_codec_classification() {
        use zeroclaw_api::session_events::payloads::{content_leaf, content_leaf_codecs};
        let conn = Connection::open_in_memory().expect("connection");
        let query = format!(
            "SELECT CASE WHEN ?1 IN ({}) THEN NULL ELSE ?2 END",
            content_leaf_sql_list()
        );
        for codec in content_leaf_codecs().chain([
            "session.content/v1",
            "session.future/v1",
            "session.text/v2",
        ]) {
            let body: Option<Vec<u8>> = conn
                .query_row(&query, params![codec, b"body".as_slice()], |row| row.get(0))
                .expect("body projection");
            assert_eq!(body.is_none(), content_leaf(codec), "{codec}");
        }
    }

    pub(super) fn request(stream: &SessionStreamId, key: &str, expected: u64) -> AppendRequest {
        let mut event = SessionEvent::new(
            stream.clone(),
            EventKind::SessionStarted,
            serde_json::json!({"session_id":"s","owning_agent":"test","capture_mode":"native"}),
            EventActor::default(),
        )
        .expect("event");
        event.branch_id = Some(zeroclaw_api::session_events::BranchId::new());
        AppendRequest {
            stream_id: stream.clone(),
            expected_head: EventCursor {
                stream_id: stream.clone(),
                sequence: expected,
            },
            idempotency_key: key.to_owned(),
            events: vec![event],
            payloads: Vec::new(),
            erasure_reference_commitment: None,
        }
    }

    fn exercise(store: &dyn SessionEventStore) {
        let stream = SessionStreamId::new();
        let first_request = request(&stream, "first", 0);
        let receipt = store
            .append_batch(first_request.clone())
            .expect("first append");
        assert_eq!(receipt.first.sequence, 1);
        let retry = store.append_batch(first_request).expect("idempotent retry");
        assert_eq!(retry, receipt);
        let error = store
            .append_batch(request(&stream, "second", 0))
            .expect_err("head conflict");
        assert!(matches!(error, AppendError::HeadConflict { .. }));
        let page = store
            .read_page(&stream, &EventCursor::beginning(stream.clone()), None, 10)
            .expect("page");
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.raw_events.len(), 1);
    }

    #[test]
    fn sqlite_append_is_idempotent_and_readable() {
        exercise(&SqliteEventStore::open(TempDir::new().expect("temp").path()).expect("sqlite"));
    }

    #[test]
    fn jsonl_append_is_idempotent_and_readable() {
        exercise(&JsonlEventStore::open(TempDir::new().expect("temp").path()).expect("jsonl"));
    }

    #[test]
    fn latest_event_lookup_preserves_stream_branch_and_turn_scope() {
        use zeroclaw_api::session_events::*;
        for backend in ["sqlite", "jsonl"] {
            let dir = TempDir::new().expect("temp");
            let store = open_event_store(dir.path(), backend).expect("store");
            let stream = SessionStreamId::new();
            let root = request(&stream, "root", 0);
            let branch = root.events[0].branch_id.clone().expect("branch");
            store.append_batch(root).expect("root");
            let mut expected = Vec::new();
            for index in 0..3 {
                let turn = TurnId::new();
                let content = PayloadRecord::json(
                    "session.content/v1",
                    &ContentPayload {
                        message_id: MessageId::new(),
                        parts: vec![],
                    },
                )
                .expect("content");
                let ingress = PayloadRecord::json(
                    "session.ingress/v1",
                    &serde_json::json!({"source":"fixture"}),
                )
                .expect("ingress");
                let mut event = SessionEvent::typed(
                    stream.clone(),
                    EventData::TurnAccepted {
                        input_ref: StoredPayloadRef::from(&content),
                        ingress_ref: StoredPayloadRef::from(&ingress),
                        reply_route_ref: None,
                    },
                    EventActor::default(),
                )
                .expect("event");
                event.branch_id = Some(branch.clone());
                event.turn_id = Some(turn.clone());
                expected.push((turn, event.event_id.clone()));
                store
                    .append_batch(AppendRequest {
                        stream_id: stream.clone(),
                        expected_head: store.head(&stream).expect("head"),
                        idempotency_key: format!("turn-{index}"),
                        events: vec![event],
                        payloads: vec![content, ingress],
                        erasure_reference_commitment: None,
                    })
                    .expect("append");
            }
            let latest = store
                .read_latest_event(&stream, &branch, None, &EventKind::TurnAccepted)
                .expect("lookup")
                .expect("latest");
            assert_eq!(latest.event_id, expected[2].1);
            for (turn, id) in expected {
                assert_eq!(
                    store
                        .read_latest_event(&stream, &branch, Some(&turn), &EventKind::TurnAccepted)
                        .expect("lookup")
                        .expect("turn")
                        .event_id,
                    id
                );
            }
            assert!(
                store
                    .read_latest_event(&stream, &BranchId::new(), None, &EventKind::TurnAccepted)
                    .expect("foreign branch")
                    .is_none()
            );
            assert!(
                store
                    .read_latest_event(
                        &SessionStreamId::new(),
                        &branch,
                        None,
                        &EventKind::TurnAccepted
                    )
                    .expect("foreign stream")
                    .is_none()
            );
            assert!(
                store
                    .read_latest_event(&stream, &branch, None, &EventKind::ToolEffectStarted)
                    .expect("absent kind")
                    .is_none()
            );
            if backend == "sqlite" {
                let conn = Connection::open(dir.path().join("sessions/session-events.db"))
                    .expect("connection");
                for (sql, index_name) in [
                    (
                        "EXPLAIN QUERY PLAN SELECT event_id FROM session_events WHERE stream_id='fixture' AND json_extract(event_json, '$.branch_id')='branch' AND json_extract(event_json, '$.kind')='turn_accepted' ORDER BY sequence DESC LIMIT 1",
                        "session_events_latest_by_branch_kind",
                    ),
                    (
                        "EXPLAIN QUERY PLAN SELECT event_id FROM session_events WHERE stream_id='fixture' AND json_extract(event_json, '$.branch_id')='branch' AND json_extract(event_json, '$.turn_id')='turn' AND json_extract(event_json, '$.kind')='turn_accepted' ORDER BY sequence DESC LIMIT 1",
                        "session_events_latest_by_turn_kind",
                    ),
                ] {
                    let plan: String = conn
                        .query_row(sql, [], |row| row.get(3))
                        .expect("query plan");
                    assert!(plan.contains(index_name), "{plan}");
                }
            }
        }
    }
}

#[cfg(test)]
mod conformance {
    use super::*;
    use tempfile::TempDir;
    use zeroclaw_api::session_events::payloads::{StoredPayload, erasure_scope};
    use zeroclaw_api::session_events::*;

    fn root(stream: &SessionStreamId) -> SessionEvent {
        let mut event = SessionEvent::typed(
            stream.clone(),
            EventData::SessionStarted {
                session_id: SessionId::new(),
                owning_agent: "fixture".into(),
                capture_mode: CaptureMode::Native,
            },
            EventActor::default(),
        )
        .expect("root");
        event.branch_id = Some(BranchId::new());
        event
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
    fn with_backends(test: impl Fn(&dyn SessionEventStore, &TempDir, &str)) {
        for backend in ["sqlite", "jsonl"] {
            let dir = TempDir::new().expect("dir");
            let store = open_event_store(dir.path(), backend).expect("store");
            test(&*store, &dir, backend);
        }
    }

    #[test]
    fn payload_reads_have_no_byte_budget() {
        with_backends(|store, dir, backend| {
            let stream = SessionStreamId::new();
            let payload = PayloadRecord::new(
                PayloadId::new(),
                "session.text/v1".into(),
                b"title".to_vec(),
            )
            .expect("payload");
            let event = SessionEvent::typed(
                stream.clone(),
                EventData::SessionMetadataChanged {
                    field: "title".into(),
                    new_value_ref: StoredPayloadRef::from(&payload),
                },
                EventActor::default(),
            )
            .expect("event");
            store
                .append_batch(batch(
                    store,
                    &stream,
                    vec![root(&stream), event],
                    vec![payload.clone()],
                ))
                .expect("append");
            // Expand the persisted leaf independently of the write-batch budget:
            // readers must return the full stored content without a size cap.
            let size = MAX_BATCH_BYTES + 1;
            if backend == "sqlite" {
                let conn = Connection::open(dir.path().join("sessions/session-events.db"))
                    .expect("connection");
                conn.execute(
                    "UPDATE event_payloads SET body=zeroblob(?1) WHERE payload_id=?2",
                    params![size, payload.payload_id.as_str()],
                )
                .expect("expand payload");
            } else {
                let path = dir
                    .path()
                    .join("sessions/session-events-jsonl/payloads")
                    .join(format!("{}.bin", payload.payload_id));
                fs::OpenOptions::new()
                    .write(true)
                    .open(path)
                    .expect("payload file")
                    .set_len(size as u64)
                    .expect("expand payload");
            }
            let PayloadRead::Present(record) =
                store.read_payload(&payload.payload_id).expect("read")
            else {
                panic!("present payload");
            };
            assert_eq!(record.body.len(), size);
        });
    }
    #[test]
    fn sqlite_reads_edited_event_without_a_content_hash() {
        let dir = TempDir::new().expect("dir");
        let store = SqliteEventStore::open(dir.path()).expect("store");
        let stream = SessionStreamId::new();
        store
            .append_batch(batch(&store, &stream, vec![root(&stream)], vec![]))
            .expect("append");
        let conn = Connection::open(dir.path().join("sessions/session-events.db")).expect("conn");
        let raw: Vec<u8> = conn
            .query_row("SELECT event_json FROM session_events", [], |row| {
                row.get(0)
            })
            .expect("raw");
        let mut value: serde_json::Value = serde_json::from_slice(&raw).expect("json");
        value["body"]["fields"]["owning_agent"] = serde_json::json!("edited");
        conn.execute(
            "UPDATE session_events SET event_json=?1",
            params![serde_json::to_vec(&value).expect("json")],
        )
        .expect("edit");
        let page = store
            .read_page(&stream, &EventCursor::beginning(stream.clone()), None, 10)
            .expect("page");
        assert!(
            matches!(&page.events[0].body.data, EventData::SessionStarted { owning_agent, .. } if owning_agent == "edited")
        );
    }

    #[test]
    fn count_limited_pages_return_every_committed_event() {
        with_backends(|store, _, _| {
            let stream = SessionStreamId::new();
            store
                .append_batch(batch(store, &stream, vec![root(&stream)], vec![]))
                .expect("root");
            for _ in 0..5 {
                let text = PayloadRecord::new(
                    PayloadId::new(),
                    "session.text/v1".into(),
                    b"title".to_vec(),
                )
                .expect("text");
                let event = SessionEvent::typed(
                    stream.clone(),
                    EventData::SessionMetadataChanged {
                        field: "title".into(),
                        new_value_ref: StoredPayloadRef::from(&text),
                    },
                    EventActor::default(),
                )
                .expect("metadata");
                store
                    .append_batch(batch(store, &stream, vec![event], vec![text]))
                    .expect("append");
            }
            let full = store
                .read_page(&stream, &EventCursor::beginning(stream.clone()), None, 10)
                .expect("full");
            for limit in [0, MAX_PAGE_EVENTS + 1] {
                assert!(matches!(
                    store.read_page(
                        &stream,
                        &EventCursor::beginning(stream.clone()),
                        None,
                        limit
                    ),
                    Err(ReadError::TooLarge)
                ));
            }
            let mut cursor = EventCursor::beginning(stream.clone());
            let mut seen = Vec::new();
            loop {
                let page = store
                    .read_page(&stream, &cursor, Some(&full.through), 2)
                    .expect("bounded page");
                assert!(page.events.len() <= 2);
                let last = page.events.last().expect("progress");
                cursor.sequence = last.sequence;
                seen.extend(page.events.iter().map(|event| event.event_id.clone()));
                if !page.has_more {
                    break;
                }
            }
            assert_eq!(
                seen,
                full.events
                    .iter()
                    .map(|event| event.event_id.clone())
                    .collect::<Vec<_>>()
            );
        });
    }

    #[test]
    fn committed_byte_limit_rejects_without_receipt_and_boundary_reopens() {
        fn sized(mut request: AppendRequest, target: usize) -> AppendRequest {
            let original: usize = request
                .events
                .iter()
                .map(|event| event.canonical_bytes().expect("event bytes").len())
                .sum();
            let text = match &mut request.events.first_mut().expect("root event").body.data {
                EventData::SessionStarted { owning_agent, .. } => owning_agent,
                _ => panic!("size fixture"),
            };
            text.extend(std::iter::repeat_n(
                'x',
                target.checked_sub(original).expect("padding"),
            ));
            request
        }
        for backend in ["sqlite", "jsonl"] {
            for (event_count, payload_bytes) in [(1, 0), (2, 0), (2, 1024)] {
                let directory = TempDir::new().expect("directory");
                let store = open_event_store(directory.path(), backend).expect("store");
                let stream = SessionStreamId::new();
                let mut events = vec![root(&stream)];
                let payloads = if payload_bytes == 0 {
                    vec![]
                } else {
                    vec![
                        PayloadRecord::new(
                            PayloadId::new(),
                            "session.text/v1".into(),
                            vec![b'x'; payload_bytes],
                        )
                        .expect("payload"),
                    ]
                };
                if event_count == 2 {
                    events.push(
                        SessionEvent::typed(
                            stream.clone(),
                            match payloads.first() {
                                Some(payload) => EventData::SessionMetadataChanged {
                                    field: "title".into(),
                                    new_value_ref: StoredPayloadRef::from(payload),
                                },
                                None => EventData::SessionClosed {
                                    reason_key: "fixture".into(),
                                },
                            },
                            EventActor::default(),
                        )
                        .expect("close"),
                    );
                }
                let template = batch(&*store, &stream, events, payloads);
                let original: usize = template
                    .events
                    .iter()
                    .map(|event| event.canonical_bytes().expect("event bytes").len())
                    .sum();
                let mut committed = template.events.clone();
                for (index, event) in committed.iter_mut().enumerate() {
                    event.sequence = index as u64 + 1;
                    event.committed_at = now_millis();
                }
                let framed: usize = committed
                    .iter()
                    .map(|event| event.canonical_bytes().expect("committed bytes").len() + 1)
                    .sum();
                let envelope_growth = framed - original;
                let oversized = sized(template.clone(), MAX_BATCH_BYTES - payload_bytes);
                oversized
                    .validate()
                    .expect("input fits the pre-commit budget");
                assert!(
                    matches!(
                        store.append_batch(oversized),
                        Err(AppendError::Contract(ContractError::InvalidRequest(_)))
                    ),
                    "{backend}: final metadata and framing must fit"
                );
                assert_eq!(store.head(&stream).expect("unchanged head").sequence, 0);
                assert!(
                    store
                        .lookup_receipt(&stream, &template.idempotency_key)
                        .expect("receipt lookup")
                        .is_none()
                );
                assert!(
                    store
                        .read_page(&stream, &EventCursor::beginning(stream.clone()), None, 10)
                        .expect("empty history")
                        .events
                        .is_empty()
                );

                let accepted = sized(
                    template.clone(),
                    MAX_BATCH_BYTES - payload_bytes - envelope_growth,
                );
                let receipt = store
                    .append_batch(accepted)
                    .expect("final framed batch fits exactly");
                assert_eq!(receipt.event_ids.len(), event_count);
                drop(store);
                let store =
                    open_event_store(directory.path(), backend).expect("reopen boundary batch");
                let page = store
                    .read_page(&stream, &EventCursor::beginning(stream.clone()), None, 10)
                    .expect("maximum-budget page remains readable");
                assert_eq!(page.events.len(), event_count);
                assert!(!page.has_more);
                assert_eq!(
                    page.raw_events.iter().map(Vec::len).sum::<usize>()
                        + event_count
                        + payload_bytes,
                    MAX_BATCH_BYTES
                );
                for payload in &template.payloads {
                    assert_eq!(
                        store.read_payload(&payload.payload_id).expect("payload"),
                        PayloadRead::Present(payload.clone())
                    );
                }
                assert_eq!(
                    store
                        .lookup_receipt(&stream, &template.idempotency_key)
                        .expect("reopened receipt"),
                    Some(receipt)
                );
            }
        }
    }

    #[test]
    fn erasure_freezes_reads_and_physically_removes_content_after_confirmation() {
        with_backends(|store, dir, backend| {
            let stream = SessionStreamId::new();
            let payload = PayloadRecord::new(
                PayloadId::new(),
                "session.text/v1".into(),
                b"private fixture content".to_vec(),
            )
            .expect("payload");
            let reference = StoredPayloadRef::from(&payload);
            let event = SessionEvent::typed(
                stream.clone(),
                EventData::SessionMetadataChanged {
                    field: "title".into(),
                    new_value_ref: reference.clone(),
                },
                EventActor::default(),
            )
            .expect("metadata");
            store
                .append_batch(batch(
                    store,
                    &stream,
                    vec![root(&stream), event],
                    vec![payload.clone()],
                ))
                .expect("create");
            let page = store
                .read_page(&stream, &EventCursor::beginning(stream.clone()), None, 100)
                .expect("page");
            let existing = vec![StoredPayload {
                reference: reference.clone(),
                body: Some(payload.body.clone()),
            }];
            let (_, commitment) =
                erasure_scope(&page.events, &existing, std::slice::from_ref(&reference))
                    .expect("scope");
            let intent = SessionEvent::typed(
                stream.clone(),
                EventData::PayloadErasureRequested {
                    targets: vec![reference.clone()],
                    reason_category: "operator_requested".into(),
                },
                EventActor::default(),
            )
            .expect("intent");
            let intent_id = intent.event_id.clone();
            let mut request = batch(store, &stream, vec![intent], vec![]);
            request.erasure_reference_commitment = Some(commitment);
            store.append_batch(request).expect("intent commit");
            assert_eq!(
                store.read_payload(&payload.payload_id).expect("pending"),
                PayloadRead::ErasurePending
            );
            let reused = SessionEvent::typed(
                stream.clone(),
                EventData::SessionMetadataChanged {
                    field: "title".into(),
                    new_value_ref: reference,
                },
                EventActor::default(),
            )
            .expect("reuse");
            assert!(
                store
                    .append_batch(batch(store, &stream, vec![reused], vec![]))
                    .is_err()
            );
            let confirm = SessionEvent::typed(
                stream.clone(),
                EventData::PayloadRedacted {
                    erasure_requested_event_id: intent_id,
                },
                EventActor::default(),
            )
            .expect("confirm");
            let request = batch(store, &stream, vec![confirm], vec![]);
            let receipt = store.append_batch(request.clone()).expect("redact");
            assert_eq!(store.append_batch(request).expect("retry"), receipt);
            assert_eq!(
                store.read_payload(&payload.payload_id).expect("redacted"),
                PayloadRead::Redacted
            );
            let reopened = open_event_store(dir.path(), backend).expect("reopen");
            assert_eq!(
                reopened
                    .read_payload(&payload.payload_id)
                    .expect("redacted after restart"),
                PayloadRead::Redacted
            );
            if backend == "jsonl" {
                assert!(
                    !dir.path()
                        .join("sessions/session-events-jsonl/payloads")
                        .join(format!("{}.bin", payload.payload_id))
                        .exists()
                );
            } else {
                for name in ["session-events.db", "session-events.db-wal"] {
                    let path = dir.path().join("sessions").join(name);
                    if path.exists() {
                        let bytes = fs::read(path).expect("physical database bytes");
                        assert!(
                            !bytes
                                .windows(payload.body.len())
                                .any(|window| window == payload.body),
                            "erased content remains in {name}"
                        );
                    }
                }
            }
        });
    }
    #[test]
    fn sqlite_erasure_retry_purges_wal_after_pinned_reader_releases() {
        let dir = TempDir::new().expect("directory");
        let store = SqliteEventStore::open(dir.path()).expect("store");
        let stream = SessionStreamId::new();
        let payload = PayloadRecord::new(
            PayloadId::new(),
            "session.text/v1".into(),
            b"unique fixture awaiting physical erasure".to_vec(),
        )
        .expect("payload");
        let reference = StoredPayloadRef::from(&payload);
        let metadata = SessionEvent::typed(
            stream.clone(),
            EventData::SessionMetadataChanged {
                field: "title".into(),
                new_value_ref: reference.clone(),
            },
            EventActor::default(),
        )
        .expect("metadata");
        store
            .append_batch(batch(
                &store,
                &stream,
                vec![root(&stream), metadata],
                vec![payload.clone()],
            ))
            .expect("seed");
        let page = store
            .read_page(&stream, &EventCursor::beginning(stream.clone()), None, 100)
            .expect("events");
        let (_, commitment) = erasure_scope(
            &page.events,
            &[StoredPayload {
                reference: reference.clone(),
                body: Some(payload.body.clone()),
            }],
            std::slice::from_ref(&reference),
        )
        .expect("scope");
        let intent = SessionEvent::typed(
            stream.clone(),
            EventData::PayloadErasureRequested {
                targets: vec![reference],
                reason_category: "operator_requested".into(),
            },
            EventActor::default(),
        )
        .expect("intent");
        let id = intent.event_id.clone();
        let mut request = batch(&store, &stream, vec![intent], vec![]);
        request.erasure_reference_commitment = Some(commitment);
        store.append_batch(request).expect("intent");
        let reader =
            Connection::open(dir.path().join("sessions/session-events.db")).expect("reader");
        reader
            .execute_batch("BEGIN; SELECT count(*) FROM sqlite_master;")
            .expect("pinned reader");
        let confirm = SessionEvent::typed(
            stream.clone(),
            EventData::PayloadRedacted {
                erasure_requested_event_id: id,
            },
            EventActor::default(),
        )
        .expect("confirm");
        let request = batch(&store, &stream, vec![confirm], vec![]);
        assert!(matches!(
            store.append_batch(request.clone()),
            Err(AppendError::CommitOutcomeUnknown(_))
        ));
        let receipt = store
            .lookup_receipt(&stream, &request.idempotency_key)
            .expect("receipt")
            .expect("committed");
        assert_eq!(
            store
                .read_payload(&payload.payload_id)
                .expect("logical redaction"),
            PayloadRead::Redacted
        );
        reader.execute_batch("ROLLBACK").expect("release snapshot");
        assert_eq!(store.append_batch(request).expect("retry cleanup"), receipt);
        for name in ["session-events.db", "session-events.db-wal"] {
            let path = dir.path().join("sessions").join(name);
            if path.exists() {
                assert!(
                    !fs::read(path)
                        .expect("physical bytes")
                        .windows(payload.body.len())
                        .any(|bytes| bytes == payload.body)
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn sqlite_refuses_symlink_sidecars_before_opening_database() {
        for suffix in ["-wal", "-shm"] {
            let dir = TempDir::new().expect("directory");
            fs::create_dir_all(dir.path().join("sessions")).expect("sessions");
            let victim = dir.path().join("fixture-victim");
            fs::write(&victim, b"untouched fixture").expect("victim");
            std::os::unix::fs::symlink(
                &victim,
                dir.path()
                    .join("sessions")
                    .join(format!("session-events.db{suffix}")),
            )
            .expect("symlink");
            assert!(SqliteEventStore::open(dir.path()).is_err());
            assert_eq!(fs::read(victim).expect("victim"), b"untouched fixture");
        }
    }

    #[test]
    fn changed_head_is_not_part_of_idempotency_identity() {
        with_backends(|store, _, _| {
            let stream = SessionStreamId::new();
            let mut request = batch(store, &stream, vec![root(&stream)], vec![]);
            let receipt = store.append_batch(request.clone()).expect("append");
            request.expected_head.sequence = receipt.last.sequence;
            assert_eq!(
                store.append_batch(request).expect("same frozen facts"),
                receipt
            );
        });
    }
    #[test]
    fn malformed_references_leave_no_events_or_payloads_visible() {
        with_backends(|store, _, _| {
            let stream = SessionStreamId::new();
            store
                .append_batch(batch(store, &stream, vec![root(&stream)], vec![]))
                .expect("root");
            let payload = PayloadRecord::new(
                PayloadId::new(),
                "session.text/v1".into(),
                b"fixture".to_vec(),
            )
            .expect("payload");
            let mut reference = StoredPayloadRef::from(&payload);
            reference.codec = "session.other/v1".into();
            let event = SessionEvent::typed(
                stream.clone(),
                EventData::SessionMetadataChanged {
                    field: "title".into(),
                    new_value_ref: reference,
                },
                EventActor::default(),
            )
            .expect("metadata");
            assert!(
                store
                    .append_batch(batch(store, &stream, vec![event], vec![payload.clone()]))
                    .is_err()
            );
            assert_eq!(store.head(&stream).expect("head").sequence, 1);
            assert!(matches!(
                store.read_payload(&payload.payload_id),
                Err(ReadError::PayloadMissing(_))
            ));
        });
    }
    #[test]
    fn contiguous_pagination_keeps_a_fixed_head() {
        with_backends(|store, _, _| {
            let stream = SessionStreamId::new();
            store
                .append_batch(batch(store, &stream, vec![root(&stream)], vec![]))
                .expect("root");
            for index in 0..5 {
                let title = PayloadRecord::new(
                    PayloadId::new(),
                    "session.text/v1".into(),
                    format!("title-{index}").into_bytes(),
                )
                .expect("title");
                let event = SessionEvent::typed(
                    stream.clone(),
                    EventData::SessionMetadataChanged {
                        field: "title".into(),
                        new_value_ref: StoredPayloadRef::from(&title),
                    },
                    EventActor::default(),
                )
                .expect("metadata");
                store
                    .append_batch(batch(store, &stream, vec![event], vec![title]))
                    .expect("metadata append");
            }
            let first = store
                .read_page(&stream, &EventCursor::beginning(stream.clone()), None, 1)
                .expect("page1");
            assert!(first.has_more);
            assert_eq!(first.through.sequence, 6);
            let closed = SessionEvent::typed(
                stream.clone(),
                EventData::SessionClosed {
                    reason_key: "fixture_closed".into(),
                },
                EventActor::default(),
            )
            .expect("close");
            store
                .append_batch(batch(store, &stream, vec![closed], vec![]))
                .expect("close");
            let mut after = EventCursor {
                stream_id: stream.clone(),
                sequence: 1,
            };
            let mut seen = vec![1];
            loop {
                let page = store
                    .read_page(&stream, &after, Some(&first.through), 2)
                    .expect("next page");
                seen.extend(page.events.iter().map(|event| event.sequence));
                after.sequence = page.events.last().expect("page event").sequence;
                if !page.has_more {
                    break;
                }
            }
            assert_eq!(seen, vec![1, 2, 3, 4, 5, 6]);
            assert_eq!(store.head(&stream).expect("new head").sequence, 7);
        });
    }
    #[test]
    fn jsonl_orphan_segments_are_invisible_but_committed_loss_is_corrupt() {
        let dir = TempDir::new().expect("dir");
        let store = JsonlEventStore::open(dir.path()).expect("store");
        let stream = SessionStreamId::new();
        store
            .append_batch(batch(&store, &stream, vec![root(&stream)], vec![]))
            .expect("root");
        let segments = dir.path().join("sessions/session-events-jsonl/segments");
        fs::write(segments.join("orphan.jsonl"), b"{incomplete orphan").expect("orphan");
        assert_eq!(store.head(&stream).expect("head").sequence, 1);
        let committed = fs::read_dir(&segments)
            .expect("segments")
            .map(|entry| entry.expect("entry").path())
            .find(|path| path.file_name().is_some_and(|name| name != "orphan.jsonl"))
            .expect("committed");
        fs::remove_file(committed).expect("delete committed");
        assert!(matches!(store.head(&stream), Err(ReadError::Corrupt(_))));
    }
    #[test]
    fn jsonl_independent_handles_serialize_same_head() {
        let dir = TempDir::new().expect("dir");
        let stream = SessionStreamId::new();
        let first = JsonlEventStore::open(dir.path()).expect("first");
        let second = JsonlEventStore::open(dir.path()).expect("second");
        let one = batch(&first, &stream, vec![root(&stream)], vec![]);
        let two = batch(&second, &stream, vec![root(&stream)], vec![]);
        let a = std::thread::spawn(move || first.append_batch(one));
        let b = std::thread::spawn(move || second.append_batch(two));
        let a = a.join().expect("join");
        let b = b.join().expect("join");
        assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
        assert!(
            matches!(a, Err(AppendError::HeadConflict { .. }))
                || matches!(b, Err(AppendError::HeadConflict { .. }))
        );
    }
}
