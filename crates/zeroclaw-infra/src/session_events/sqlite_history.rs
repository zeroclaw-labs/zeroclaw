use super::*;

impl SqliteEventStore {
    // Rebuild the rule index from committed rows. Recovery relies on the
    // transaction boundary and contiguous sequence numbers, not a hash chain.
    pub(super) fn verified_history(conn: &Connection) -> Result<Vec<SessionEvent>, ReadError> {
        let mut statement = conn
            .prepare("SELECT session_id,sequence,event_id,event_json FROM session_events ORDER BY session_id,sequence")
            .map_err(read_error)?;
        let mut rows = statement.query([]).map_err(read_error)?;
        let mut history = Vec::new();
        let mut current: Option<(String, u64)> = None;
        while let Some(row) = rows.next().map_err(read_error)? {
            let session_id: String = row.get(0).map_err(read_error)?;
            let sequence: u64 = row.get(1).map_err(read_error)?;
            let event_id: String = row.get(2).map_err(read_error)?;
            let raw: Vec<u8> = row.get(3).map_err(read_error)?;
            let event = parse_event(&raw)?;
            if event.session_id.as_str() != session_id
                || event.sequence != sequence
                || event.event_id.as_str() != event_id
                || sequence
                    != current
                        .as_ref()
                        .filter(|(stream, _)| stream == &session_id)
                        .map_or(1, |(_, last)| last.saturating_add(1))
            {
                return Err(ReadError::Corrupt("event index or sequence gap".into()));
            }
            current = Some((session_id, sequence));
            history.push(event);
        }
        Ok(history)
    }
}
