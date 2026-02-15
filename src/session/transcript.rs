//! JSONL transcript persistence for Aria agent sessions.
//!
//! Transcripts are stored as JSONL files where the first line is always a
//! [`SessionHeader`] and subsequent lines are [`AgentMessage`] entries.
//! A [`SessionStore`] manages the `sessions.json` index for listing and
//! looking up sessions by ID.

use super::types::{AgentMessage, NormalizedUsage, SessionEntry, SessionHeader};

use anyhow::{Context, Result};
use serde_json;
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

// ── TranscriptWriter ─────────────────────────────────────────────

/// Appends agent messages to a JSONL transcript file.
///
/// The writer creates the file on `open`, writes the session header as the
/// first line, and provides `append` / `append_batch` for subsequent messages.
/// All write operations hold a mutex so the writer is safe to share across
/// threads via `Arc<TranscriptWriter>`.
pub struct TranscriptWriter {
    path: PathBuf,
    file: Mutex<Option<BufWriter<File>>>,
}

impl TranscriptWriter {
    /// Create a new transcript file at `path`, writing `header` as the first JSONL line.
    ///
    /// Parent directories are created automatically.
    pub fn open(path: &Path, header: &SessionHeader) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating transcript directory {}", parent.display()))?;
        }

        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .with_context(|| format!("opening transcript file {}", path.display()))?;

        let mut writer = BufWriter::new(file);

        let header_json = serde_json::to_string(header).context("serializing session header")?;
        writeln!(writer, "{header_json}").context("writing session header")?;
        writer.flush().context("flushing session header")?;

        Ok(Self {
            path: path.to_path_buf(),
            file: Mutex::new(Some(writer)),
        })
    }

    /// Serialize `message` as JSON and append it as a single JSONL line.
    pub fn append(&self, message: &AgentMessage) -> Result<()> {
        let line = serde_json::to_string(message).context("serializing agent message")?;

        let mut guard = self
            .file
            .lock()
            .map_err(|e| anyhow::anyhow!("transcript writer lock poisoned: {e}"))?;

        let writer = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("transcript writer already closed"))?;

        writeln!(writer, "{line}")
            .with_context(|| format!("appending to transcript {}", self.path.display()))?;
        writer.flush().context("flushing transcript")?;

        Ok(())
    }

    /// Append multiple messages in a single lock acquisition.
    pub fn append_batch(&self, messages: &[AgentMessage]) -> Result<()> {
        if messages.is_empty() {
            return Ok(());
        }

        let mut guard = self
            .file
            .lock()
            .map_err(|e| anyhow::anyhow!("transcript writer lock poisoned: {e}"))?;

        let writer = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("transcript writer already closed"))?;

        for message in messages {
            let line =
                serde_json::to_string(message).context("serializing agent message in batch")?;
            writeln!(writer, "{line}").with_context(|| {
                format!("appending batch to transcript {}", self.path.display())
            })?;
        }

        writer.flush().context("flushing transcript after batch")?;

        Ok(())
    }

    /// Force an fsync on the underlying file descriptor.
    pub fn sync(&self) -> Result<()> {
        let mut guard = self
            .file
            .lock()
            .map_err(|e| anyhow::anyhow!("transcript writer lock poisoned: {e}"))?;

        let writer = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("transcript writer already closed"))?;

        writer.flush().context("flushing before sync")?;
        writer
            .get_ref()
            .sync_all()
            .context("fsync transcript file")?;

        Ok(())
    }
}

// ── TranscriptReader ─────────────────────────────────────────────

/// Reads an existing JSONL transcript file.
///
/// Provides methods to read the header, iterate messages, paginate, count,
/// and tail.
pub struct TranscriptReader {
    path: PathBuf,
}

impl TranscriptReader {
    /// Open an existing transcript file for reading.
    pub fn open(path: &Path) -> Result<Self> {
        if !path.exists() {
            anyhow::bail!("transcript file does not exist: {}", path.display());
        }
        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    /// Read and deserialize the session header (first JSONL line).
    pub fn read_header(&self) -> Result<SessionHeader> {
        let file = File::open(&self.path)
            .with_context(|| format!("opening transcript {}", self.path.display()))?;
        let reader = BufReader::new(file);
        let first_line = reader.lines().next().ok_or_else(|| {
            anyhow::anyhow!("transcript file is empty: {}", self.path.display())
        })??;

        serde_json::from_str(&first_line).context("deserializing session header")
    }

    /// Read all agent messages (everything after the header line).
    pub fn read_messages(&self) -> Result<Vec<AgentMessage>> {
        let file = File::open(&self.path)
            .with_context(|| format!("opening transcript {}", self.path.display()))?;
        let reader = BufReader::new(file);

        let mut messages = Vec::new();
        for (i, line) in reader.lines().enumerate() {
            let line = line.context("reading transcript line")?;
            if i == 0 {
                continue; // skip header
            }
            if line.trim().is_empty() {
                continue;
            }
            let msg: AgentMessage = serde_json::from_str(&line)
                .with_context(|| format!("deserializing message at line {}", i + 1))?;
            messages.push(msg);
        }

        Ok(messages)
    }

    /// Read a paginated range of messages.
    ///
    /// `offset` is zero-based relative to the first message (not the header).
    /// Returns up to `limit` messages starting from `offset`.
    pub fn read_messages_range(&self, offset: usize, limit: usize) -> Result<Vec<AgentMessage>> {
        let file = File::open(&self.path)
            .with_context(|| format!("opening transcript {}", self.path.display()))?;
        let reader = BufReader::new(file);

        let mut messages = Vec::new();
        let mut msg_index: usize = 0;

        for (i, line) in reader.lines().enumerate() {
            let line = line.context("reading transcript line")?;
            if i == 0 {
                continue; // skip header
            }
            if line.trim().is_empty() {
                continue;
            }

            if msg_index >= offset && messages.len() < limit {
                let msg: AgentMessage = serde_json::from_str(&line)
                    .with_context(|| format!("deserializing message at line {}", i + 1))?;
                messages.push(msg);
            }

            msg_index += 1;

            if messages.len() >= limit {
                break;
            }
        }

        Ok(messages)
    }

    /// Count the number of messages without fully deserializing them.
    ///
    /// Counts all non-empty lines after the header.
    pub fn message_count(&self) -> Result<usize> {
        let file = File::open(&self.path)
            .with_context(|| format!("opening transcript {}", self.path.display()))?;
        let reader = BufReader::new(file);

        let mut count = 0usize;
        for (i, line) in reader.lines().enumerate() {
            let line = line.context("reading transcript line")?;
            if i == 0 {
                continue; // skip header
            }
            if !line.trim().is_empty() {
                count += 1;
            }
        }

        Ok(count)
    }

    /// Return the last `n` messages from the transcript.
    ///
    /// If fewer than `n` messages exist, returns all available messages.
    pub fn tail(&self, n: usize) -> Result<Vec<AgentMessage>> {
        let file = File::open(&self.path)
            .with_context(|| format!("opening transcript {}", self.path.display()))?;
        let reader = BufReader::new(file);

        // Collect all non-header, non-empty raw lines first.
        let mut raw_lines: Vec<String> = Vec::new();
        for (i, line) in reader.lines().enumerate() {
            let line = line.context("reading transcript line")?;
            if i == 0 {
                continue; // skip header
            }
            if !line.trim().is_empty() {
                raw_lines.push(line);
            }
        }

        // Take the last `n` lines.
        let start = raw_lines.len().saturating_sub(n);
        let mut messages = Vec::with_capacity(n.min(raw_lines.len()));
        for (idx, line) in raw_lines[start..].iter().enumerate() {
            let msg: AgentMessage = serde_json::from_str(line)
                .with_context(|| format!("deserializing tail message at index {idx}"))?;
            messages.push(msg);
        }

        Ok(messages)
    }
}

// ── SessionStore ─────────────────────────────────────────────────

/// Manages the `sessions.json` index file.
///
/// Provides cached, lazy-loaded access to `SessionEntry` records with
/// get / set / list / remove / update\_usage operations. The cache is
/// persisted to disk on every mutation.
pub struct SessionStore {
    dir: PathBuf,
    cache: Mutex<HashMap<String, SessionEntry>>,
    loaded: AtomicBool,
}

impl SessionStore {
    /// Create a new store rooted at `dir`. The index file is `dir/sessions.json`.
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
            cache: Mutex::new(HashMap::new()),
            loaded: AtomicBool::new(false),
        }
    }

    /// Look up a session by ID. Returns `None` if it does not exist.
    pub fn get(&self, session_id: &str) -> Result<Option<SessionEntry>> {
        self.ensure_loaded()?;
        let guard = self
            .cache
            .lock()
            .map_err(|e| anyhow::anyhow!("session store lock poisoned: {e}"))?;
        Ok(guard.get(session_id).cloned())
    }

    /// Insert or update a session entry, then persist to disk.
    pub fn set(&self, entry: SessionEntry) -> Result<()> {
        self.ensure_loaded()?;
        {
            let mut guard = self
                .cache
                .lock()
                .map_err(|e| anyhow::anyhow!("session store lock poisoned: {e}"))?;
            guard.insert(entry.session_id.clone(), entry);
        }
        self.persist()
    }

    /// Return all session entries (unordered).
    pub fn list(&self) -> Result<Vec<SessionEntry>> {
        self.ensure_loaded()?;
        let guard = self
            .cache
            .lock()
            .map_err(|e| anyhow::anyhow!("session store lock poisoned: {e}"))?;
        Ok(guard.values().cloned().collect())
    }

    /// Remove a session from the index. Returns `true` if the session existed.
    pub fn remove(&self, session_id: &str) -> Result<bool> {
        self.ensure_loaded()?;
        let existed;
        {
            let mut guard = self
                .cache
                .lock()
                .map_err(|e| anyhow::anyhow!("session store lock poisoned: {e}"))?;
            existed = guard.remove(session_id).is_some();
        }
        if existed {
            self.persist()?;
        }
        Ok(existed)
    }

    /// Increment token counts for an existing session.
    pub fn update_usage(&self, session_id: &str, usage: &NormalizedUsage) -> Result<()> {
        self.ensure_loaded()?;
        {
            let mut guard = self
                .cache
                .lock()
                .map_err(|e| anyhow::anyhow!("session store lock poisoned: {e}"))?;
            let entry = guard
                .get_mut(session_id)
                .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
            entry.total_input_tokens += usage.input_tokens;
            entry.total_output_tokens += usage.output_tokens;
            entry.message_count += 1;
        }
        self.persist()
    }

    // ── Internal helpers ─────────────────────────────────────────

    /// Lazy-load the sessions.json index into the in-memory cache.
    fn ensure_loaded(&self) -> Result<()> {
        if self.loaded.load(Ordering::Acquire) {
            return Ok(());
        }

        let index_path = self.index_path();
        let mut guard = self
            .cache
            .lock()
            .map_err(|e| anyhow::anyhow!("session store lock poisoned: {e}"))?;

        // Double-check under the lock.
        if self.loaded.load(Ordering::Relaxed) {
            return Ok(());
        }

        if index_path.exists() {
            let data = fs::read_to_string(&index_path)
                .with_context(|| format!("reading {}", index_path.display()))?;
            let entries: Vec<SessionEntry> = serde_json::from_str(&data)
                .with_context(|| format!("parsing {}", index_path.display()))?;
            for entry in entries {
                guard.insert(entry.session_id.clone(), entry);
            }
        }

        self.loaded.store(true, Ordering::Release);
        Ok(())
    }

    /// Serialize the cache to `sessions.json` atomically.
    ///
    /// Writes to a temporary file then renames, so a crash mid-write
    /// never corrupts the index.
    fn persist(&self) -> Result<()> {
        let index_path = self.index_path();

        if let Some(parent) = index_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating directory {}", parent.display()))?;
        }

        let guard = self
            .cache
            .lock()
            .map_err(|e| anyhow::anyhow!("session store lock poisoned: {e}"))?;

        let entries: Vec<&SessionEntry> = guard.values().collect();
        let data = serde_json::to_string_pretty(&entries).context("serializing sessions index")?;

        // Atomic write: temp file → rename
        let tmp_path = index_path.with_extension("json.tmp");
        fs::write(&tmp_path, &data)
            .with_context(|| format!("writing temp file {}", tmp_path.display()))?;
        fs::rename(&tmp_path, &index_path).with_context(|| {
            format!(
                "renaming {} to {}",
                tmp_path.display(),
                index_path.display()
            )
        })?;

        Ok(())
    }

    /// Path to the sessions.json file.
    fn index_path(&self) -> PathBuf {
        self.dir.join("sessions.json")
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::types::{ContentBlock, Role};
    use tempfile::TempDir;

    /// Helper: create a minimal session header.
    fn test_header() -> SessionHeader {
        SessionHeader {
            session_id: "test-session-1".into(),
            created_at: "2025-06-01T00:00:00Z".into(),
            model: "claude-3".into(),
            system_prompt: Some("You are a helpful assistant.".into()),
            metadata: HashMap::new(),
        }
    }

    /// Helper: create a test agent message.
    fn test_message(index: usize) -> AgentMessage {
        AgentMessage {
            message_id: None,
            role: if index % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            },
            content: vec![ContentBlock::Text {
                text: format!("Message {index}"),
            }],
            timestamp: Some(format!("2025-06-01T00:00:{index:02}Z")),
            usage: Some(NormalizedUsage {
                input_tokens: 10 * (index as u64 + 1),
                output_tokens: 20 * (index as u64 + 1),
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            }),
            model: Some("claude-3".into()),
            metadata: None,
        }
    }

    /// Helper: create a test session entry.
    fn test_entry(id: &str) -> SessionEntry {
        SessionEntry {
            session_id: id.into(),
            created_at: "2025-06-01T00:00:00Z".into(),
            updated_at: None,
            model: "claude-3".into(),
            message_count: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            transcript_path: None,
            metadata: HashMap::new(),
        }
    }

    // ── TranscriptWriter + TranscriptReader roundtrip ────────────

    #[test]
    fn write_header_and_messages_then_read_back() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("transcripts/sess.jsonl");

        let header = test_header();
        let messages: Vec<AgentMessage> = (0..5).map(test_message).collect();

        // Write
        let writer = TranscriptWriter::open(&path, &header).unwrap();
        for msg in &messages {
            writer.append(msg).unwrap();
        }

        // Read
        let reader = TranscriptReader::open(&path).unwrap();
        let read_header = reader.read_header().unwrap();
        let read_messages = reader.read_messages().unwrap();

        assert_eq!(read_header, header);
        assert_eq!(read_messages.len(), messages.len());
        for (a, b) in read_messages.iter().zip(messages.iter()) {
            assert_eq!(a, b);
        }
    }

    #[test]
    fn jsonl_format_each_line_is_valid_json() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("valid.jsonl");

        let header = test_header();
        let writer = TranscriptWriter::open(&path, &header).unwrap();
        writer.append(&test_message(0)).unwrap();
        writer.append(&test_message(1)).unwrap();

        let raw = fs::read_to_string(&path).unwrap();
        for (i, line) in raw.lines().enumerate() {
            assert!(!line.trim().is_empty(), "line {i} should not be empty");
            let parsed: serde_json::Value = serde_json::from_str(line).unwrap_or_else(|e| {
                panic!("line {i} is not valid JSON: {e}\n  content: {line}");
            });
            assert!(parsed.is_object(), "line {i} should be a JSON object");
        }
    }

    #[test]
    fn tail_returns_last_n_messages() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("tail.jsonl");

        let header = test_header();
        let messages: Vec<AgentMessage> = (0..10).map(test_message).collect();

        let writer = TranscriptWriter::open(&path, &header).unwrap();
        for msg in &messages {
            writer.append(msg).unwrap();
        }

        let reader = TranscriptReader::open(&path).unwrap();

        // Tail 3
        let last3 = reader.tail(3).unwrap();
        assert_eq!(last3.len(), 3);
        assert_eq!(last3[0], messages[7]);
        assert_eq!(last3[1], messages[8]);
        assert_eq!(last3[2], messages[9]);

        // Tail more than available
        let all = reader.tail(100).unwrap();
        assert_eq!(all.len(), 10);

        // Tail 0
        let none = reader.tail(0).unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn message_count_accuracy() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("count.jsonl");

        let header = test_header();
        let writer = TranscriptWriter::open(&path, &header).unwrap();

        let reader = TranscriptReader::open(&path).unwrap();
        assert_eq!(reader.message_count().unwrap(), 0);

        for i in 0..7 {
            writer.append(&test_message(i)).unwrap();
        }

        assert_eq!(reader.message_count().unwrap(), 7);
    }

    #[test]
    fn paginated_read_messages_range() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("range.jsonl");

        let header = test_header();
        let messages: Vec<AgentMessage> = (0..10).map(test_message).collect();

        let writer = TranscriptWriter::open(&path, &header).unwrap();
        for msg in &messages {
            writer.append(msg).unwrap();
        }

        let reader = TranscriptReader::open(&path).unwrap();

        // First page
        let page1 = reader.read_messages_range(0, 3).unwrap();
        assert_eq!(page1.len(), 3);
        assert_eq!(page1[0], messages[0]);
        assert_eq!(page1[2], messages[2]);

        // Middle page
        let page2 = reader.read_messages_range(3, 3).unwrap();
        assert_eq!(page2.len(), 3);
        assert_eq!(page2[0], messages[3]);
        assert_eq!(page2[2], messages[5]);

        // Past end
        let page_end = reader.read_messages_range(8, 5).unwrap();
        assert_eq!(page_end.len(), 2);
        assert_eq!(page_end[0], messages[8]);
        assert_eq!(page_end[1], messages[9]);

        // Entirely past end
        let page_empty = reader.read_messages_range(20, 5).unwrap();
        assert!(page_empty.is_empty());
    }

    #[test]
    fn append_batch_writes_multiple_messages() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("batch.jsonl");

        let header = test_header();
        let messages: Vec<AgentMessage> = (0..4).map(test_message).collect();

        let writer = TranscriptWriter::open(&path, &header).unwrap();
        writer.append_batch(&messages).unwrap();

        let reader = TranscriptReader::open(&path).unwrap();
        let read = reader.read_messages().unwrap();
        assert_eq!(read.len(), 4);
        for (a, b) in read.iter().zip(messages.iter()) {
            assert_eq!(a, b);
        }
    }

    #[test]
    fn sync_does_not_error() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("sync.jsonl");

        let header = test_header();
        let writer = TranscriptWriter::open(&path, &header).unwrap();
        writer.append(&test_message(0)).unwrap();
        writer.sync().unwrap();
    }

    #[test]
    fn reader_open_nonexistent_file_returns_error() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nope.jsonl");
        assert!(TranscriptReader::open(&path).is_err());
    }

    // ── SessionStore ─────────────────────────────────────────────

    #[test]
    fn session_store_get_set_list() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());

        // Initially empty
        assert!(store.list().unwrap().is_empty());
        assert!(store.get("sess-1").unwrap().is_none());

        // Set two entries
        store.set(test_entry("sess-1")).unwrap();
        store.set(test_entry("sess-2")).unwrap();

        // Get
        let entry = store.get("sess-1").unwrap().unwrap();
        assert_eq!(entry.session_id, "sess-1");

        // List
        let all = store.list().unwrap();
        assert_eq!(all.len(), 2);

        // Upsert
        let mut updated = test_entry("sess-1");
        updated.model = "gpt-4".into();
        store.set(updated).unwrap();
        let entry = store.get("sess-1").unwrap().unwrap();
        assert_eq!(entry.model, "gpt-4");

        // Still two entries
        assert_eq!(store.list().unwrap().len(), 2);
    }

    #[test]
    fn session_store_remove() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());

        store.set(test_entry("sess-1")).unwrap();
        store.set(test_entry("sess-2")).unwrap();

        assert!(store.remove("sess-1").unwrap());
        assert!(!store.remove("sess-1").unwrap()); // already removed
        assert!(store.get("sess-1").unwrap().is_none());
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn session_store_update_usage() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());

        store.set(test_entry("sess-1")).unwrap();

        let usage = NormalizedUsage {
            input_tokens: 50,
            output_tokens: 100,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        };

        store.update_usage("sess-1", &usage).unwrap();
        store.update_usage("sess-1", &usage).unwrap();

        let entry = store.get("sess-1").unwrap().unwrap();
        assert_eq!(entry.total_input_tokens, 100);
        assert_eq!(entry.total_output_tokens, 200);
        assert_eq!(entry.message_count, 2);
    }

    #[test]
    fn session_store_update_usage_nonexistent_session_errors() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());

        let usage = NormalizedUsage::default();
        assert!(store.update_usage("no-such-session", &usage).is_err());
    }

    #[test]
    fn session_store_persists_to_disk_and_reloads() {
        let tmp = TempDir::new().unwrap();

        // First store instance — write data
        {
            let store = SessionStore::new(tmp.path());
            store.set(test_entry("sess-1")).unwrap();
            store.set(test_entry("sess-2")).unwrap();
        }

        // Second store instance — reload from disk
        {
            let store = SessionStore::new(tmp.path());
            let all = store.list().unwrap();
            assert_eq!(all.len(), 2);
            assert!(store.get("sess-1").unwrap().is_some());
            assert!(store.get("sess-2").unwrap().is_some());
        }
    }
}
