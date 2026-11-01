use super::*;

impl SqliteEventStore {
    pub(in crate::session_events) fn export_native(
        &self,
        root: Option<&SessionStreamId>,
    ) -> Result<NativeImage, AppendError> {
        let conn = self.conn.lock();
        let tx = conn.unchecked_transaction().map_err(map_sqlite)?;
        let mut batches = Vec::new();
        {
            let mut statement=tx.prepare("SELECT stream_id,idempotency_key,request_commitment,first_seq,last_seq FROM session_append_receipts ORDER BY stream_id,first_seq").map_err(map_sqlite)?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, u64>(3)?,
                        row.get::<_, u64>(4)?,
                    ))
                })
                .map_err(map_sqlite)?;
            for row in rows {
                let (stream, key, commitment, first, last) = row.map_err(map_sqlite)?;
                let stream = SessionStreamId::parse(stream)?;
                Self::receipt_range(&tx, &stream, first, last)
                    .map_err(|error| AppendError::Corrupt(error.to_string()))?;
                let mut events=tx.prepare("SELECT event_json FROM session_events WHERE stream_id=?1 AND sequence BETWEEN ?2 AND ?3 ORDER BY sequence").map_err(map_sqlite)?;
                let raw = events
                    .query_map(params![stream.as_str(), first, last], |row| {
                        row.get::<_, Vec<u8>>(0)
                    })
                    .map_err(map_sqlite)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(map_sqlite)?;
                batches.push(NativeBatch {
                    stream,
                    key,
                    commitment: Digest::from(commitment),
                    raw,
                });
            }
        }
        let mut payloads = Vec::new();
        {
            let mut statement = tx
                .prepare("SELECT payload_id,codec FROM event_payloads ORDER BY payload_id")
                .map_err(map_sqlite)?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(map_sqlite)?;
            for row in rows {
                let (id, codec) = row.map_err(map_sqlite)?;
                // Codec ownership comes from the API registry. Content bodies
                // are hydrated only after selecting the migrating family.
                let body = if zeroclaw_api::session_events::payloads::content_leaf(&codec) {
                    None
                } else {
                    tx.query_row(
                        "SELECT body FROM event_payloads WHERE payload_id=?1",
                        params![id],
                        |row| row.get(0),
                    )
                    .map_err(map_sqlite)?
                };
                payloads.push(NativePayload {
                    reference: StoredPayloadRef {
                        payload_id: PayloadId::parse(id)?,
                        codec,
                    },
                    body,
                });
            }
        }
        let mut selected = select_family(NativeImage { batches, payloads }, root)?;
        for payload in &mut selected.payloads {
            if zeroclaw_api::session_events::payloads::content_leaf(&payload.reference.codec) {
                payload.body = tx
                    .query_row(
                        "SELECT body FROM event_payloads WHERE payload_id=?1",
                        params![payload.reference.payload_id.as_str()],
                        |row| row.get(0),
                    )
                    .map_err(map_sqlite)?;
            }
        }
        selected.validate()?;
        // No missing receipt may hide a committed tail from the transfer.
        let mut tails = BTreeMap::new();
        for batch in &selected.batches {
            for raw in &batch.raw {
                let event =
                    parse_event(raw).map_err(|error| AppendError::Corrupt(error.to_string()))?;
                tails.insert(event.stream_id, event.sequence);
            }
        }
        for (stream, tail) in tails {
            let head = Self::current_head(&tx, &stream)
                .map_err(|error| AppendError::Corrupt(error.to_string()))?;
            if head != tail {
                return Err(AppendError::Corrupt(
                    "migration receipt coverage gap".into(),
                ));
            }
        }
        Ok(selected)
    }

    pub(in crate::session_events) fn import_native(
        &self,
        image: &NativeImage,
    ) -> Result<(), AppendError> {
        image.validate()?;
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite)?;
        let mut heads = BTreeMap::new();
        for batch in &image.batches {
            for raw in &batch.raw {
                let event =
                    parse_event(raw).map_err(|error| AppendError::Corrupt(error.to_string()))?;
                let existing: Option<Vec<u8>> = tx
                    .query_row(
                        "SELECT event_json FROM session_events WHERE stream_id=?1 AND sequence=?2",
                        params![event.stream_id.as_str(), event.sequence],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(map_sqlite)?;
                if let Some(existing) = existing {
                    if existing != *raw {
                        return Err(AppendError::Corrupt(
                            "migration target event conflict".into(),
                        ));
                    }
                } else {
                    tx.execute("INSERT INTO session_events(stream_id,sequence,event_id,event_json) VALUES(?1,?2,?3,?4)",params![event.stream_id.as_str(),event.sequence,event.event_id.as_str(),raw]).map_err(map_sqlite)?;
                }
                heads.insert(event.stream_id.clone(), event.sequence);
            }
            let first = parse_event(
                batch
                    .raw
                    .first()
                    .ok_or_else(|| AppendError::Corrupt("empty migration batch".into()))?,
            )
            .map_err(|error| AppendError::Corrupt(error.to_string()))?
            .sequence;
            let last = parse_event(
                batch
                    .raw
                    .last()
                    .ok_or_else(|| AppendError::Corrupt("empty migration batch".into()))?,
            )
            .map_err(|error| AppendError::Corrupt(error.to_string()))?
            .sequence;
            let existing:Option<(String,u64,u64)>=tx.query_row("SELECT request_commitment,first_seq,last_seq FROM session_append_receipts WHERE stream_id=?1 AND idempotency_key=?2",params![batch.stream.as_str(),batch.key],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional().map_err(map_sqlite)?;
            if let Some(existing) = existing {
                if existing != (batch.commitment.as_str().to_owned(), first, last) {
                    return Err(AppendError::Corrupt(
                        "migration target receipt conflict".into(),
                    ));
                }
            } else {
                tx.execute("INSERT INTO session_append_receipts(stream_id,idempotency_key,request_commitment,first_seq,last_seq) VALUES(?1,?2,?3,?4,?5)",params![batch.stream.as_str(),batch.key,batch.commitment.as_str(),first,last]).map_err(map_sqlite)?;
            }
        }
        for payload in &image.payloads {
            let r = &payload.reference;
            let existing: Option<(String, Option<Vec<u8>>)> = tx
                .query_row(
                    "SELECT codec,body FROM event_payloads WHERE payload_id=?1",
                    params![r.payload_id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(map_sqlite)?;
            if let Some(existing) = existing {
                if existing != (r.codec.clone(), payload.body.clone()) {
                    return Err(AppendError::Corrupt(
                        "migration target payload conflict".into(),
                    ));
                }
            } else {
                tx.execute(
                    "INSERT INTO event_payloads(payload_id,codec,body) VALUES(?1,?2,?3)",
                    params![r.payload_id.as_str(), r.codec, payload.body],
                )
                .map_err(map_sqlite)?;
            }
        }
        for (stream, sequence) in heads {
            let tail: u64 = tx
                .query_row(
                    "SELECT MAX(sequence) FROM session_events WHERE stream_id=?1",
                    params![stream.as_str()],
                    |row| row.get(0),
                )
                .map_err(map_sqlite)?;
            if tail != sequence {
                return Err(AppendError::Corrupt(
                    "migration target has additional events".into(),
                ));
            }
        }
        tx.commit()
            .map_err(|error| AppendError::CommitOutcomeUnknown(error.to_string()))?;
        *self.validation.lock() = None;
        Ok(())
    }
}
