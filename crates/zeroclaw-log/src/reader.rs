//! Paginated stream reader for the JSONL log file.

use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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

/// Segment-aware pagination cursor.
///
/// Two forms exist, with different stability properties:
///
/// - **Archive cursor** (`archive:<seq>:<off>`): identifies a position in a
///   numbered archive. The sequence number is written into the archive name at
///   rotation time and is never reused, so this form survives any number of
///   subsequent rotations. If the segment has been deleted by retention, the
///   reader returns an empty page with `at_end = true` rather than silently
///   jumping to a different position.
///
/// - **Active cursor** (`active:<off>:<anchor_id>`): identifies a position in
///   the current active file. Because the active file's path is stable but its
///   content is replaced on each rotation, an anchor event id is carried
///   alongside the offset. If the active file no longer contains that event,
///   the reader searches all open segments for the anchor; if found it resumes
///   from that position, if not it returns `at_end = true`.
///
/// A legacy two-field form (`<seg_basename>:<off>`) from older daemons is also
/// accepted on input. It is ambiguous by construction, naming either a legacy
/// archive or the active file, so it is resolved against the archives first
/// (an archive name is never reassigned) and then the active file.
///
/// The token is opaque to clients: round-trip it verbatim as
/// `?until_segment_cursor=`. Constructing or modifying it is unsupported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentCursor {
    pub(crate) kind: CursorKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CursorKind {
    /// Position inside a numbered archive, addressed by its never-reused
    /// sequence number.
    Archive { seq: u64, off: u64 },
    /// Position inside a legacy archive written before sequence numbering.
    /// Addressed by filename, which is stable for an archive: unlike the
    /// active path, an archive name is never reassigned to different content.
    LegacyArchive { name: String, off: u64 },
    /// Position inside the active file, with an optional anchor event id.
    Active { off: u64, anchor_id: Option<String> },
}

impl SegmentCursor {
    /// Parse from one of four wire forms:
    ///
    /// - `active:<off>:<anchor_id>` — active cursor with anchor
    /// - `active:<off>` — active cursor without anchor (legacy active)
    /// - `archive:<seq>:<off>` — numbered archive cursor
    /// - `<basename>:<off>` — pre-sequence form, ambiguous by construction:
    ///   an older daemon issued it for the active file, and it is also how a
    ///   legacy archive is addressed. Parsing cannot tell them apart, so the
    ///   ambiguity is carried to `resolve_cursor`, which tries the archives
    ///   first and falls back to the active file.
    ///
    /// Returns `None` on any parse error.
    pub fn from_wire(s: &str) -> Option<Self> {
        // Active cursor: starts with the fixed "active:" prefix.
        if let Some(rest) = s.strip_prefix("active:") {
            // rest is either "<off>" or "<off>:<anchor_id>"
            return match rest.split_once(':') {
                // An explicit trailing colon with no anchor (`active:10:`) is
                // malformed. Rejecting it rather than normalising to
                // `active:10` keeps a client-side construction bug visible.
                Some((_, "")) => None,
                // Anchors are event ids, which are UUIDs and never contain a
                // colon. An extra field means the token was built by hand or
                // corrupted in transit, so reject it rather than silently
                // folding the remainder into the anchor and resolving against
                // an id that was never issued.
                Some((_, anchor)) if anchor.contains(':') => None,
                Some((off_str, anchor)) => Some(Self {
                    kind: CursorKind::Active {
                        off: off_str.parse().ok()?,
                        anchor_id: Some(anchor.to_owned()),
                    },
                }),
                None => Some(Self {
                    kind: CursorKind::Active {
                        off: rest.parse().ok()?,
                        anchor_id: None,
                    },
                }),
            };
        }

        // Numbered archives have their own prefix so an archive sequence can
        // never be confused with a legacy cursor for an all-numeric basename.
        if let Some(rest) = s.strip_prefix("archive:")
            && rest.contains(':')
        {
            let (seq_str, off_str) = rest.split_once(':')?;
            if seq_str.is_empty() || off_str.is_empty() || off_str.contains(':') {
                return None;
            }
            return Some(Self {
                kind: CursorKind::Archive {
                    seq: seq_str.parse().ok()?,
                    off: off_str.parse().ok()?,
                },
            });
        }

        // Legacy form: `<basename>:<off>`. Exactly two fields; the name must
        // not contain a colon because writer-generated segment names never do.
        // An extra colon means the token is malformed rather than a legacy one.
        let (name, off_str) = s.split_once(':')?;
        if name.is_empty() || off_str.contains(':') {
            return None;
        }
        Some(Self {
            kind: CursorKind::LegacyArchive {
                name: name.to_owned(),
                off: off_str.parse().ok()?,
            },
        })
    }

    /// Serialize to wire format.
    pub fn to_wire(&self) -> String {
        match &self.kind {
            CursorKind::Archive { seq, off } => format!("archive:{seq}:{off}"),
            // The legacy wire form predates the `active:` prefix and the
            // sequence grammar, so it is the one shape that still addresses a
            // segment by name.
            CursorKind::LegacyArchive { name, off } => format!("{name}:{off}"),
            CursorKind::Active {
                off,
                anchor_id: Some(id),
            } => format!("active:{off}:{id}"),
            CursorKind::Active {
                off,
                anchor_id: None,
            } => format!("active:{off}"),
        }
    }
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
    ///
    /// For multi-segment deployments, this is `None` when the oldest event is
    /// in an archive file rather than the active file; use
    /// [`Self::next_segment_cursor`] in that case.
    pub next_cursor_line_offset: Option<u64>,
    /// Segment-aware cursor for the oldest event on this page. Pass back as
    /// `?until_segment_cursor=` on the next request to walk older pages across
    /// segment boundaries. Supersedes `next_cursor_line_offset` for
    /// multi-segment deployments. `None` when the page is empty.
    pub next_segment_cursor: Option<String>,
    /// True when the file was fully scanned. UI uses this to disable
    /// "load older" affordances.
    pub at_end: bool,
    /// True when a retained segment could not be read and was left out.
    ///
    /// The page is still returned: one unreadable archive should not cost the
    /// caller every other segment. But it makes `at_end` non-authoritative —
    /// it then means "no older events among the segments that could be read",
    /// which is not the same as "no older events exist". A caller that stops
    /// paging on `at_end` should surface this rather than present the result
    /// as the complete history.
    ///
    /// The daemon also logs each skipped segment, but a log line reaches the
    /// operator while this reaches the caller that has to decide what to do.
    ///
    /// Defaulted on deserialize so a page from an older daemon still parses.
    #[serde(default)]
    pub incomplete: bool,
}

#[allow(deprecated)] // we still populate `next_cursor` for backwards compat
pub fn load_page(path: &Path, filter: &LogFilter, limit: usize) -> Result<LogPage> {
    let limit = limit.clamp(1, 10_000);

    if !path.exists() {
        return Ok(LogPage {
            events: Vec::new(),
            next_cursor: None,
            next_cursor_line_offset: None,
            next_segment_cursor: None,
            at_end: true,
            incomplete: false,
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
        next_segment_cursor: None,
        at_end,
        // This path reads one file and fails outright if it cannot be opened,
        // so a page from here is never a partial view.
        incomplete: false,
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

/// Split `foo.jsonl` into `("foo", ".jsonl")`. A name with no dot, or one
/// whose only dot is leading, keeps an empty extension.
pub(crate) fn split_base_ext(file_name: &str) -> (&str, &str) {
    match file_name.rfind('.') {
        Some(i) if i > 0 => (&file_name[..i], &file_name[i..]),
        _ => (file_name, ""),
    }
}

/// True when `s` is exactly a `YYYYMMDD-HHMMSS` stamp: 8 digits, `-`, 6 digits.
pub(crate) fn is_stamp(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 15
        && b[..8].iter().all(u8::is_ascii_digit)
        && b[8] == b'-'
        && b[9..].iter().all(u8::is_ascii_digit)
}

/// Width of the zero-padded sequence prefix in a numbered archive name.
///
/// Ten digits keeps the prefix distinguishable from a bare `YYYYMMDD` stamp
/// (eight digits) and leaves headroom far beyond any realistic rotation count.
pub(crate) const SEQ_WIDTH: usize = 10;

/// Ordering key for one archive, derived from its name rather than its mtime.
///
/// Rotation writes the sequence number into the archive name, so segment order
/// is fixed at write time and does not depend on when a reader enumerates the
/// directory. That is what makes the order survive several rotations landing
/// during a single read: an mtime-based key is an observation made at
/// enumeration time, and two rotations can leave it describing an order that
/// no longer holds.
///
/// Archives written before sequence numbers existed carry no number. They are
/// ordered by mtime and sort before every numbered archive, which is correct
/// because they can only predate the upgrade that introduced numbering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ArchiveOrder {
    /// Pre-numbering archive, ordered by mtime. Sorts before all numbered ones.
    Legacy(SystemTime),
    /// Sequence number taken from the archive name.
    Seq(u64),
}

/// Parse the sequence number out of an archive name core.
///
/// Accepts the numbered form `<seq>-<stamp>`; returns `None` for the legacy
/// `<stamp>` and `<stamp>.<counter>` forms, which carry no number.
pub(crate) fn archive_seq(core: &str) -> Option<u64> {
    let (seq, rest) = core.split_once('-')?;
    // A stamp is `YYYYMMDD-HHMMSS`, whose own first segment is 8 digits. The
    // sequence prefix is zero-padded to 10, so the two cannot be confused.
    if seq.len() != SEQ_WIDTH || !seq.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // The remainder must be a bare stamp; otherwise the leading digits are
    // part of something else and this is not a numbered archive.
    if !is_stamp(rest) {
        return None;
    }
    seq.parse().ok()
}

/// True when `core` is a stamp this writer generates, optionally with a
/// same-second disambiguation counter appended.
pub(crate) fn is_archive_core(core: &str) -> bool {
    // Current form: `<seq>-<stamp>`, where the sequence number fixes segment
    // order at write time.
    if archive_seq(core).is_some() {
        return true;
    }
    // Legacy forms, still readable so an upgrade does not orphan existing
    // archives: `<stamp>` and `<stamp>.<counter>`.
    match core.split_once('.') {
        Some((stamp, counter)) => {
            !counter.is_empty() && counter.bytes().all(|b| b.is_ascii_digit()) && is_stamp(stamp)
        }
        None => is_stamp(core),
    }
}

/// Enumerate the archive files that belong to `active`, each paired with its
/// [`ArchiveOrder`] key.
///
/// Matching is restricted to names this writer generates, so unrelated
/// siblings in the same directory are never returned. The active file itself
/// is excluded. Order is unspecified; callers that need stream order sort by
/// the returned key.
///
/// A directory that cannot be listed at all is an error. A single entry whose
/// metadata cannot be read is logged and skipped instead, because one bad file
/// should not cost the caller every other archive; see
/// [`list_archives_reporting`] when the caller needs to know that happened.
pub(crate) fn list_archives(active: &Path) -> Result<Vec<(PathBuf, ArchiveOrder)>> {
    let mut unreadable = false;
    list_archives_reporting(active, &mut unreadable)
}

/// [`list_archives`], additionally setting `unreadable` when an entry that
/// looked like an archive had to be skipped because its metadata could not be
/// read. Query paths use that to mark the resulting page incomplete.
pub(crate) fn list_archives_reporting(
    active: &Path,
    unreadable: &mut bool,
) -> Result<Vec<(PathBuf, ArchiveOrder)>> {
    let dir = active.parent().unwrap_or_else(|| Path::new("."));
    let active_name = active
        .file_name()
        .and_then(|s| s.to_str())
        .context("log path has no file name")?;
    let (base, ext) = split_base_ext(active_name);
    let prefix = format!("{base}.");

    let mut out = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(err) => {
            return Err(err).with_context(|| format!("reading log dir {}", dir.display()));
        }
    };
    for entry in entries {
        let entry = entry.with_context(|| format!("reading entry in {}", dir.display()))?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if name == active_name {
            continue;
        }
        let Some(suffix) = name.strip_prefix(&prefix) else {
            continue;
        };
        let core = if ext.is_empty() {
            suffix
        } else {
            let Some(core) = suffix.strip_suffix(ext) else {
                continue;
            };
            core
        };
        if !is_archive_core(core) {
            continue;
        }
        // `symlink_metadata` does not follow links, so a symlink shaped like an
        // archive name is rejected below instead of being opened. A local actor
        // who can write into the log directory could otherwise point a
        // matching name at any file the daemon can read and have an
        // authenticated log query return its contents.
        let meta = match std::fs::symlink_metadata(entry.path()) {
            Ok(m) => m,
            // Retention or a rotation removed it between the listing and now.
            // An ordinary race, not an unreadable segment.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                tracing::warn!(
                    target: "zeroclaw_log",
                    error = ?err,
                    path = %entry.path().display(),
                    "log: could not read archive metadata; this archive is excluded from \
                     the merged query result and may be inaccessible",
                );
                *unreadable = true;
                continue;
            }
        };
        if !meta.is_file() {
            continue;
        }
        // Prefer the sequence number in the name; fall back to mtime only for
        // archives written before numbering existed.
        let order = match archive_seq(core) {
            Some(seq) => ArchiveOrder::Seq(seq),
            None => ArchiveOrder::Legacy(meta.modified().unwrap_or(SystemTime::UNIX_EPOCH)),
        };
        out.push((entry.path(), order));
    }
    Ok(out)
}

/// Lightweight descriptor for one log segment.
///
/// Segments are opened on demand, one at a time, during a scan rather than all
/// pinned up front. Archives are immutable once created, so opening an archive
/// path at scan time yields the same bytes an earlier pin would have. The
/// active file can be replaced by a rotation between enumeration and scan, and
/// this design accepts that:
///
///   - Rotation before we open the active path: we read the replacement file,
///     and the rotated-away content appears as a numbered archive on the next
///     query.
///   - Rotation while we are reading a segment: the already-open handle keeps
///     reading its original inode and returns the pre-rotation content.
///
/// Neither case corrupts a page. At worst one rotation's worth of events is
/// delayed by a single request, which is the tradeoff this reader takes in
/// exchange for holding one descriptor at a time instead of all of them.
#[derive(Debug, Clone)]
pub(crate) struct SegmentMeta {
    pub(crate) path: PathBuf,
    /// Basename, captured at enumeration. A legacy archive has no sequence
    /// number, so its name is the only stable way a cursor can address it.
    pub(crate) name: String,
    /// Sequence number parsed from the archive name, or `None` for the active
    /// file and for legacy archives written before numbering existed.
    pub(crate) seq: Option<u64>,
    /// True for the active file.
    pub(crate) is_active: bool,
}

/// Enumerate the segments of this query's logical stream, oldest first with
/// the active file last.
///
/// Archives are ordered by the sequence number embedded in their name at
/// rotation time, so the order cannot be disturbed by enumeration-time races.
/// The active file is always last because it holds the newest events.
///
/// A missing active file is normal on a fresh workspace and simply yields a
/// list of archives.
///
/// `unreadable` is set when a segment that belongs to this stream had to be
/// left out because it could not be inspected. The query paths carry that into
/// `LogPage::incomplete`, so a caller is never told the history ended when it
/// only ran out of readable files.
pub(crate) fn enumerate_segment_metas(
    active: &Path,
    reads_archives: bool,
    unreadable: &mut bool,
) -> Result<Vec<SegmentMeta>> {
    let mut segs: Vec<SegmentMeta> = Vec::new();

    if reads_archives {
        let mut archives = list_archives_reporting(active, unreadable).with_context(|| {
            format!(
                "enumerating archives next to {} for a page query",
                active.display()
            )
        })?;
        archives.sort_by_key(|(_, order)| *order);
        for (path, order) in archives {
            let seq = match order {
                ArchiveOrder::Seq(n) => Some(n),
                ArchiveOrder::Legacy(_) => None,
            };
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_owned();
            segs.push(SegmentMeta {
                path,
                name,
                seq,
                is_active: false,
            });
        }
    }

    // Not `Path::exists`: that collapses every error into "absent", so a
    // permission or I/O failure on the active file would drop the newest
    // segment from the stream and let `at_end` be computed as if the missing
    // events did not exist. Only `NotFound` means absent — normal on a fresh
    // workspace, and normal for the instant between a rotation's rename and
    // the next append.
    let active_present = match std::fs::metadata(active) {
        Ok(_) => true,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(err) => {
            tracing::warn!(
                target: "zeroclaw_log",
                error = ?err,
                path = %active.display(),
                "log: could not stat the active log file; this page omits it and is \
                 not a complete view of the stream",
            );
            *unreadable = true;
            false
        }
    };

    if active_present {
        segs.push(SegmentMeta {
            path: active.to_owned(),
            name: active
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_owned(),
            seq: None,
            is_active: true,
        });
    }

    Ok(segs)
}

/// Read one page across the active file and every retained archive.
///
/// This is the entry point for `/api/logs` and the `logs/query` RPC. It owns
/// Scan one segment, feeding matching events into the sliding window.
///
/// Opens the file, reads it, and closes it before returning, so a query holds
/// one descriptor at a time no matter how many segments it walks. An
/// unreadable segment is skipped with a warning rather than failing the whole
/// page: one bad file should not hide the history in every other segment.
///
/// `until_off` bounds the scan to lines whose `line_byte_end` is strictly
/// below it, which is how a cursor excludes the page it already returned.
fn scan_segment(
    seg: &SegmentMeta,
    filter: &LogFilter,
    needle: Option<&str>,
    limit: usize,
    until_off: Option<u64>,
    window: &mut VecDeque<(LogEvent, SegmentRef, u64)>,
    dropped_older: &mut bool,
    unreadable: &mut bool,
) -> Result<()> {
    let file = match File::open(&seg.path) {
        Ok(f) => f,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // Retention pruned it, or a rotation moved it, between enumeration
            // and now. Both are ordinary races, not failures.
            return Ok(());
        }
        Err(err) => {
            tracing::warn!(
                target: "zeroclaw_log",
                error = ?err,
                path = %seg.path.display(),
                "log: could not open segment; excluded from this page",
            );
            // Skipping keeps the rest of the history available, but the page
            // can no longer claim to be the whole stream.
            *unreadable = true;
            return Ok(());
        }
    };

    let mut reader = BufReader::new(file);
    let mut next_byte_offset: u64 = 0;
    let mut buf = String::new();

    loop {
        buf.clear();
        let bytes_read = reader.read_line(&mut buf).context("reading log line")?;
        if bytes_read == 0 {
            break;
        }
        let line_byte_end = next_byte_offset + bytes_read as u64;

        if let Some(cap) = until_off
            && line_byte_end >= cap
        {
            break;
        }

        let trimmed = buf.trim();
        next_byte_offset = line_byte_end;

        if trimmed.is_empty() {
            continue;
        }

        let event: LogEvent = match serde_json::from_str(trimmed) {
            Ok(e) => e,
            Err(err) => {
                tracing::trace!(
                    target: "zeroclaw_log",
                    error = ?err,
                    "log: skipping malformed JSONL line"
                );
                continue;
            }
        };

        if !matches_filter(&event, filter, needle) {
            continue;
        }

        let seg_ref = if seg.is_active {
            SegmentRef::Active
        } else {
            match seg.seq {
                Some(seq) => SegmentRef::Archive(seq),
                None => SegmentRef::LegacyArchive(seg.name.clone()),
            }
        };
        window.push_back((event, seg_ref, line_byte_end));
        if window.len() > limit {
            window.pop_front();
            *dropped_older = true;
        }
    }
    Ok(())
}

/// Which segment an event came from, in the form a cursor can address.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SegmentRef {
    Active,
    /// A numbered archive, addressed by its never-reused sequence number.
    Archive(u64),
    /// A legacy archive written before sequence numbering existed. It has no
    /// number, but its filename is stable, so a cursor addresses it by name.
    LegacyArchive(String),
}

/// Find where a cursor says to resume, as an index into `segs` plus a byte cap.
///
/// Returns `None` when the cursor addresses history that is no longer present,
/// which the caller reports as "at end" rather than silently restarting from
/// the newest page.
fn resolve_cursor(segs: &[SegmentMeta], cursor: &SegmentCursor) -> Option<(usize, u64)> {
    match &cursor.kind {
        // An archive sequence is permanent: the number is written into the name
        // at rotation and never reused. Not finding it means retention removed
        // that segment, so there is genuinely nothing older to return. The
        // explicit `archive:` wire prefix keeps this identity distinct from a
        // legacy cursor naming an all-numeric active file.
        CursorKind::Archive { seq, off } => segs
            .iter()
            .position(|s| s.seq == Some(*seq))
            .map(|idx| (idx, *off)),

        // A name-addressed cursor. This is the pre-sequence wire form, and it
        // has always been ambiguous about which segment it names: a daemon that
        // predates sequence numbering issued `<basename>:<off>` for the active
        // file, while a legacy archive is addressed the same way. Both are
        // still in circulation, so try the archives first (an archive name is
        // never reassigned, so a hit there is unambiguous) and fall back to the
        // active file when the name matches it instead.
        //
        // The active fallback carries no anchor, so it cannot detect a rotation
        // that happened since the cursor was issued. That is the same exposure
        // the form always had; cursors this reader issues for the active file
        // use the anchored `active:` shape.
        CursorKind::LegacyArchive { name, off } => segs
            .iter()
            .position(|s| !s.is_active && s.name == *name)
            .or_else(|| segs.iter().position(|s| s.is_active && s.name == *name))
            .map(|idx| (idx, *off)),

        // The active file's path is stable but its content is replaced on every
        // rotation, so the offset alone cannot be trusted. The anchor id says
        // which event the offset was measured against.
        CursorKind::Active { off, anchor_id } => {
            let active_idx = segs.iter().position(|s| s.is_active);

            let Some(anchor) = anchor_id else {
                // A legacy cursor without an anchor can only be applied to the
                // active file as-is. If a rotation has happened since it was
                // issued the offset is meaningless, but there is no way to
                // detect that, so this keeps the pre-anchor behaviour.
                return active_idx.map(|idx| (idx, *off));
            };

            // Fast path: the active file still holds the anchored event at the
            // recorded boundary, so nothing rotated since the cursor was issued.
            if let Some(idx) = active_idx
                && anchor_is_at_offset(&segs[idx], *off, anchor)
            {
                return Some((idx, *off));
            }

            // Otherwise the active file was rotated. The events the cursor
            // addressed now live in an archive, so find the one holding the
            // anchor and resume immediately after it. Newest first, since a
            // just-rotated segment is the likely home.
            for (idx, seg) in segs.iter().enumerate().rev() {
                if let Some(end_off) = find_anchor_offset(seg, anchor) {
                    return Some((idx, end_off));
                }
            }

            // The anchored event is in no surviving segment: it was rotated out
            // and then pruned. Report the end of history.
            None
        }
    }
}

/// True when the first non-empty JSONL line ending at or after `off` carries
/// `anchor_id`. Any read or parse failure answers `false`, which routes the
/// caller to the slower whole-segment search rather than trusting a guess.
fn anchor_is_at_offset(seg: &SegmentMeta, off: u64, anchor_id: &str) -> bool {
    let Ok(file) = File::open(&seg.path) else {
        return false;
    };
    let mut reader = BufReader::new(file);
    let mut buf = String::new();
    let mut byte_off: u64 = 0;
    loop {
        buf.clear();
        let Ok(n) = reader.read_line(&mut buf) else {
            return false;
        };
        if n == 0 {
            return false;
        }
        byte_off += n as u64;
        if byte_off < off {
            continue;
        }
        let trimmed = buf.trim();
        if trimmed.is_empty() {
            continue;
        }
        return serde_json::from_str::<LogEvent>(trimmed)
            .map(|e| e.id == anchor_id)
            .unwrap_or(false);
    }
}

/// Byte offset just past the event with `anchor_id` in this segment, if it is
/// there. That offset excludes the anchored event itself, so resuming from it
/// returns strictly older events.
fn find_anchor_offset(seg: &SegmentMeta, anchor_id: &str) -> Option<u64> {
    let file = File::open(&seg.path).ok()?;
    let mut reader = BufReader::new(file);
    let mut buf = String::new();
    let mut byte_off: u64 = 0;
    loop {
        buf.clear();
        let n = reader.read_line(&mut buf).ok()?;
        if n == 0 {
            return None;
        }
        let line_end = byte_off + n as u64;
        let trimmed = buf.trim();
        if !trimmed.is_empty()
            && serde_json::from_str::<LogEvent>(trimmed)
                .map(|e| e.id == anchor_id)
                .unwrap_or(false)
        {
            return Some(line_end);
        }
        byte_off = line_end;
    }
}

/// Read one page across the active file and every retained archive.
///
/// This is the entry point for `/api/logs` and the `logs/query` RPC. It owns
/// segment enumeration so callers never hold a list of their own, which could
/// go stale the moment the writer rotates.
///
/// Segments are opened one at a time as the scan reaches them, so a query holds
/// a single descriptor regardless of how much history is retained. A rotation
/// racing the scan is accepted rather than compensated for: order comes from
/// the sequence numbers in archive names, so it can never be scrambled, and the
/// worst case is that one rotation's worth of events is first visible on the
/// next request.
/// Scan `segs` up to and including `cursor_idx`, stopping at `cursor_off`
/// bytes inside that segment. Returns the assembled `LogPage`.
#[allow(deprecated)]
fn do_scan(
    segs: &[SegmentMeta],
    filter: &LogFilter,
    needle: Option<&str>,
    limit: usize,
    cursor_idx: usize,
    cursor_off: Option<u64>,
    enum_unreadable: bool,
) -> Result<LogPage> {
    let mut window: VecDeque<(LogEvent, SegmentRef, u64)> = VecDeque::with_capacity(limit + 1);
    let mut dropped_older = false;
    // Seeded from enumeration: a segment that could not even be inspected is
    // already missing from `segs`, so the scan itself cannot notice it.
    let mut unreadable = enum_unreadable;

    for (i, seg) in segs.iter().enumerate() {
        if i > cursor_idx {
            break;
        }
        let until_off = if i == cursor_idx { cursor_off } else { None };
        scan_segment(
            seg,
            filter,
            needle,
            limit,
            until_off,
            &mut window,
            &mut dropped_older,
            &mut unreadable,
        )?;
    }

    // The cursor for the next page addresses the oldest event on this one.
    // Every segment kind can produce one: numbered archives by sequence, legacy
    // archives by filename (stable for an archive, unlike the active path), and
    // the active file by offset plus an anchor event id.
    let oldest = window.front();
    let next_segment_cursor = oldest.map(|(evt, seg_ref, off)| {
        let kind = match seg_ref {
            SegmentRef::Active => CursorKind::Active {
                off: *off,
                anchor_id: Some(evt.id.clone()),
            },
            SegmentRef::Archive(seq) => CursorKind::Archive {
                seq: *seq,
                off: *off,
            },
            SegmentRef::LegacyArchive(name) => CursorKind::LegacyArchive {
                name: name.clone(),
                off: *off,
            },
        };
        SegmentCursor { kind }.to_wire()
    });
    let next_cursor_line_offset = oldest.and_then(|(_, seg_ref, off)| match seg_ref {
        SegmentRef::Active => Some(*off),
        SegmentRef::Archive(_) | SegmentRef::LegacyArchive(_) => None,
    });

    let mut events: Vec<LogEvent> = window.into_iter().map(|(e, _, _)| e).collect();
    events.reverse();

    let next_cursor = events.last().map(|e| (e.timestamp.clone(), e.id.clone()));
    // `at_end` answers exactly one question: are there OLDER events past this
    // page? Every segment up to and including the cursor's is scanned in full,
    // so the only way an older match can exist is if the sliding window evicted
    // one. Hitting the cursor mid-segment truncates that segment's *newer*
    // tail, which says nothing about older events and deliberately does not
    // feed this decision.
    //
    // Whether a cursor can be issued is a separate question and must not be
    // folded in here. Forcing `at_end` true at a boundary that cannot be
    // addressed would claim history ended when it had not, stranding the rows
    // behind it. Every segment kind can now issue a cursor, so the two concerns
    // stay independent.
    //
    // Both readings are conditioned on the segments that could actually be
    // read; `incomplete` is what tells the caller that condition was not
    // vacuous.
    let at_end = !dropped_older;

    Ok(LogPage {
        events,
        next_cursor,
        next_cursor_line_offset,
        next_segment_cursor,
        at_end,
        incomplete: unreadable,
    })
}

/// Resolve a cursor against `segs` into `(cursor_idx, cursor_off)`.
///
/// `Err(())` means the cursor addresses a segment that is not in this listing.
/// The caller decides what that implies: it can mean the segment was pruned, or
/// merely that a rotation created it after this listing was taken.
fn resolve_or_at_end(
    segs: &[SegmentMeta],
    segment_cursor: Option<&SegmentCursor>,
    filter: &LogFilter,
) -> Result<(usize, Option<u64>), ()> {
    match segment_cursor {
        Some(cursor) => resolve_cursor(segs, cursor)
            .map(|(idx, off)| (idx, Some(off)))
            .ok_or(()),
        None => match (
            filter.until_line_offset,
            segs.iter().position(|s| s.is_active),
        ) {
            (Some(off), Some(idx)) => Ok((idx, Some(off))),
            _ => Ok((segs.len().saturating_sub(1), None)),
        },
    }
}

/// How many times a read is redone because its segment snapshot moved under it.
const MAX_SNAPSHOT_ATTEMPTS: usize = 3;

/// Read one page across the active file and every retained archive.
///
/// This is the entry point for `/api/logs` and the `logs/query` RPC. It owns
/// segment enumeration so callers never hold a list of their own, which could
/// go stale the moment the writer rotates.
///
/// Segments are opened one at a time as the scan reaches them, so a query holds
/// a single descriptor regardless of how much history is retained.
///
/// # Rotation racing the listing
///
/// Enumeration and reading are not atomic, so a rotation can land between them:
/// the listing names the pre-rotation active file, the rename turns that file
/// into a new archive, and a fresh, near-empty active file takes its place. The
/// scan then reads the replacement and never sees the rotated-away content —
/// which sits, unlisted, between two segments this page did read.
///
/// That is not recoverable on a later request. Pagination walks newest to
/// oldest, so once this page's cursor lands in a segment older than the
/// rotated-away one, following that cursor only ever moves further away from
/// it. The events are skipped permanently.
///
/// Two properties make this cheap to close. Archives are immutable and only
/// ever added or pruned as whole files, so a scan that stops before the active
/// file cannot be racing anything and needs no check at all. And any rotation
/// necessarily changes the set of archives on disk.
///
/// So: read, then re-enumerate, and redo the whole page if the archive set
/// moved. Checking afterwards rather than before is what closes the window —
/// a check before opening the file leaves room for a rotation between the check
/// and the open. Read-then-verify has no such gap: either the set is unchanged,
/// in which case nothing moved beneath the read, or it changed and the page is
/// discarded and redone against the newer listing.
///
/// Rotations during the redo just go round again, bounded by
/// `MAX_SNAPSHOT_ATTEMPTS`. Exhausting that bound returns an error: every page
/// produced by those attempts is known to omit a segment, so returning one as
/// successful would let backward pagination skip retained events permanently.
pub fn query_log_page(
    active: &Path,
    reads_archives: bool,
    filter: &LogFilter,
    limit: usize,
    segment_cursor: Option<&SegmentCursor>,
) -> Result<LogPage> {
    let limit = limit.clamp(1, 10_000);
    let needle = filter.q.as_deref().map(|s| s.to_ascii_lowercase());

    for _ in 0..MAX_SNAPSHOT_ATTEMPTS {
        let mut unreadable = false;
        let segs = enumerate_segment_metas(active, reads_archives, &mut unreadable)?;
        after_enumerate_hook();

        let Ok((cursor_idx, cursor_off)) = resolve_or_at_end(&segs, segment_cursor, filter) else {
            // The cursor names a segment this listing does not have. Either a
            // rotation created it after the listing was taken, or retention
            // removed it for good. The archive set says which.
            if reads_archives && archive_set_moved(active, &segs)? {
                continue;
            }
            let mut page = at_end_page();
            page.incomplete = unreadable;
            return Ok(page);
        };

        let page = do_scan(
            &segs,
            filter,
            needle.as_deref(),
            limit,
            cursor_idx,
            cursor_off,
            unreadable,
        )?;

        // A scan that stops before the active file read only immutable
        // archives, so no rotation could have moved anything it saw.
        let read_active = segs.get(cursor_idx).is_some_and(|s| s.is_active);
        if !read_active || !reads_archives {
            return Ok(page);
        }
        if !archive_set_moved(active, &segs)? {
            return Ok(page);
        }
    }

    anyhow::bail!("log segment snapshot did not stabilize after {MAX_SNAPSHOT_ATTEMPTS} attempts")
}

// Test seam: runs immediately after each reader-owned enumeration, letting a
// test rotate the log inside the exact window the surrounding code exists to
// survive. Without it a test can only check the pieces, and would still pass
// with the re-check removed.
#[cfg(test)]
thread_local! {
    static AFTER_ENUMERATE: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Run the installed seam, if any. The callback is moved out for the call so
/// it can itself touch the log without re-entering a live borrow.
#[cfg(test)]
fn after_enumerate_hook() {
    let mut hook = AFTER_ENUMERATE.with(|slot| slot.borrow_mut().take());
    if let Some(callback) = hook.as_mut() {
        callback();
    }
    AFTER_ENUMERATE.with(|slot| *slot.borrow_mut() = hook);
}

#[cfg(not(test))]
fn after_enumerate_hook() {}

/// True when the archives on disk are no longer the ones in `before`.
///
/// A rotation renames the active file into a new archive, and retention deletes
/// whole archives, so either one shows up as a difference here. The active file
/// is excluded: its name never changes, so it carries no signal.
fn archive_set_moved(active: &Path, before: &[SegmentMeta]) -> Result<bool> {
    let now = list_archives(active)?;
    let mut before_names: Vec<&str> = before
        .iter()
        .filter(|s| !s.is_active)
        .map(|s| s.name.as_str())
        .collect();
    let mut now_names: Vec<&str> = now
        .iter()
        .filter_map(|(p, _)| p.file_name().and_then(|s| s.to_str()))
        .collect();
    before_names.sort_unstable();
    now_names.sort_unstable();
    Ok(before_names != now_names)
}

/// The empty page returned when a cursor addresses history that no longer
/// exists. `at_end` keeps a paging client where it is instead of silently
/// restarting it at the newest events.
#[allow(deprecated)]
fn at_end_page() -> LogPage {
    LogPage {
        events: Vec::new(),
        next_cursor: None,
        next_cursor_line_offset: None,
        next_segment_cursor: None,
        at_end: true,
        // Callers that skipped a segment overwrite this.
        incomplete: false,
    }
}

/// Outcome of [`find_event_across_segments`].
#[derive(Debug, Clone)]
pub struct SegmentLookup {
    /// The event, when a segment held it.
    pub event: Option<LogEvent>,
    /// True when a segment of this stream could not be read and was skipped.
    ///
    /// Only meaningful alongside `event: None`. A hit is authoritative however
    /// many other segments were skipped, but a miss is not: the id may be
    /// sitting in the segment that could not be opened, and answering "no such
    /// event" would be a guess presented as a fact.
    pub incomplete: bool,
}

/// Find one event by id across the active file and every retained archive,
/// newest source first. Owns segment enumeration for the same reason as
/// [`query_log_page`]: the caller never holds a list that can go stale.
///
/// A segment that cannot be read is skipped rather than failing the lookup, so
/// one unreadable file does not hide an event another segment still has. When
/// that happens and nothing matched, [`SegmentLookup::incomplete`] says the
/// miss is not authoritative.
pub fn find_event_across_segments(
    active: &Path,
    reads_archives: bool,
    id: &str,
) -> Result<SegmentLookup> {
    // A rotation can replace the active path after enumeration. Reading that
    // replacement succeeds but misses every event moved into the newly created,
    // unlisted archive, so verify the archive set after every complete miss and
    // retry from a fresh snapshot when it moved.
    for _ in 0..MAX_SNAPSHOT_ATTEMPTS {
        // Same policy boundary as `query_log_page`: an id that only exists in
        // an unmanaged archive is not part of this policy's logical stream.
        let mut incomplete = false;
        let segs = enumerate_segment_metas(active, reads_archives, &mut incomplete)?;
        after_enumerate_hook();
        let mut active_missing = false;

        // Newest first (active file, then archives newest to oldest), since a
        // recently rotated event is the likelier target.
        for seg in segs.iter().rev() {
            let file = match File::open(&seg.path) {
                Ok(f) => f,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    // A missing archive may have been pruned. A missing active
                    // path can also be the rename half of an in-flight rotation;
                    // redo the lookup instead of treating that window as a
                    // conclusive miss.
                    active_missing |= seg.is_active;
                    continue;
                }
                Err(err) => {
                    tracing::warn!(
                        target: "zeroclaw_log",
                        error = ?err,
                        path = %seg.path.display(),
                        "log: skipping unreadable segment during id lookup"
                    );
                    incomplete = true;
                    continue;
                }
            };
            for line in BufReader::new(file).lines() {
                let Ok(line) = line else {
                    // The rest of this segment is unreadable. Keep searching
                    // the others, but the part not read could have held the id.
                    tracing::warn!(
                        target: "zeroclaw_log",
                        path = %seg.path.display(),
                        "log: stopped reading a segment early during id lookup"
                    );
                    incomplete = true;
                    break;
                };
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(event) = serde_json::from_str::<LogEvent>(trimmed)
                    && event.id == id
                {
                    return Ok(SegmentLookup {
                        event: Some(event),
                        incomplete: false,
                    });
                }
            }
        }

        if active_missing || (reads_archives && archive_set_moved(active, &segs)?) {
            continue;
        }
        return Ok(SegmentLookup {
            event: None,
            incomplete,
        });
    }

    // Continuous churn prevented a stable miss. Make that uncertainty visible
    // so the RPC layer does not turn it into an authoritative "not found".
    Ok(SegmentLookup {
        event: None,
        incomplete: true,
    })
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

    /// Set a file's mtime so a test can make the name-derived and
    /// mtime-derived ordering keys disagree deliberately.
    fn set_mtime(path: &Path, when: SystemTime) {
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
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

    #[test]
    fn multi_segment_reads_across_active_and_archive() {
        let tmp = tempfile::tempdir().unwrap();
        let active = tmp.path().join("trace.jsonl");
        // Archive: two older events.
        let archive = tmp.path().join("trace.20260101-000000.jsonl");
        let mut old_a = make_event("a", None);
        old_a.timestamp = "2026-01-01T00:00:00.000Z".into();
        old_a.message = Some("old-a".into());
        let mut old_b = make_event("b", None);
        old_b.timestamp = "2026-01-01T00:00:01.000Z".into();
        old_b.message = Some("old-b".into());
        write_jsonl(&archive, &[old_a, old_b]);

        // Active file: two newer events.
        let mut new_c = make_event("c", None);
        new_c.timestamp = "2026-06-01T00:00:00.000Z".into();
        new_c.message = Some("new-c".into());
        let mut new_d = make_event("d", None);
        new_d.timestamp = "2026-06-01T00:00:01.000Z".into();
        new_d.message = Some("new-d".into());
        write_jsonl(&active, &[new_c, new_d]);

        let page = query_log_page(&active, true, &LogFilter::default(), 10, None).unwrap();

        assert_eq!(page.events.len(), 4, "all 4 events across segments");
        // Newest first.
        assert_eq!(page.events[0].message.as_deref(), Some("new-d"));
        assert_eq!(page.events[1].message.as_deref(), Some("new-c"));
        assert_eq!(page.events[2].message.as_deref(), Some("old-b"));
        assert_eq!(page.events[3].message.as_deref(), Some("old-a"));
        assert!(page.at_end, "entire stream was scanned");
    }

    #[test]
    fn segment_cursor_paginates_into_archive() {
        let tmp = tempfile::tempdir().unwrap();
        let active = tmp.path().join("trace.jsonl");
        let archive = tmp.path().join("trace.20260101-000000.jsonl");

        let mut old_a = make_event("a", None);
        old_a.timestamp = "2026-01-01T00:00:00.000Z".into();
        old_a.message = Some("old-a".into());
        let mut old_b = make_event("b", None);
        old_b.timestamp = "2026-01-01T00:00:01.000Z".into();
        old_b.message = Some("old-b".into());
        write_jsonl(&archive, &[old_a, old_b]);

        let mut new_c = make_event("c", None);
        new_c.timestamp = "2026-06-01T00:00:00.000Z".into();
        new_c.message = Some("new-c".into());
        let mut new_d = make_event("d", None);
        new_d.timestamp = "2026-06-01T00:00:01.000Z".into();
        new_d.message = Some("new-d".into());
        write_jsonl(&active, &[new_c, new_d]);

        // Page 1: limit 2 → newest two events (from active file).
        let page1 = query_log_page(&active, true, &LogFilter::default(), 2, None).unwrap();
        assert_eq!(page1.events.len(), 2);
        assert_eq!(page1.events[0].message.as_deref(), Some("new-d"));
        assert_eq!(page1.events[1].message.as_deref(), Some("new-c"));
        assert!(!page1.at_end, "there are older events in the archive");
        let cursor_wire = page1
            .next_segment_cursor
            .clone()
            .expect("cursor must be set");

        // Page 2: using the cursor → should return the two archive events.
        let cursor = SegmentCursor::from_wire(&cursor_wire).expect("valid cursor wire format");
        let page2 = query_log_page(&active, true, &LogFilter::default(), 2, Some(&cursor)).unwrap();
        assert_eq!(page2.events.len(), 2);
        assert_eq!(page2.events[0].message.as_deref(), Some("old-b"));
        assert_eq!(page2.events[1].message.as_deref(), Some("old-a"));
        assert!(page2.at_end, "no older events remain");
    }

    /// Regression for the active-file rotation race: when the active file is
    /// rotated between two pagination requests, the segment cursor produced on
    /// page 1 names the (now-renamed) file's basename, which is the same as the
    /// new active file. Without the anchor_id check, page 2 would apply the old
    /// byte offset to the new file and return newly-written events instead of
    /// the expected older ones.
    #[test]
    fn segment_cursor_survives_active_file_rotation_between_pages() {
        let tmp = tempfile::tempdir().unwrap();
        let active = tmp.path().join("trace.jsonl");
        let archive = tmp.path().join("trace.20260101-000000.jsonl");

        // Initial active file has 4 events [a, b, c, d].
        let mut ev_a = make_event("a", None);
        ev_a.timestamp = "2026-01-01T00:00:00.000Z".into();
        ev_a.message = Some("ev-a".into());
        let mut ev_b = make_event("b", None);
        ev_b.timestamp = "2026-01-01T00:00:01.000Z".into();
        ev_b.message = Some("ev-b".into());
        let mut ev_c = make_event("c", None);
        ev_c.timestamp = "2026-01-01T00:00:02.000Z".into();
        ev_c.message = Some("ev-c".into());
        let mut ev_d = make_event("d", None);
        ev_d.timestamp = "2026-01-01T00:00:03.000Z".into();
        ev_d.message = Some("ev-d".into());
        write_jsonl(
            &active,
            &[ev_a.clone(), ev_b.clone(), ev_c.clone(), ev_d.clone()],
        );

        // Page 1: limit 2 → returns [d, c] with a cursor pointing into the
        // active file before c (i.e., the cursor anchors on ev_c).
        let page1 = query_log_page(&active, true, &LogFilter::default(), 2, None).unwrap();
        assert_eq!(page1.events.len(), 2);
        assert_eq!(page1.events[0].message.as_deref(), Some("ev-d"));
        assert_eq!(page1.events[1].message.as_deref(), Some("ev-c"));
        let cursor_wire = page1
            .next_segment_cursor
            .clone()
            .expect("cursor must be set");

        // Simulate a rotation: rename the active file to an archive, then write
        // a new event [e] into a fresh active file with the same basename.
        std::fs::rename(&active, &archive).unwrap();
        let mut ev_e = make_event("e", None);
        ev_e.timestamp = "2026-01-01T00:00:04.000Z".into();
        ev_e.message = Some("ev-e".into());
        write_jsonl(&active, &[ev_e.clone()]);

        // Page 2 with the cursor from page 1. The cursor names "trace.jsonl"
        // (now a new file) but anchor_id = ev_c.id. The reader should detect
        // the mismatch and find ev_c in the archive, then return [b, a] — not
        // [e, d] as the broken pre-fix implementation would.
        let cursor = SegmentCursor::from_wire(&cursor_wire).expect("valid cursor");
        let page2 = query_log_page(&active, true, &LogFilter::default(), 2, Some(&cursor)).unwrap();
        assert_eq!(
            page2.events.len(),
            2,
            "expected [b, a] not newly-written events"
        );
        assert_eq!(
            page2.events[0].message.as_deref(),
            Some("ev-b"),
            "oldest-of-page-1 must not duplicate into page 2"
        );
        assert_eq!(page2.events[1].message.as_deref(), Some("ev-a"));
        assert!(page2.at_end, "all events seen");
    }

    #[test]
    fn query_log_page_recovers_when_rotation_races_enumeration() {
        // The stale-segment-list race: a caller enumerates archives, the writer
        // rotates the active file, and only then does the read happen. The
        // archive holding the anchored event did not exist at enumeration time,
        // so a reader that trusts its input list cannot find the anchor and
        // silently restarts from the newest page.
        //
        // `query_log_page` owns enumeration, so its own listing is taken after
        // the rotation and this scenario resolves. The test drives the public
        // entry point exactly as the gateway does: it never hands in a list.
        let tmp = tempfile::tempdir().unwrap();
        let active = tmp.path().join("trace.jsonl");
        let archive = tmp.path().join("trace.20260101-000000.jsonl");

        let mut ev_a = make_event("a", None);
        ev_a.timestamp = "2026-01-01T00:00:00.000Z".into();
        ev_a.message = Some("ev-a".into());
        let mut ev_b = make_event("b", None);
        ev_b.timestamp = "2026-01-01T00:00:01.000Z".into();
        ev_b.message = Some("ev-b".into());
        let mut ev_c = make_event("c", None);
        ev_c.timestamp = "2026-01-01T00:00:02.000Z".into();
        ev_c.message = Some("ev-c".into());
        let mut ev_d = make_event("d", None);
        ev_d.timestamp = "2026-01-01T00:00:03.000Z".into();
        ev_d.message = Some("ev-d".into());
        write_jsonl(&active, &[ev_a, ev_b, ev_c, ev_d]);

        // Page 1 over the active file alone: no archives exist yet, so the
        // cursor anchors on ev_c inside `trace.jsonl`.
        let page1 = query_log_page(&active, true, &LogFilter::default(), 2, None).unwrap();
        assert_eq!(page1.events[0].message.as_deref(), Some("ev-d"));
        assert_eq!(page1.events[1].message.as_deref(), Some("ev-c"));
        let cursor_wire = page1
            .next_segment_cursor
            .clone()
            .expect("cursor must be set");

        // The writer rotates: the file the cursor names becomes an archive, and
        // a brand-new `trace.jsonl` holding unrelated content takes its place.
        std::fs::rename(&active, &archive).unwrap();
        let mut ev_e = make_event("e", None);
        ev_e.timestamp = "2026-01-01T00:00:04.000Z".into();
        ev_e.message = Some("ev-e".into());
        write_jsonl(&active, &[ev_e]);

        // Page 2 must continue into the rotated-away history, not restart from
        // the replacement active file.
        let cursor = SegmentCursor::from_wire(&cursor_wire).expect("valid cursor");
        let page2 = query_log_page(&active, true, &LogFilter::default(), 2, Some(&cursor)).unwrap();
        assert_eq!(
            page2.events.len(),
            2,
            "expected the two older events, got {:?}",
            page2
                .events
                .iter()
                .map(|e| e.message.as_deref())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            page2.events[0].message.as_deref(),
            Some("ev-b"),
            "page 2 must resume below the anchor, not return post-rotation writes"
        );
        assert_eq!(page2.events[1].message.as_deref(), Some("ev-a"));
    }

    #[test]
    fn find_event_across_segments_searches_active_then_archives() {
        let tmp = tempfile::tempdir().unwrap();
        let active = tmp.path().join("trace.jsonl");
        let archive = tmp.path().join("trace.20260101-000000.jsonl");

        let mut archived = make_event("find-test", None);
        archived.id = "archived-id".into();
        archived.message = Some("in-archive".into());
        write_jsonl(&archive, &[archived]);

        let mut live = make_event("find-test", None);
        live.id = "live-id".into();
        live.message = Some("in-active".into());
        write_jsonl(&active, &[live]);

        let hit = find_event_across_segments(&active, true, "live-id").unwrap();
        assert_eq!(
            hit.event.expect("active hit").message.as_deref(),
            Some("in-active")
        );

        let hit = find_event_across_segments(&active, true, "archived-id").unwrap();
        assert_eq!(
            hit.event.expect("archive hit").message.as_deref(),
            Some("in-archive"),
            "an id that has rotated out of the active file must still resolve"
        );

        let miss = find_event_across_segments(&active, true, "no-such-id").unwrap();
        assert!(miss.event.is_none());
        assert!(
            !miss.incomplete,
            "every segment was readable, so the miss is authoritative"
        );
    }

    #[test]
    fn rolling_scope_does_not_merge_orphaned_archives() {
        // `rolling` never creates archives, but a path that previously ran
        // `rotating` still has them on disk, explicitly unmanaged. Merging them
        // into a rolling query would resurrect events the rolling window is
        // supposed to have discarded, and nothing would ever prune them since
        // no rotation runs to trigger retention.
        let tmp = tempfile::tempdir().unwrap();
        let active = tmp.path().join("trace.jsonl");
        let orphan = tmp.path().join("trace.20260101-000000.jsonl");

        let mut old_event = make_event("legacy", None);
        old_event.id = "orphaned".into();
        old_event.timestamp = "2026-01-01T00:00:00.000Z".into();
        old_event.message = Some("from-a-previous-rotating-config".into());
        write_jsonl(&orphan, &[old_event]);

        let mut live = make_event("current", None);
        live.id = "live".into();
        live.timestamp = "2026-01-02T00:00:00.000Z".into();
        live.message = Some("in-the-rolling-window".into());
        write_jsonl(&active, &[live]);

        // reads_archives = false: the rolling scope.
        let page = query_log_page(&active, false, &LogFilter::default(), 10, None).unwrap();
        assert_eq!(
            page.events.len(),
            1,
            "rolling must read the active file alone, got {:?}",
            page.events
                .iter()
                .map(|e| e.message.as_deref())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            page.events[0].message.as_deref(),
            Some("in-the-rolling-window")
        );

        // An id that lives only in the orphaned archive is likewise out of scope.
        assert!(
            find_event_across_segments(&active, false, "orphaned")
                .unwrap()
                .event
                .is_none(),
            "an orphaned archive's events are not part of the rolling stream"
        );

        // The same directory under the rotating scope does merge them, which is
        // what makes the distinction a policy boundary rather than a path one.
        let page = query_log_page(&active, true, &LogFilter::default(), 10, None).unwrap();
        assert_eq!(page.events.len(), 2, "rotating merges retained archives");
        assert!(
            find_event_across_segments(&active, true, "orphaned")
                .unwrap()
                .event
                .is_some()
        );
    }

    #[test]
    fn cursor_into_a_pruned_segment_holds_position_instead_of_restarting() {
        // A client pages back with an archive cursor, and retention deletes that
        // archive before the next request. The sequence number is never reused,
        // so its absence is unambiguous: the history it addressed is gone. The
        // read reports the end of the stream rather than resolving to some other
        // segment, which would silently hand the client the newest events again
        // and read as successful pagination.
        let tmp = tempfile::tempdir().unwrap();
        let active = tmp.path().join("trace.jsonl");
        let archive = tmp.path().join("trace.0000000003-20260101-000000.jsonl");

        let mut old = make_event("a", None);
        old.id = "id-old".into();
        old.timestamp = "2026-01-01T00:00:00.000Z".into();
        write_jsonl(&archive, &[old]);

        let mut live = make_event("b", None);
        live.id = "id-live".into();
        live.timestamp = "2026-06-01T00:00:00.000Z".into();
        write_jsonl(&active, &[live]);

        // A cursor into archive 3 resolves while that archive is present.
        let cursor = SegmentCursor {
            kind: CursorKind::Archive { seq: 3, off: 4096 },
        };
        let page = query_log_page(&active, true, &LogFilter::default(), 5, Some(&cursor)).unwrap();
        assert_eq!(
            page.events.len(),
            1,
            "the addressed archive is present, so its event is returned"
        );

        // Retention prunes it; the same cursor now addresses nothing.
        std::fs::remove_file(&archive).unwrap();
        let page = query_log_page(&active, true, &LogFilter::default(), 5, Some(&cursor)).unwrap();
        assert!(
            page.events.is_empty(),
            "a pruned segment must not resolve to a different one, got {:?}",
            page.events
                .iter()
                .map(|e| e.id.as_str())
                .collect::<Vec<_>>()
        );
        assert!(page.at_end, "the client is at the end of surviving history");
    }

    #[test]
    fn active_cursor_follows_its_anchor_into_the_rotated_archive() {
        // The active file's path is stable but its content is replaced on every
        // rotation, so an offset alone would point into unrelated new content
        // after one. The anchor id is what makes the cursor survive: the reader
        // finds the event in whichever segment now holds it and resumes there.
        let tmp = tempfile::tempdir().unwrap();
        let active = tmp.path().join("trace.jsonl");
        let archive = tmp.path().join("trace.0000000001-20260101-000000.jsonl");

        let mut ev_a = make_event("a", None);
        ev_a.id = "id-a".into();
        ev_a.timestamp = "2026-01-01T00:00:00.000Z".into();
        ev_a.message = Some("old-a".into());
        let mut ev_b = make_event("b", None);
        ev_b.id = "id-b".into();
        ev_b.timestamp = "2026-01-01T00:00:01.000Z".into();
        ev_b.message = Some("old-b".into());
        write_jsonl(&active, &[ev_a, ev_b]);

        // Rotate: the cursor's file becomes an archive and a new active file
        // takes over its basename.
        std::fs::rename(&active, &archive).unwrap();
        let mut ev_c = make_event("c", None);
        ev_c.id = "id-c".into();
        ev_c.timestamp = "2026-06-01T00:00:00.000Z".into();
        ev_c.message = Some("new-c".into());
        write_jsonl(&active, &[ev_c]);

        // The cursor was issued against the pre-rotation active file, anchored
        // on id-b. Only id-a is older than that anchor.
        let cursor = SegmentCursor {
            kind: CursorKind::Active {
                off: 4096,
                anchor_id: Some("id-b".into()),
            },
        };
        let page = query_log_page(&active, true, &LogFilter::default(), 5, Some(&cursor)).unwrap();
        let seen: Vec<&str> = page
            .events
            .iter()
            .map(|e| e.message.as_deref().unwrap_or_default())
            .collect();
        assert_eq!(
            seen,
            vec!["old-a"],
            "the anchor must be found in the archive it rotated into, and the \
             post-rotation active event must not reappear"
        );
    }

    #[test]
    fn segment_order_survives_multiple_rotations_during_one_read() {
        // The case an mtime-derived ordering key cannot handle. Two rotations
        // land while a reader is assembling its snapshot:
        //
        //   open A  ->  A rotates to an archive
        //           ->  B becomes active, then rotates too
        //           ->  C becomes active
        //
        // Ordering by enumeration-time mtime can place the newer archive
        // before the older pinned one, and the reader then reverses the merged
        // result into the wrong newest-first order. The sequence number is
        // written into the name at rotation time, so it describes the order
        // regardless of when or in what order a reader observes the files.
        let tmp = tempfile::tempdir().unwrap();
        let active = tmp.path().join("trace.jsonl");

        // Archives as rotation would leave them, created newest-first on disk
        // so that mtime order is the reverse of true segment order.
        let seg_b = tmp.path().join("trace.0000000002-20260101-000200.jsonl");
        let seg_a = tmp.path().join("trace.0000000001-20260101-000100.jsonl");

        let mut ev_b = make_event("b", None);
        ev_b.id = "id-b".into();
        ev_b.timestamp = "2026-01-01T00:02:00.000Z".into();
        ev_b.message = Some("segment-b".into());
        write_jsonl(&seg_b, &[ev_b]);

        let mut ev_a = make_event("a", None);
        ev_a.id = "id-a".into();
        ev_a.timestamp = "2026-01-01T00:01:00.000Z".into();
        ev_a.message = Some("segment-a".into());
        write_jsonl(&seg_a, &[ev_a]);

        let mut ev_c = make_event("c", None);
        ev_c.id = "id-c".into();
        ev_c.timestamp = "2026-01-01T00:03:00.000Z".into();
        ev_c.message = Some("segment-c".into());
        write_jsonl(&active, &[ev_c]);

        // Force the two keys to disagree, rather than relying on write order
        // and filesystem timestamp resolution to produce a difference. The
        // lower-sequence segment is given the *newer* mtime, so ordering by
        // mtime yields the opposite result to ordering by sequence and the
        // assertion below can tell the two apart.
        let base = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000);
        set_mtime(&seg_b, base);
        set_mtime(&seg_a, base + std::time::Duration::from_secs(3600));
        let a_mtime = std::fs::metadata(&seg_a).unwrap().modified().unwrap();
        let b_mtime = std::fs::metadata(&seg_b).unwrap().modified().unwrap();
        assert!(
            a_mtime > b_mtime,
            "test setup: the lower-sequence segment must carry the newer mtime"
        );

        let page = query_log_page(&active, true, &LogFilter::default(), 10, None).unwrap();
        let seen: Vec<&str> = page
            .events
            .iter()
            .map(|e| e.message.as_deref().unwrap_or_default())
            .collect();
        assert_eq!(
            seen,
            vec!["segment-c", "segment-b", "segment-a"],
            "segments must be merged newest-first by sequence, not by mtime"
        );
    }

    #[test]
    fn legacy_archives_sort_before_numbered_ones() {
        // Archives written before sequence numbering existed carry no number.
        // They can only predate the upgrade, so they belong at the start of the
        // stream regardless of what their mtimes say.
        let tmp = tempfile::tempdir().unwrap();
        let active = tmp.path().join("trace.jsonl");
        let legacy = tmp.path().join("trace.20260101-000000.jsonl");
        let numbered = tmp.path().join("trace.0000000001-20260101-000100.jsonl");

        let mut ev_old = make_event("old", None);
        ev_old.id = "id-old".into();
        ev_old.timestamp = "2026-01-01T00:00:00.000Z".into();
        ev_old.message = Some("legacy".into());
        write_jsonl(&legacy, &[ev_old]);

        let mut ev_new = make_event("new", None);
        ev_new.id = "id-new".into();
        ev_new.timestamp = "2026-01-01T00:01:00.000Z".into();
        ev_new.message = Some("numbered".into());
        write_jsonl(&numbered, &[ev_new]);

        let mut ev_live = make_event("live", None);
        ev_live.id = "id-live".into();
        ev_live.timestamp = "2026-01-01T00:02:00.000Z".into();
        ev_live.message = Some("active".into());
        write_jsonl(&active, &[ev_live]);

        let page = query_log_page(&active, true, &LogFilter::default(), 10, None).unwrap();
        let seen: Vec<&str> = page
            .events
            .iter()
            .map(|e| e.message.as_deref().unwrap_or_default())
            .collect();
        assert_eq!(
            seen,
            vec!["active", "numbered", "legacy"],
            "a legacy archive must read as older than every numbered archive"
        );
    }
    #[test]
    fn legacy_archive_pages_all_the_way_through_its_older_rows() {
        // A legacy archive has no sequence number in its name, so for a while
        // it could not issue a pagination cursor at all. Reporting `at_end`
        // there stranded every row the page limit had evicted: a three-row
        // archive read with `limit = 2` returned the newest two and claimed the
        // history was finished. A cursor addressed by filename fixes that, and
        // this walks the whole archive to prove no row is unreachable.
        let tmp = tempfile::tempdir().unwrap();
        let active = tmp.path().join("trace.jsonl");
        let legacy = tmp.path().join("trace.20260101-000000.jsonl");

        let mut rows = Vec::new();
        for i in 0..3 {
            let mut ev = make_event("legacy", None);
            ev.id = format!("id-{i}");
            ev.timestamp = format!("2026-01-01T00:00:0{i}.000Z");
            ev.message = Some(format!("legacy-{i}"));
            rows.push(ev);
        }
        write_jsonl(&legacy, &rows);

        let mut live = make_event("live", None);
        live.id = "id-live".into();
        live.timestamp = "2026-06-01T00:00:00.000Z".into();
        live.message = Some("active".into());
        write_jsonl(&active, &[live]);

        // Page back two rows at a time, following the cursor until the reader
        // says the walk is over.
        let mut seen: Vec<String> = Vec::new();
        let mut cursor: Option<SegmentCursor> = None;
        for _ in 0..8 {
            let page =
                query_log_page(&active, true, &LogFilter::default(), 2, cursor.as_ref()).unwrap();
            seen.extend(
                page.events
                    .iter()
                    .map(|e| e.message.as_deref().unwrap_or_default().to_owned()),
            );
            if page.at_end {
                break;
            }
            let wire = page
                .next_segment_cursor
                .expect("a page that is not at_end must carry a cursor to continue");
            cursor = Some(SegmentCursor::from_wire(&wire).expect("reader emitted a valid cursor"));
        }

        assert_eq!(
            seen,
            vec!["active", "legacy-2", "legacy-1", "legacy-0"],
            "every row must be reachable by paging, including the oldest in a \
             legacy archive"
        );
    }

    #[test]
    fn legacy_name_cursor_resolves_against_the_active_file_too() {
        // The pre-sequence wire form `<basename>:<off>` is ambiguous by
        // construction: an older daemon issued it for the active file, and it
        // is also how a legacy archive is addressed. Both are still in
        // circulation, so resolution has to try archives and then the active
        // file. Matching only archives made every older active cursor resolve
        // to nothing, which surfaced as an empty at-end page and silently ended
        // the client's walk.
        let tmp = tempfile::tempdir().unwrap();
        let active = tmp.path().join("trace.jsonl");

        let mut ev_a = make_event("a", None);
        ev_a.id = "id-a".into();
        ev_a.timestamp = "2026-01-01T00:00:00.000Z".into();
        ev_a.message = Some("older".into());
        let mut ev_b = make_event("b", None);
        ev_b.id = "id-b".into();
        ev_b.timestamp = "2026-01-01T00:00:01.000Z".into();
        ev_b.message = Some("newer".into());
        write_jsonl(&active, &[ev_a, ev_b]);

        // Take the real byte boundary between the two events so the cursor
        // addresses a position the reader will actually honour.
        let first_page = query_log_page(&active, true, &LogFilter::default(), 1, None).unwrap();
        let boundary = first_page
            .next_cursor_line_offset
            .expect("an active-file page carries a byte offset");

        // An older daemon would have issued exactly this token.
        let legacy = SegmentCursor::from_wire(&format!("trace.jsonl:{boundary}"))
            .expect("the two-field form must still parse");
        let page = query_log_page(&active, true, &LogFilter::default(), 10, Some(&legacy)).unwrap();

        let seen: Vec<&str> = page
            .events
            .iter()
            .map(|e| e.message.as_deref().unwrap_or_default())
            .collect();
        assert_eq!(
            seen,
            vec!["older"],
            "a legacy cursor naming the active file must resolve against it, \
             not fall through to an empty at-end page"
        );
    }

    #[test]
    fn cursor_wire_format_round_trips_and_rejects_malformed_tokens() {
        // Archive and active cursors carry distinct fixed prefixes. Both forms
        // must survive a round trip unchanged so a client that echoes the token
        // back lands on the same position.
        let archive = SegmentCursor {
            kind: CursorKind::Archive { seq: 7, off: 4096 },
        };
        assert_eq!(archive.to_wire(), "archive:7:4096");
        assert_eq!(SegmentCursor::from_wire("archive:7:4096"), Some(archive));

        let active = SegmentCursor {
            kind: CursorKind::Active {
                off: 512,
                anchor_id: Some("evt-1".into()),
            },
        };
        assert_eq!(active.to_wire(), "active:512:evt-1");
        assert_eq!(SegmentCursor::from_wire("active:512:evt-1"), Some(active));

        // An anchorless active cursor is the legacy shape and stays supported.
        let bare = SegmentCursor {
            kind: CursorKind::Active {
                off: 512,
                anchor_id: None,
            },
        };
        assert_eq!(bare.to_wire(), "active:512");
        assert_eq!(SegmentCursor::from_wire("active:512"), Some(bare));

        // A trailing colon with no anchor is malformed. Accepting it would
        // silently normalise to `active:512` and hide a client-side bug, so the
        // parser rejects it and the HTTP/RPC layers turn that into an
        // invalid-parameter response.
        assert_eq!(SegmentCursor::from_wire("active:512:"), None);

        // A two-field token still belongs to the legacy basename grammar, even
        // when that basename is literally `archive`.
        assert_eq!(
            SegmentCursor::from_wire("archive:7").map(|cursor| cursor.kind),
            Some(CursorKind::LegacyArchive {
                name: "archive".into(),
                off: 7,
            }),
            "the new prefix must not invalidate a legacy cursor whose basename \
             is literally `archive`"
        );
        // Once the second colon selects the explicit archive grammar, malformed
        // fields are rejected rather than falling through to the legacy form.
        assert_eq!(SegmentCursor::from_wire("archive:7:"), None);
        assert_eq!(SegmentCursor::from_wire("archive::4096"), None);
        assert_eq!(SegmentCursor::from_wire("archive:abc:4096"), None);
        assert_eq!(SegmentCursor::from_wire("archive:7:abc"), None);
        assert_eq!(SegmentCursor::from_wire("archive:7:4096:extra"), None);

        // Non-numeric offsets are rejected in active and legacy forms too.
        assert_eq!(SegmentCursor::from_wire("active:abc"), None);
        assert_eq!(SegmentCursor::from_wire("active:abc:evt-1"), None);
        assert_eq!(SegmentCursor::from_wire("trace.jsonl:abc"), None);
        assert_eq!(SegmentCursor::from_wire(""), None);
    }

    #[test]
    fn query_returns_all_segments_even_when_active_is_empty_after_rotation() {
        // After a rotation the active file is empty (or has very few events)
        // and all pre-rotation events live in a numbered archive. A cursorless
        // request that sees only the new empty active on the first scan would
        // report `at_end: true` with zero events while history is sitting in
        // the archive. This verifies that the archive is included in the result
        // regardless — which the re-enumeration retry defends against in the
        // concurrent case and which is the baseline for any non-racy layout.
        let tmp = tempfile::tempdir().unwrap();
        let active = tmp.path().join("trace.jsonl");
        let archive = tmp.path().join("trace.0000000001-20260101-000000.jsonl");

        let mut ev_a = make_event("a", None);
        ev_a.id = "id-a".into();
        ev_a.timestamp = "2026-01-01T00:00:00.000Z".into();
        ev_a.message = Some("archived-a".into());
        let mut ev_b = make_event("b", None);
        ev_b.id = "id-b".into();
        ev_b.timestamp = "2026-01-01T00:00:01.000Z".into();
        ev_b.message = Some("archived-b".into());
        write_jsonl(&archive, &[ev_a, ev_b]);

        // Empty active file — the state right after a rotation.
        std::fs::write(&active, b"").unwrap();

        let page = query_log_page(&active, true, &LogFilter::default(), 10, None).unwrap();
        let messages: Vec<&str> = page
            .events
            .iter()
            .map(|e| e.message.as_deref().unwrap_or_default())
            .collect();
        assert_eq!(
            messages,
            vec!["archived-b", "archived-a"],
            "archive events must appear even when the active file is empty"
        );
        assert!(page.at_end);
    }

    #[test]
    fn re_enumeration_detects_new_segment_that_appeared_after_first_snapshot() {
        // Proves that `enumerate_segment_metas` produces a larger list when a
        // new numbered archive exists on the second call, which is exactly the
        // condition the retry uses to decide whether to re-scan. This is a
        // direct property test of the detection mechanism rather than of the
        // concurrent race (which cannot be injected deterministically).
        let tmp = tempfile::tempdir().unwrap();
        let active = tmp.path().join("trace.jsonl");

        let mut live = make_event("live", None);
        live.id = "id-live".into();
        live.message = Some("active".into());
        write_jsonl(&active, &[live]);

        let before = enumerate_segment_metas(&active, true, &mut false).unwrap();
        assert_eq!(before.len(), 1, "only the active file before rotation");

        // Simulate rotation: the old active becomes a numbered archive and
        // a fresh active file is written.
        let archive = tmp.path().join("trace.0000000001-20260101-000000.jsonl");
        std::fs::rename(&active, &archive).unwrap();
        let mut new_live = make_event("new", None);
        new_live.id = "id-new".into();
        new_live.message = Some("new-active".into());
        write_jsonl(&active, &[new_live]);

        let after = enumerate_segment_metas(&active, true, &mut false).unwrap();
        assert_eq!(
            after.len(),
            2,
            "the archive created by rotation must appear in the second enumeration"
        );
        assert!(
            after.iter().any(|s| s.path == archive),
            "the rotated archive must be in the new snapshot"
        );
        // The retry condition `segs2.len() > segs.len()` holds.
        assert!(after.len() > before.len());
    }

    #[test]
    fn a_query_holds_one_descriptor_regardless_of_segment_count() {
        // `retention_max_files = 0` means "keep everything", so nothing bounds
        // how many archives a long-lived instance accumulates. Segments are
        // addressed by path and opened one at a time during the scan, so the
        // descriptor cost of a read does not grow with that count. sd-journal
        // held every segment open at once and shipped fd-exhaustion reports
        // until it was capped; this reader has no cap because it needs none.
        let tmp = tempfile::tempdir().unwrap();
        let active = tmp.path().join("trace.jsonl");

        for seq in 1..=40u64 {
            let path = tmp
                .path()
                .join(format!("trace.{seq:010}-20260101-000000.jsonl"));
            let mut ev = make_event("archived", None);
            ev.id = format!("id-{seq}");
            ev.timestamp = format!("2026-01-01T00:{:02}:00.000Z", seq % 60);
            ev.message = Some(format!("seg-{seq}"));
            write_jsonl(&path, &[ev]);
        }
        let mut live = make_event("live", None);
        live.id = "id-live".into();
        live.timestamp = "2026-06-01T00:00:00.000Z".into();
        live.message = Some("active".into());
        write_jsonl(&active, &[live]);

        let segs = enumerate_segment_metas(&active, true, &mut false).unwrap();
        assert_eq!(segs.len(), 41, "every segment is in scope, uncapped");
        assert!(segs.last().unwrap().is_active, "the active file sorts last");

        // Every segment is readable in one page: no archive is dropped to stay
        // under a descriptor budget.
        let page = query_log_page(&active, true, &LogFilter::default(), 100, None).unwrap();
        assert_eq!(
            page.events.len(),
            41,
            "all 41 events must be reachable in a single read"
        );
        assert_eq!(page.events[0].message.as_deref(), Some("active"));
        assert_eq!(
            page.events.last().unwrap().message.as_deref(),
            Some("seg-1"),
            "the oldest archive is still reached"
        );
        assert!(page.at_end);
    }

    #[test]
    fn archives_are_out_of_scope_for_a_policy_that_does_not_own_them() {
        // Only `rotating` creates and prunes archives. Every other policy reads
        // the active file alone, even when timestamped files from an earlier
        // `rotating` configuration are still sitting beside it — merging them
        // would resurrect events the active policy is supposed to have dropped,
        // and nothing would ever prune them.
        let tmp = tempfile::tempdir().unwrap();
        let active = tmp.path().join("trace.jsonl");
        for seq in 1..=12u64 {
            let path = tmp
                .path()
                .join(format!("trace.{seq:010}-20260101-000000.jsonl"));
            let mut ev = make_event("archived", None);
            ev.id = format!("id-{seq}");
            ev.message = Some(format!("seg-{seq}"));
            write_jsonl(&path, &[ev]);
        }
        let mut live = make_event("live", None);
        live.id = "id-live".into();
        live.message = Some("active".into());
        write_jsonl(&active, &[live]);

        let segs = enumerate_segment_metas(&active, false, &mut false).unwrap();
        assert_eq!(segs.len(), 1, "only the active file is in scope");
        assert!(segs[0].is_active);

        let page = query_log_page(&active, false, &LogFilter::default(), 100, None).unwrap();
        let seen: Vec<&str> = page
            .events
            .iter()
            .map(|e| e.message.as_deref().unwrap_or_default())
            .collect();
        assert_eq!(seen, vec!["active"], "archive events must stay invisible");
    }

    /// Install the enumeration seam for one test, removing it on drop so a
    /// panicking test cannot leave it armed for the next one on this thread.
    #[must_use]
    struct EnumerateHook;

    impl EnumerateHook {
        fn install(callback: impl FnMut() + 'static) -> Self {
            AFTER_ENUMERATE.with(|slot| *slot.borrow_mut() = Some(Box::new(callback)));
            Self
        }
    }

    impl Drop for EnumerateHook {
        fn drop(&mut self) {
            AFTER_ENUMERATE.with(|slot| *slot.borrow_mut() = None);
        }
    }

    #[test]
    fn a_rotation_between_the_listing_and_the_read_does_not_lose_a_segment() {
        // Enumeration and reading are not atomic. A rotation landing between
        // them turns the listed active file into an archive the listing does
        // not have, and the scan reads the fresh replacement instead. The
        // rotated-away events then sit unlisted between two segments the page
        // did read, and since pagination only walks toward older history, a
        // cursor from this page can never come back for them.
        //
        // The rotation is driven from inside that window rather than before
        // the call, because a rotation that has already finished is visible to
        // the very first listing and would exercise nothing.
        let tmp = tempfile::tempdir().unwrap();
        let active = tmp.path().join("trace.jsonl");

        // Old archive big enough to fill the page on its own, which is what
        // let the stale page look like an ordinary non-terminal one.
        let mut bulk = String::new();
        for i in 0..20u32 {
            let mut ev = make_event("x", None);
            ev.id = format!("old-{i}");
            ev.timestamp = format!("2026-01-01T00:00:{i:02}.000Z");
            ev.message = Some(format!("old-{i}"));
            bulk.push_str(&serde_json::to_string(&ev).unwrap());
            bulk.push('\n');
        }
        std::fs::write(
            tmp.path().join("trace.0000000001-20260101-000000.jsonl"),
            &bulk,
        )
        .unwrap();

        let mut doomed = make_event("x", None);
        doomed.id = "doomed".into();
        doomed.timestamp = "2026-01-01T01:00:00.000Z".into();
        doomed.message = Some("ROTATED-AWAY".into());
        write_jsonl(&active, &[doomed]);

        // Rotate once, on the first enumeration only: the redo must succeed
        // against a settled directory rather than chasing a moving target.
        let rotate_dir = tmp.path().to_path_buf();
        let mut fired = false;
        let _hook = EnumerateHook::install(move || {
            if fired {
                return;
            }
            fired = true;
            let active = rotate_dir.join("trace.jsonl");
            std::fs::rename(
                &active,
                rotate_dir.join("trace.0000000002-20260101-010000.jsonl"),
            )
            .unwrap();
            let mut replacement = make_event("x", None);
            replacement.id = "fresh".into();
            replacement.timestamp = "2026-01-01T02:00:00.000Z".into();
            replacement.message = Some("new-active".into());
            write_jsonl(&active, &[replacement]);
        });

        let page = query_log_page(&active, true, &LogFilter::default(), 2, None).unwrap();
        let seen: Vec<&str> = page
            .events
            .iter()
            .map(|e| e.message.as_deref().unwrap_or_default())
            .collect();
        assert_eq!(
            seen,
            vec!["new-active", "ROTATED-AWAY"],
            "the segment created by the rotation must be in the page, not \
             stranded between two segments it did read"
        );
    }

    #[test]
    fn query_refuses_a_page_when_its_snapshot_never_stabilizes() {
        // Every attempt reads a replacement active file while the content it
        // enumerated moves into a newly created, unlisted archive. Returning
        // any one of those pages as successful would permanently strand the
        // omitted segment behind an older page cursor.
        let tmp = tempfile::tempdir().unwrap();
        let active = tmp.path().join("trace.jsonl");

        let mut initial = make_event("x", None);
        initial.id = "initial".into();
        initial.message = Some("initial-active".into());
        write_jsonl(&active, &[initial]);

        let rotate_dir = tmp.path().to_path_buf();
        let rotations = std::rc::Rc::new(std::cell::Cell::new(0u64));
        let hook_rotations = std::rc::Rc::clone(&rotations);
        let _hook = EnumerateHook::install(move || {
            let seq = hook_rotations.get() + 1;
            hook_rotations.set(seq);

            let active = rotate_dir.join("trace.jsonl");
            std::fs::rename(
                &active,
                rotate_dir.join(format!("trace.{seq:010}-20260101-000000.jsonl")),
            )
            .unwrap();
            let mut replacement = make_event("x", None);
            replacement.id = format!("replacement-{seq}");
            replacement.message = Some(format!("replacement-{seq}"));
            write_jsonl(&active, &[replacement]);
        });

        let err = query_log_page(&active, true, &LogFilter::default(), 1, None)
            .expect_err("a page from a known-stale snapshot must not be returned");
        assert_eq!(
            rotations.get(),
            MAX_SNAPSHOT_ATTEMPTS as u64,
            "the reader must exhaust its bounded retry budget"
        );
        assert_eq!(
            err.to_string(),
            "log segment snapshot did not stabilize after 3 attempts"
        );
    }

    #[test]
    fn lookup_retries_when_rotation_replaces_active_after_enumeration() {
        // Opening the listed active path is not enough to prove the lookup saw
        // the listed content: rotation can rename that file and create a new
        // one at the same path before the open. The open then succeeds against
        // the replacement while the target sits in a new, unlisted archive.
        let tmp = tempfile::tempdir().unwrap();
        let active = tmp.path().join("trace.jsonl");

        let mut target = make_event("x", None);
        target.id = "rotated-target".into();
        target.message = Some("moved-during-lookup".into());
        write_jsonl(&active, &[target]);

        let rotate_dir = tmp.path().to_path_buf();
        let mut fired = false;
        let _hook = EnumerateHook::install(move || {
            if fired {
                return;
            }
            fired = true;
            let active = rotate_dir.join("trace.jsonl");
            std::fs::rename(
                &active,
                rotate_dir.join("trace.0000000001-20260101-000000.jsonl"),
            )
            .unwrap();
            let mut replacement = make_event("x", None);
            replacement.id = "replacement".into();
            replacement.message = Some("new-active".into());
            write_jsonl(&active, &[replacement]);
        });

        let hit = find_event_across_segments(&active, true, "rotated-target").unwrap();
        assert_eq!(
            hit.event
                .expect("lookup must retry against the new archive")
                .message
                .as_deref(),
            Some("moved-during-lookup")
        );
        assert!(!hit.incomplete, "a successful retry is authoritative");
    }

    #[test]
    fn a_numeric_log_basename_does_not_strand_an_older_daemons_active_cursor() {
        // An older daemon paginating a log file literally named `123` issued
        // `123:<off>` meaning the active file. Numbered archives now use an
        // explicit `archive:` prefix, so the old token remains a name-addressed
        // cursor even though its basename happens to be all digits.
        let tmp = tempfile::tempdir().unwrap();
        let active = tmp.path().join("123");

        let mut older = make_event("x", None);
        older.id = "id-older".into();
        older.timestamp = "2026-01-01T00:00:00.000Z".into();
        older.message = Some("older".into());
        let mut newer = make_event("x", None);
        newer.id = "id-newer".into();
        newer.timestamp = "2026-01-01T00:00:01.000Z".into();
        newer.message = Some("newer".into());
        write_jsonl(&active, &[older, newer]);

        // The byte boundary an older daemon would have handed out.
        let first = query_log_page(&active, true, &LogFilter::default(), 1, None).unwrap();
        let boundary = first
            .next_cursor_line_offset
            .expect("an active-file page carries a byte offset");

        let cursor = SegmentCursor::from_wire(&format!("123:{boundary}"))
            .expect("the two-field form must parse");
        assert_eq!(
            cursor.kind,
            CursorKind::LegacyArchive {
                name: "123".into(),
                off: boundary
            },
            "an unprefixed numeric token is a legacy basename, not an archive"
        );

        let page = query_log_page(&active, true, &LogFilter::default(), 10, Some(&cursor)).unwrap();
        let seen: Vec<&str> = page
            .events
            .iter()
            .map(|e| e.message.as_deref().unwrap_or_default())
            .collect();
        assert_eq!(
            seen,
            vec!["older"],
            "the cursor must resolve against the active file whose name is that \
             number, not fall through to an empty page"
        );
    }

    #[test]
    fn a_pruned_archive_cursor_cannot_rebind_to_a_numeric_active_basename() {
        // The explicit archive identity must stay authoritative after
        // retention deletes the segment. In particular, sequence 123 must not
        // fall back to a current active file that happens to be named `123`, or
        // pagination would silently jump into unrelated content.
        let tmp = tempfile::tempdir().unwrap();
        let active = tmp.path().join("123");
        write_jsonl(&active, &[make_event("x", None)]);

        let cursor = SegmentCursor::from_wire("archive:123:0").expect("archive cursor parses");
        let page = query_log_page(&active, true, &LogFilter::default(), 10, Some(&cursor)).unwrap();
        assert!(page.events.is_empty());
        assert!(
            page.at_end,
            "a cursor naming a pruned archive must hold the client where it is"
        );
    }

    #[test]
    fn an_active_cursor_with_an_extra_field_is_rejected() {
        // Anchors are event ids, which are UUIDs and never contain a colon. An
        // extra field means the token was hand-built or corrupted, and folding
        // the remainder into the anchor would resolve against an id that was
        // never issued.
        assert!(
            SegmentCursor::from_wire("active:10:abc:def").is_none(),
            "an anchor cannot contain a colon"
        );
        // The well-formed shapes still parse.
        assert_eq!(
            SegmentCursor::from_wire("active:10:abc").map(|c| c.kind),
            Some(CursorKind::Active {
                off: 10,
                anchor_id: Some("abc".into())
            })
        );
        assert_eq!(
            SegmentCursor::from_wire("active:10").map(|c| c.kind),
            Some(CursorKind::Active {
                off: 10,
                anchor_id: None
            })
        );
    }

    #[test]
    fn archive_set_moved_ignores_the_active_file() {
        // The active path never changes name, so it carries no signal. Only
        // archives appearing or disappearing mean the world moved.
        let tmp = tempfile::tempdir().unwrap();
        let active = tmp.path().join("trace.jsonl");
        write_jsonl(&active, &[make_event("a", None)]);
        std::fs::write(
            tmp.path().join("trace.0000000001-20260101-000000.jsonl"),
            "x\n",
        )
        .unwrap();

        let segs = enumerate_segment_metas(&active, true, &mut false).unwrap();
        assert!(!archive_set_moved(&active, &segs).unwrap());

        // Appending to the active file is not a move.
        write_jsonl(&active, &[make_event("a", None), make_event("b", None)]);
        assert!(!archive_set_moved(&active, &segs).unwrap());

        // Retention deleting an archive is, even though nothing was added.
        std::fs::remove_file(tmp.path().join("trace.0000000001-20260101-000000.jsonl")).unwrap();
        assert!(archive_set_moved(&active, &segs).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_archive_keeps_the_rest_of_the_page_but_marks_it_incomplete() {
        // One bad file should not cost the caller every other segment, so the
        // page is still returned. But `at_end` is then only "no older events
        // among the segments that could be read", and a client that stops
        // paging on it would present a page with a hole as the whole history.
        // `incomplete` is what reaches that client; the warn log only reaches
        // the operator.
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let active = tmp.path().join("trace.jsonl");
        let archive = tmp.path().join("trace.0000000001-20260101-000000.jsonl");

        let mut buried = make_event("x", None);
        buried.id = "buried".into();
        buried.timestamp = "2026-01-01T00:00:00.000Z".into();
        buried.message = Some("in-the-unreadable-archive".into());
        write_jsonl(&archive, &[buried]);

        let mut live = make_event("x", None);
        live.id = "live".into();
        live.timestamp = "2026-01-01T01:00:00.000Z".into();
        live.message = Some("in-the-active-file".into());
        write_jsonl(&active, &[live]);

        std::fs::set_permissions(&archive, std::fs::Permissions::from_mode(0o000)).unwrap();
        if File::open(&archive).is_ok() {
            // Running as root, where mode bits do not deny the open. There is
            // nothing to assert about a failure that cannot be produced.
            return;
        }

        let page = query_log_page(&active, true, &LogFilter::default(), 10, None).unwrap();
        let seen: Vec<&str> = page
            .events
            .iter()
            .map(|e| e.message.as_deref().unwrap_or_default())
            .collect();
        assert_eq!(
            seen,
            vec!["in-the-active-file"],
            "the readable segments must still be returned"
        );
        assert!(
            page.incomplete,
            "a skipped segment must be visible to the caller, not only in the log"
        );

        // A lookup that misses is likewise not authoritative: the id could be
        // sitting in the segment that could not be opened.
        let miss = find_event_across_segments(&active, true, "buried").unwrap();
        assert!(miss.event.is_none());
        assert!(
            miss.incomplete,
            "a miss over an unread segment must not be reported as `not found`"
        );

        // A hit is authoritative however many other segments were skipped.
        let hit = find_event_across_segments(&active, true, "live").unwrap();
        assert!(hit.event.is_some());
        assert!(
            !hit.incomplete,
            "finding the event answers the question regardless of what was skipped"
        );

        // Leave the archive removable by the tempdir drop.
        std::fs::set_permissions(&archive, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_wearing_an_archive_name_is_not_read() {
        // Matching the writer's filename shape is not path confinement. A local
        // actor who can write into the log directory could otherwise point an
        // archive-shaped name at any file the daemon can read and have an
        // authenticated log query hand back its contents.
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let secret = outside.join("secret.jsonl");

        let mut leaked = make_event("x", None);
        leaked.id = "leaked".into();
        leaked.message = Some("NOT-PART-OF-THE-LOG".into());
        write_jsonl(&secret, &[leaked]);

        let dir = tmp.path().join("logs");
        std::fs::create_dir(&dir).unwrap();
        let active = dir.join("trace.jsonl");
        let mut live = make_event("x", None);
        live.id = "live".into();
        live.message = Some("in-the-active-file".into());
        write_jsonl(&active, &[live]);

        std::os::unix::fs::symlink(&secret, dir.join("trace.0000000001-20260101-000000.jsonl"))
            .unwrap();

        let page = query_log_page(&active, true, &LogFilter::default(), 10, None).unwrap();
        let seen: Vec<&str> = page
            .events
            .iter()
            .map(|e| e.message.as_deref().unwrap_or_default())
            .collect();
        assert_eq!(
            seen,
            vec!["in-the-active-file"],
            "a symlinked archive name must not pull in a file from elsewhere"
        );
        assert!(
            !page.incomplete,
            "a rejected symlink is not a segment of this stream, so the page is \
             still a complete view of it"
        );

        assert!(
            find_event_across_segments(&active, true, "leaked")
                .unwrap()
                .event
                .is_none(),
            "the id lookup must respect the same boundary as the page query"
        );
    }
}
