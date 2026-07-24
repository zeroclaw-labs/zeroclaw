//! Paginated stream reader for the JSONL log file.

use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::event::LogEvent;

#[derive(Debug, Clone, Default)]
pub struct LogFilter {
    /// RFC 3339 lower bound (inclusive).
    pub since_ts: Option<String>,
    /// RFC 3339 upper bound (exclusive — used by pagination cursor).
    pub until_ts: Option<String>,
    /// Match against the cursor's id when `until_ts` ties.
    pub until_id: Option<String>,
    pub until_line_offset: Option<u64>,
    /// Match exact event.action (case-insensitive).
    pub action: Option<String>,
    /// Match exact event.category (case-insensitive).
    pub category: Option<String>,
    /// Match exact event.outcome (case-insensitive).
    pub outcome: Option<String>,
    /// Minimum severity_number.
    pub severity_min: Option<u8>,
    /// Match exact trace_id.
    pub trace_id: Option<String>,
    /// Substring search across message + attributes.
    pub q: Option<String>,
    /// Hide events with event.category == "internal" by default.
    pub hide_internal: bool,
    /// Per-attribution-field exact-match constraints. Key is any
    /// `zeroclaw.*` attribution name. Empty map = no attribution filter.
    pub field_eq: BTreeMap<String, String>,
}

/// One page returned by [`load_page`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogPage {
    pub events: Vec<LogEvent>,
    #[deprecated(
        since = "0.8.0",
        note = "tie-breaks by lexicographic id and can silently drop events; \
                use `next_cursor_line_offset` / `until_line_offset` instead. \
                Removal tracked in zeroclaw-labs/zeroclaw#8012."
    )]
    pub next_cursor: Option<(String, String)>,
    /// Byte offset past the OLDEST event on this page (the event in
    /// file order that is earliest among this page's matches). Pass
    /// back as [`LogFilter::until_line_offset`] on the next request to
    /// walk older pages. `None` when the page is empty.
    pub next_cursor_line_offset: Option<u64>,
    /// True when the file was fully scanned. UI uses this to disable
    /// "load older" affordances.
    pub at_end: bool,
}

#[allow(deprecated)] // we still populate `next_cursor` for backwards compat
pub fn load_page(path: &Path, filter: &LogFilter, limit: usize) -> Result<LogPage> {
    let limit = limit.clamp(1, 10_000);

    if !path.exists() {
        return Ok(LogPage {
            events: Vec::new(),
            next_cursor: None,
            next_cursor_line_offset: None,
            at_end: true,
        });
    }

    let file = File::open(path).with_context(|| format!("opening log: {}", path.display()))?;
    let mut reader = BufReader::new(file);

    let mut window: VecDeque<(LogEvent, u64)> = VecDeque::with_capacity(limit + 1);
    let needle = filter.q.as_deref().map(|s| s.to_ascii_lowercase());
    // `dropped_older` records whether we ever pushed past `limit` and
    // had to evict the oldest matching event. If false at the end, every
    // matching event in the file is in `window` — meaning there are no
    // older results the caller could page back to.
    let mut dropped_older = false;
    // `stopped_early` records whether we bailed out of the scan because
    // we hit the caller's `until_line_offset` cap. When true, there are
    // older events past the cursor and we must report `at_end = false`.
    let mut stopped_early = false;
    // Cap on `line_byte_end`. Lines whose end reaches or exceeds this
    // byte offset belong to a newer page (or are uninteresting partial
    // reads at file end) and stop the scan. `None` means "scan the
    // entire file".
    let until_line_offset = filter.until_line_offset;
    // Running byte offset of the next line we'll read. Starts at 0.
    // We track it manually instead of using `reader.stream_position()`
    // because that method interacts poorly with the `BufReader` borrow
    // we already hold.
    let mut next_byte_offset: u64 = 0;

    let mut buf = String::new();
    loop {
        buf.clear();
        let bytes_read = reader.read_line(&mut buf).context("reading log line")?;
        if bytes_read == 0 {
            break;
        }
        let line_byte_end = next_byte_offset + bytes_read as u64;

        // Stop scanning as soon as we cross the caller's cursor. This
        // is checked BEFORE parsing so we never even attempt to decode
        // JSON for lines that belong to a newer page.
        if let Some(cap) = until_line_offset
            && line_byte_end >= cap
        {
            stopped_early = true;
            break;
        }

        let trimmed = buf.trim();
        next_byte_offset = line_byte_end;

        if trimmed.is_empty() {
            continue;
        }

        let event: LogEvent = match serde_json::from_str(trimmed) {
            Ok(event) => event,
            Err(err) => {
                tracing::trace!(
                    target: "zeroclaw_log",
                    error = ?err,
                    "log: skipping malformed JSONL line"
                );
                continue;
            }
        };

        if !matches_filter(&event, filter, needle.as_deref()) {
            continue;
        }

        window.push_back((event, line_byte_end));
        if window.len() > limit {
            window.pop_front();
            dropped_older = true;
        }
    }

    // The byte-offset cursor must point at the OLDEST event currently
    // in the window — that's the event a follow-up page would resume
    // from in file order. We snapshot its offset before stripping the
    // offsets out of the deque below.
    let oldest_line_offset = window.front().map(|(_, end)| *end);

    let mut events: Vec<LogEvent> = window.into_iter().map(|(e, _)| e).collect();
    // Reverse so newest is first.
    events.reverse();

    // next_cursor is the OLDEST event in the page (the last one in
    // newest-first ordering = events.last()). Caller uses it as
    // `until_ts` / `until_id` for the next "load older" request when
    // they haven't upgraded to byte-offset cursors yet.
    let next_cursor = events.last().map(|e| (e.timestamp.clone(), e.id.clone()));

    let at_end = !dropped_older && !stopped_early || events.is_empty();

    Ok(LogPage {
        events,
        next_cursor,
        next_cursor_line_offset: oldest_line_offset,
        at_end,
    })
}

fn matches_filter(event: &LogEvent, filter: &LogFilter, needle: Option<&str>) -> bool {
    if filter.hide_internal && event.event.category == "internal" {
        return false;
    }
    if let Some(ref since) = filter.since_ts
        && event.timestamp.as_str() < since.as_str()
    {
        return false;
    }
    if let Some(ref until) = filter.until_ts {
        // Cursor pagination: include events strictly older than the
        // cursor. If the timestamps tie, fall back to id ordering for
        // deterministic pagination.
        match event.timestamp.as_str().cmp(until.as_str()) {
            std::cmp::Ordering::Greater => return false,
            std::cmp::Ordering::Equal => {
                if let Some(ref until_id) = filter.until_id
                    && event.id.as_str() >= until_id.as_str()
                {
                    return false;
                }
            }
            std::cmp::Ordering::Less => {}
        }
    }
    if let Some(ref action) = filter.action
        && !event.event.action.eq_ignore_ascii_case(action)
    {
        return false;
    }
    if let Some(ref category) = filter.category
        && !event.event.category.eq_ignore_ascii_case(category)
    {
        return false;
    }
    if let Some(ref outcome) = filter.outcome
        && !event.event.outcome.eq_ignore_ascii_case(outcome)
    {
        return false;
    }
    if let Some(min) = filter.severity_min
        && event.severity_number < min
    {
        return false;
    }
    for (key, want) in &filter.field_eq {
        if event.zeroclaw.get(key) != Some(want.as_str()) {
            return false;
        }
    }
    if let Some(ref tid) = filter.trace_id
        && event.trace_id.as_deref() != Some(tid.as_str())
    {
        return false;
    }
    if let Some(n) = needle {
        let hay_msg = event.message.as_deref().unwrap_or("").to_ascii_lowercase();
        let hay_attrs = event.attributes.to_string().to_ascii_lowercase();
        if !hay_msg.contains(n) && !hay_attrs.contains(n) {
            return false;
        }
    }
    true
}

/// Find a single event by id. Scans the file backwards from the end.
pub fn find_event_by_id(path: &Path, id: &str) -> Result<Option<LogEvent>> {
    if !path.exists() {
        return Ok(None);
    }
    let file = File::open(path).with_context(|| format!("opening log: {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut found: Option<LogEvent> = None;
    for line in reader.lines() {
        let line = line.context("reading log line")?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<LogEvent>(trimmed)
            && event.id == id
        {
            found = Some(event); // Don't break — last write wins for duplicate ids.
        }
    }
    Ok(found)
}

/// Helper for the gateway: the path the writer is configured to use.
#[must_use]
pub fn current_log_path() -> Option<PathBuf> {
    crate::writer::runtime_trace_path()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventCategory, Severity};
    use std::io::Write;

    fn write_jsonl(path: &Path, events: &[LogEvent]) {
        let mut file = std::fs::File::create(path).unwrap();
        for event in events {
            let line = serde_json::to_string(event).unwrap();
            file.write_all(line.as_bytes()).unwrap();
            file.write_all(b"\n").unwrap();
        }
    }

    fn make_event(action: &str, agent: Option<&str>) -> LogEvent {
        let mut event = LogEvent::new(Severity::Info, action, EventCategory::Agent);
        if let Some(alias) = agent {
            event.zeroclaw.set("agent_alias", alias);
        }
        event
    }

    #[test]
    fn empty_file_returns_at_end() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace.jsonl");
        let page = load_page(&path, &LogFilter::default(), 10).unwrap();
        assert!(page.events.is_empty());
        assert!(page.at_end);
    }

    #[test]
    fn returns_newest_first_within_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace.jsonl");
        let mut events = Vec::new();
        for index in 0..5 {
            let mut event = make_event("test", None);
            // Force monotonically increasing timestamp.
            event.timestamp = format!("2026-05-15T19:00:0{index}.000Z");
            event.message = Some(format!("event-{index}"));
            events.push(event);
        }
        write_jsonl(&path, &events);

        let page = load_page(&path, &LogFilter::default(), 3).unwrap();
        assert_eq!(page.events.len(), 3);
        assert_eq!(page.events[0].message.as_deref(), Some("event-4"));
        assert_eq!(page.events[1].message.as_deref(), Some("event-3"));
        assert_eq!(page.events[2].message.as_deref(), Some("event-2"));
        assert!(!page.at_end);
    }

    #[test]
    fn filter_by_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace.jsonl");
        let events = vec![
            make_event("a", Some("clamps")),
            make_event("b", Some("glados")),
            make_event("c", Some("clamps")),
        ];
        write_jsonl(&path, &events);

        let mut field_eq = BTreeMap::new();
        field_eq.insert("agent_alias".into(), "clamps".into());
        let filter = LogFilter {
            field_eq,
            ..Default::default()
        };
        let page = load_page(&path, &filter, 10).unwrap();
        assert_eq!(page.events.len(), 2);
    }

    #[test]
    fn filter_by_native_trace_id() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace.jsonl");
        let mut a = make_event("a", None);
        a.trace_id = Some("turn-1".into());
        let mut b = make_event("b", None);
        b.trace_id = Some("turn-2".into());
        let mut c = make_event("c", None);
        c.trace_id = Some("turn-1".into());
        write_jsonl(&path, &[a, b, c]);

        // The exact turn matches its two rows...
        let filter = LogFilter {
            trace_id: Some("turn-1".into()),
            ..Default::default()
        };
        assert_eq!(load_page(&path, &filter, 10).unwrap().events.len(), 2);

        // ...and an unknown id matches nothing (the bug this fixes: before the
        // layer promotion the native field was always None, so this returned 0
        // for EVERY id, including real ones).
        let filter = LogFilter {
            trace_id: Some("turn-missing".into()),
            ..Default::default()
        };
        assert_eq!(load_page(&path, &filter, 10).unwrap().events.len(), 0);
    }

    #[test]
    fn hide_internal_drops_internal_category() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace.jsonl");
        let mut agent_event = make_event("a", None);
        agent_event.event.category = "agent".into();
        let mut internal_event = make_event("b", None);
        internal_event.event.category = "internal".into();
        write_jsonl(&path, &[agent_event, internal_event]);

        let filter = LogFilter {
            hide_internal: true,
            ..Default::default()
        };
        let page = load_page(&path, &filter, 10).unwrap();
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.events[0].event.action, "a");
    }

    #[test]
    fn substring_query_matches_message_and_attributes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace.jsonl");
        let mut with_alpha_message = make_event("a", None);
        with_alpha_message.message = Some("alpha bravo".into());
        let mut with_attr_payload = make_event("b", None);
        with_attr_payload.attributes = serde_json::json!({ "k": "delta echo" });
        let mut with_foxtrot_message = make_event("c", None);
        with_foxtrot_message.message = Some("foxtrot".into());
        write_jsonl(
            &path,
            &[with_alpha_message, with_attr_payload, with_foxtrot_message],
        );

        let filter = LogFilter {
            q: Some("bravo".into()),
            ..Default::default()
        };
        let page = load_page(&path, &filter, 10).unwrap();
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.events[0].event.action, "a");

        let attr_filter = LogFilter {
            q: Some("delta".into()),
            ..Default::default()
        };
        let attr_page = load_page(&path, &attr_filter, 10).unwrap();
        assert_eq!(attr_page.events.len(), 1);
        assert_eq!(attr_page.events[0].event.action, "b");
    }

    #[test]
    #[allow(deprecated)] // legacy cursor is the subject under test
    fn cursor_pagination_returns_older_pages() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace.jsonl");
        let mut events = Vec::new();
        for index in 0..6 {
            let mut event = make_event("test", None);
            event.timestamp = format!("2026-05-15T19:00:0{index}.000Z");
            event.message = Some(format!("event-{index}"));
            events.push(event);
        }
        write_jsonl(&path, &events);

        let first_page = load_page(&path, &LogFilter::default(), 3).unwrap();
        assert_eq!(first_page.events[0].message.as_deref(), Some("event-5"));
        let (cursor_ts, cursor_id) = first_page.next_cursor.unwrap();

        let older_filter = LogFilter {
            until_ts: Some(cursor_ts),
            until_id: Some(cursor_id),
            ..Default::default()
        };
        let older_page = load_page(&path, &older_filter, 3).unwrap();
        assert_eq!(older_page.events[0].message.as_deref(), Some("event-2"));
        assert_eq!(older_page.events[1].message.as_deref(), Some("event-1"));
        assert_eq!(older_page.events[2].message.as_deref(), Some("event-0"));
        assert!(older_page.at_end);
    }

    #[test]
    #[allow(deprecated)] // legacy cursor is the subject under test
    fn same_timestamp_pagination_walks_all_events_exactly_once() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace.jsonl");
        let shared_ts = "2026-05-15T19:00:00.000Z";
        let ids = ["evt-a", "evt-b", "evt-c", "evt-d", "evt-e"];
        let mut events = Vec::new();
        for id in ids {
            let mut event = make_event("test", None);
            event.timestamp = shared_ts.to_string();
            event.id = id.to_string();
            event.message = Some(format!("event-{id}"));
            events.push(event);
        }
        write_jsonl(&path, &events);

        let mut seen_ids: Vec<String> = Vec::new();
        let mut page_filter = LogFilter::default();
        let page_size = 2;
        let mut pages_walked = 0;

        loop {
            pages_walked += 1;
            assert!(pages_walked < 20, "pagination must terminate, did not");

            let page = load_page(&path, &page_filter, page_size).unwrap();
            for event in &page.events {
                assert!(
                    !seen_ids.contains(&event.id),
                    "duplicate id {:?} across pages",
                    event.id
                );
                seen_ids.push(event.id.clone());
            }

            if page.at_end {
                // at_end means "no older events exist" but the cursor
                // still points at the last event of the current page;
                // the UI uses at_end to disable the "load older" button.
                break;
            }

            let (cursor_ts, cursor_id) = page
                .next_cursor
                .expect("non-final page must expose a cursor so caller can request older events");
            page_filter = LogFilter {
                until_ts: Some(cursor_ts),
                until_id: Some(cursor_id),
                ..Default::default()
            };
        }

        // Every shared-timestamp event was visited exactly once.
        let mut expected: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
        expected.sort();
        let mut actual = seen_ids.clone();
        actual.sort();
        assert_eq!(
            actual, expected,
            "pagination must visit every tied event exactly once"
        );
    }

    #[test]
    #[allow(deprecated)] // legacy cursor is the subject under test
    fn same_timestamp_cursor_does_not_duplicate_boundary_event() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace.jsonl");
        let shared_ts = "2026-05-15T19:00:00.000Z";
        let mut events = Vec::new();
        // ids ordered so that without id tie-break, evt-b could appear on
        // both page 1 and page 2.
        let ids = ["evt-a", "evt-b", "evt-c"];
        for id in ids {
            let mut event = make_event("test", None);
            event.timestamp = shared_ts.to_string();
            event.id = id.to_string();
            event.message = Some(format!("event-{id}"));
            events.push(event);
        }
        write_jsonl(&path, &events);

        let page1 = load_page(&path, &LogFilter::default(), 1).unwrap();
        assert_eq!(page1.events.len(), 1);
        assert_eq!(page1.events[0].id, "evt-c");
        let (cursor_ts, cursor_id) = page1.next_cursor.unwrap();
        assert_eq!(cursor_id, "evt-c");

        let page2_filter = LogFilter {
            until_ts: Some(cursor_ts),
            until_id: Some(cursor_id),
            ..Default::default()
        };
        let page2 = load_page(&path, &page2_filter, 1).unwrap();
        assert_eq!(page2.events.len(), 1);
        // evt-c must NOT reappear; the next event under the cursor is
        // evt-b (id strictly less than "evt-c" at the same timestamp).
        assert_eq!(page2.events[0].id, "evt-b");
        assert_ne!(page2.events[0].id, page1.events[0].id);
    }

    #[test]
    fn line_offset_pagination_walks_scrambled_ids_exactly_once() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace.jsonl");
        let shared_ts = "2026-05-15T19:00:00.000Z";
        let ids = ["evt-c", "evt-a", "evt-e", "evt-b", "evt-d"];
        let mut events = Vec::new();
        for id in ids {
            let mut event = make_event("test", None);
            event.timestamp = shared_ts.to_string();
            event.id = id.to_string();
            event.message = Some(format!("event-{id}"));
            events.push(event);
        }
        write_jsonl(&path, &events);

        let mut seen_ids: Vec<String> = Vec::new();
        let mut page_filter = LogFilter::default();
        let page_size = 2;
        let mut pages_walked = 0;

        loop {
            pages_walked += 1;
            assert!(pages_walked < 20, "pagination must terminate");

            let page = load_page(&path, &page_filter, page_size).unwrap();
            for event in &page.events {
                assert!(
                    !seen_ids.contains(&event.id),
                    "duplicate id {:?} across pages",
                    event.id
                );
                seen_ids.push(event.id.clone());
            }

            let Some(line_offset) = page.next_cursor_line_offset else {
                // Empty page or no further bytes to scan — we are done.
                break;
            };

            page_filter = LogFilter {
                until_line_offset: Some(line_offset),
                ..Default::default()
            };
        }

        let mut expected: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
        expected.sort();
        let mut actual = seen_ids.clone();
        actual.sort();
        assert_eq!(
            actual, expected,
            "byte-offset cursor must visit every event exactly once even when ids are scrambled"
        );
    }

    #[test]
    fn line_offset_cursor_resumes_with_no_overlap_or_gap() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace.jsonl");
        // Distinct, strictly increasing timestamps so we can detect any
        // ordering regression independently of same-timestamp logic.
        let mut events = Vec::new();
        for index in 0..6 {
            let mut event = make_event("test", None);
            event.timestamp = format!("2026-05-15T19:00:0{index}.000Z");
            event.id = format!("evt-{index}");
            event.message = Some(format!("event-{index}"));
            events.push(event);
        }
        write_jsonl(&path, &events);

        let page_size = 2;
        let mut all_seen_ids: Vec<String> = Vec::new();
        let mut page_filter = LogFilter::default();

        loop {
            let page = load_page(&path, &page_filter, page_size).unwrap();
            for event in &page.events {
                assert!(
                    !all_seen_ids.contains(&event.id),
                    "duplicate {:?} across pages",
                    event.id
                );
                all_seen_ids.push(event.id.clone());
            }
            let Some(line_offset) = page.next_cursor_line_offset else {
                break;
            };
            page_filter = LogFilter {
                until_line_offset: Some(line_offset),
                ..Default::default()
            };
        }

        let expected: Vec<String> = (0..6).rev().map(|i| format!("evt-{i}")).collect();
        assert_eq!(
            all_seen_ids, expected,
            "byte-offset cursor must walk the file in newest-first page order without losing or duplicating events"
        );
    }

    #[test]
    fn line_offset_cursor_advances_monotonically() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace.jsonl");
        let mut events = Vec::new();
        for index in 0..5 {
            let mut event = make_event("test", None);
            event.timestamp = format!("2026-05-15T19:00:0{index}.000Z");
            event.message = Some(format!("event-{index}"));
            events.push(event);
        }
        write_jsonl(&path, &events);

        let mut prev_offset: Option<u64> = None;
        let mut page_filter = LogFilter::default();
        let page_size = 1;

        loop {
            let page = load_page(&path, &page_filter, page_size).unwrap();
            if page.events.is_empty() {
                break;
            }
            let offset = page
                .next_cursor_line_offset
                .expect("non-empty page must expose a line offset cursor");
            if let Some(prev) = prev_offset {
                assert!(
                    offset < prev,
                    "next_cursor_line_offset must strictly decrease across pages as we walk to older events (prev={prev}, next={offset})"
                );
            }
            prev_offset = Some(offset);
            page_filter = LogFilter {
                until_line_offset: Some(offset),
                ..Default::default()
            };
        }
    }

    #[test]
    fn line_offset_cursor_at_file_start_returns_empty_page() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace.jsonl");
        let mut events = Vec::new();
        for index in 0..3 {
            let mut event = make_event("test", None);
            event.timestamp = format!("2026-05-15T19:00:0{index}.000Z");
            events.push(event);
        }
        write_jsonl(&path, &events);

        let filter = LogFilter {
            until_line_offset: Some(0),
            ..Default::default()
        };
        let page = load_page(&path, &filter, 10).unwrap();
        assert!(
            page.events.is_empty(),
            "until_line_offset=0 must skip every line and yield an empty page"
        );
        assert!(page.next_cursor_line_offset.is_none());
        assert!(
            page.at_end,
            "empty page (regardless of cursor state) must report at_end so \
             callers stop paginating instead of looping on a cursor that \
             cannot advance"
        );
    }

    #[test]
    fn empty_page_with_filter_excludes_everything_reports_at_end() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace.jsonl");
        let mut events = Vec::new();
        for index in 0..4 {
            let mut event = make_event("test", None);
            event.timestamp = format!("2026-05-15T19:00:0{index}.000Z");
            events.push(event);
        }
        write_jsonl(&path, &events);

        // First read: filter excludes everything, no cursor set, full
        // file scanned.
        let filter = LogFilter {
            action: Some("does-not-exist".into()),
            ..Default::default()
        };
        let page = load_page(&path, &filter, 10).unwrap();
        assert!(page.events.is_empty());
        assert!(
            page.at_end,
            "empty page after a full-file scan must report at_end"
        );
        assert!(page.next_cursor_line_offset.is_none());

        // Second read: same filter, but a cursor set mid-file. The
        // reader stops at the cursor without matching anything; the
        // page is still empty and `at_end` must still be true.
        let filter_with_cursor = LogFilter {
            action: Some("does-not-exist".into()),
            until_line_offset: Some(50),
            ..Default::default()
        };
        let page2 = load_page(&path, &filter_with_cursor, 10).unwrap();
        assert!(page2.events.is_empty());
        assert!(
            page2.at_end,
            "empty page under an until_line_offset cursor must also report at_end"
        );
    }

    #[test]
    fn action_filter_matches_case_insensitively() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace.jsonl");
        write_jsonl(
            &path,
            &[
                make_event("LlmRequest", None),
                make_event("tool_call", None),
            ],
        );
        let filter = LogFilter {
            action: Some("llmrequest".into()),
            ..Default::default()
        };
        let page = load_page(&path, &filter, 10).unwrap();
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.events[0].event.action, "LlmRequest");
    }

    #[test]
    fn category_filter_matches_case_insensitively() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace.jsonl");
        let mut agent_ev = make_event("a", None);
        agent_ev.event.category = "agent".into();
        let mut tool_ev = make_event("b", None);
        tool_ev.event.category = "tool".into();
        write_jsonl(&path, &[agent_ev, tool_ev]);
        let filter = LogFilter {
            category: Some("AGENT".into()),
            ..Default::default()
        };
        let page = load_page(&path, &filter, 10).unwrap();
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.events[0].event.action, "a");
    }

    #[test]
    fn outcome_filter_matches_case_insensitively() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace.jsonl");
        let mut ok_ev = make_event("a", None);
        ok_ev.event.outcome = "success".into();
        let mut fail_ev = make_event("b", None);
        fail_ev.event.outcome = "failure".into();
        write_jsonl(&path, &[ok_ev, fail_ev]);
        let filter = LogFilter {
            outcome: Some("FAILURE".into()),
            ..Default::default()
        };
        let page = load_page(&path, &filter, 10).unwrap();
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.events[0].event.action, "b");
    }
}
