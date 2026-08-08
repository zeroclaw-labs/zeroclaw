//! JSONL-based session persistence for channel conversations.

use crate::session_backend::{
    ChannelConversationRecord, ConditionalSessionWrite, SessionBackend, SessionMutation,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};
use zeroclaw_api::model_provider::ChatMessage;
pub use zeroclaw_api::session_keys::sanitize_session_key;

const LOCK_SUFFIX: &str = ".lock";
const META_SUFFIX: &str = ".meta.json";

type MutationLock = parking_lot::Mutex<()>;

static MUTATION_LOCKS: OnceLock<parking_lot::Mutex<HashMap<PathBuf, Weak<MutationLock>>>> =
    OnceLock::new();

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionHeader {
    #[serde(rename = "type")]
    kind: String,
    version: u8,
    conversation_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacySidecar {
    conversation_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionMessage {
    role: String,
    content: String,
}

impl From<SessionMessage> for ChatMessage {
    fn from(message: SessionMessage) -> Self {
        Self {
            role: message.role,
            content: message.content,
        }
    }
}

pub struct SessionStore {
    sessions_dir: PathBuf,
    mutation_lock: Arc<MutationLock>,
}

impl SessionStore {
    pub fn new(workspace_dir: &Path) -> std::io::Result<Self> {
        let sessions_dir = workspace_dir.join("sessions");
        std::fs::create_dir_all(&sessions_dir)?;
        let mutation_lock = mutation_lock_for(&sessions_dir)?;
        Ok(Self {
            sessions_dir,
            mutation_lock,
        })
    }
    fn session_path(&self, key: &str) -> PathBuf {
        self.sessions_dir
            .join(format!("{}.jsonl", sanitize_session_key(key)))
    }
    fn lock_path(&self, key: &str) -> PathBuf {
        self.sessions_dir
            .join(format!("{}{}", sanitize_session_key(key), LOCK_SUFFIX))
    }
    fn meta_path(&self, key: &str) -> PathBuf {
        self.sessions_dir
            .join(format!("{}{}", sanitize_session_key(key), META_SUFFIX))
    }
    fn read_conversation_id(&self, key: &str) -> std::io::Result<Option<String>> {
        Ok(self
            .read_record_unlocked(key, false)?
            .map(|record| record.conversation_id))
    }
    #[allow(clippy::suspicious_open_options)]
    fn with_key_lock<R>(
        &self,
        key: &str,
        f: impl FnOnce() -> std::io::Result<R>,
    ) -> std::io::Result<R> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(self.lock_path(key))?;
        file.lock()?;
        f()
    }
    fn valid_id(id: &str) -> bool {
        uuid::Uuid::parse_str(id).is_ok_and(|u| u.get_version_num() == 4)
    }
    fn read_record_unlocked(
        &self,
        key: &str,
        upgrade: bool,
    ) -> std::io::Result<Option<ChannelConversationRecord>> {
        let path = self.session_path(key);
        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let lines: Vec<String> = std::io::BufReader::new(file)
            .lines()
            .collect::<Result<_, _>>()?;
        let nonempty: Vec<&str> = lines
            .iter()
            .map(String::as_str)
            .filter(|l| !l.trim().is_empty())
            .collect();
        if nonempty.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "session record has no header",
            ));
        }
        let first: serde_json::Value = serde_json::from_str(nonempty[0])
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let (id, start, legacy) = if first.get("type").is_some() {
            let h: SessionHeader = serde_json::from_value(first)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            if h.kind != "session_meta" || h.version != 1 || !Self::valid_id(&h.conversation_id) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "invalid session header",
                ));
            }
            (h.conversation_id, 1, false)
        } else {
            let id = match std::fs::read_to_string(self.meta_path(key)) {
                Ok(raw) => {
                    let side: LegacySidecar = serde_json::from_str(&raw)
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                    if !Self::valid_id(&side.conversation_id) {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "invalid legacy conversation id",
                        ));
                    }
                    side.conversation_id
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    uuid::Uuid::new_v4().to_string()
                }
                Err(e) => return Err(e),
            };
            (id, 0, true)
        };
        let mut history = Vec::new();
        for line in &nonempty[start..] {
            let message: SessionMessage = serde_json::from_str(line)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            history.push(message.into());
        }
        let record = ChannelConversationRecord {
            conversation_id: id,
            history,
        };
        if legacy && upgrade {
            self.write_record_unlocked(key, &record)?;
            if let Err(e) = std::fs::remove_file(self.meta_path(key))
                && e.kind() != std::io::ErrorKind::NotFound
            {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"error":e.to_string()})),
                    "could not remove folded session sidecar"
                );
            }
        }
        Ok(Some(record))
    }
    fn write_record_unlocked(
        &self,
        key: &str,
        record: &ChannelConversationRecord,
    ) -> std::io::Result<()> {
        self.write_record_with(key, record, |temp, path| {
            temp.persist(path).map(|_| ()).map_err(|error| error.error)
        })
    }

    fn write_record_with<F>(
        &self,
        key: &str,
        record: &ChannelConversationRecord,
        persist: F,
    ) -> std::io::Result<()>
    where
        F: FnOnce(tempfile::NamedTempFile, &Path) -> std::io::Result<()>,
    {
        let path = self.session_path(key);
        let mut temp = tempfile::NamedTempFile::new_in(&self.sessions_dir)?;
        let header = SessionHeader {
            kind: "session_meta".into(),
            version: 1,
            conversation_id: record.conversation_id.clone(),
        };
        serde_json::to_writer(&mut temp, &header).map_err(std::io::Error::other)?;
        temp.write_all(b"\n")?;
        for message in &record.history {
            serde_json::to_writer(&mut temp, message).map_err(std::io::Error::other)?;
            temp.write_all(b"\n")?;
        }
        temp.as_file().sync_all()?;
        persist(temp, &path)
    }
    pub(crate) fn with_locked_conversation<R>(
        &self,
        key: &str,
        f: impl FnOnce(
            &dyn Fn() -> std::io::Result<Option<ChannelConversationRecord>>,
            &Path,
        ) -> std::io::Result<R>,
    ) -> std::io::Result<R> {
        let _guard = self.mutation_lock.lock();
        self.with_key_lock(key, || {
            let load = || self.read_record_unlocked(key, true);
            f(&load, &self.session_path(key))
        })
    }
    pub fn load(&self, key: &str) -> Vec<ChatMessage> {
        self.load_fallible(key).unwrap_or_default()
    }
    fn load_fallible(&self, key: &str) -> std::io::Result<Vec<ChatMessage>> {
        let _guard = self.mutation_lock.lock();
        self.with_key_lock(key, || {
            Ok(self
                .read_record_unlocked(key, true)?
                .map_or_else(Vec::new, |r| r.history))
        })
    }
    pub fn append(&self, key: &str, m: &ChatMessage) -> std::io::Result<()> {
        let _guard = self.mutation_lock.lock();
        self.with_key_lock(key, || {
            let mut r =
                self.read_record_unlocked(key, true)?
                    .unwrap_or(ChannelConversationRecord {
                        conversation_id: uuid::Uuid::new_v4().to_string(),
                        history: vec![],
                    });
            r.history.push(m.clone());
            self.write_record_unlocked(key, &r)
        })
    }
    pub fn remove_last(&self, key: &str) -> std::io::Result<bool> {
        let _guard = self.mutation_lock.lock();
        self.with_key_lock(key, || {
            let Some(mut r) = self.read_record_unlocked(key, true)? else {
                return Ok(false);
            };
            if r.history.pop().is_none() {
                return Ok(false);
            }
            self.write_record_unlocked(key, &r)?;
            Ok(true)
        })
    }
    pub fn compact(&self, key: &str) -> std::io::Result<()> {
        let _guard = self.mutation_lock.lock();
        self.with_key_lock(key, || {
            if let Some(r) = self.read_record_unlocked(key, true)? {
                self.write_record_unlocked(key, &r)?
            }
            Ok(())
        })
    }
    pub fn clear_messages(&self, key: &str) -> std::io::Result<usize> {
        let _guard = self.mutation_lock.lock();
        self.with_key_lock(key, || {
            let Some(mut r) = self.read_record_unlocked(key, true)? else {
                return Ok(0);
            };
            let n = r.history.len();
            r.history.clear();
            self.write_record_unlocked(key, &r)?;
            Ok(n)
        })
    }
    pub fn delete_session(&self, key: &str) -> std::io::Result<bool> {
        let _guard = self.mutation_lock.lock();
        self.with_key_lock(key, || {
            let deleted = match std::fs::remove_file(self.session_path(key)) {
                Ok(()) => true,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
                Err(e) => return Err(e),
            };
            if let Err(e) = std::fs::remove_file(self.meta_path(key))
                && e.kind() != std::io::ErrorKind::NotFound
            {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"error": e.to_string()})),
                    "could not remove legacy session sidecar"
                );
            }
            Ok(deleted)
        })
    }
    pub fn session_mtime(&self, key: &str) -> Option<std::time::SystemTime> {
        std::fs::metadata(self.session_path(key))
            .and_then(|m| m.modified())
            .ok()
    }
    pub fn list_sessions(&self) -> Vec<String> {
        std::fs::read_dir(&self.sessions_dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().into_string().ok())
            .filter_map(|n| n.strip_suffix(".jsonl").map(str::to_owned))
            .collect()
    }
}

fn mutation_lock_for(sessions_dir: &Path) -> std::io::Result<Arc<MutationLock>> {
    let key = sessions_dir.canonicalize()?;
    let registry = MUTATION_LOCKS.get_or_init(|| parking_lot::Mutex::new(HashMap::new()));
    let mut locks = registry.lock();
    locks.retain(|_, lock| lock.strong_count() > 0);

    if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
        return Ok(lock);
    }

    let lock = Arc::new(MutationLock::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    Ok(lock)
}

impl SessionBackend for SessionStore {
    fn resolve_or_create_conversation_id(&self, k: &str) -> std::io::Result<String> {
        Ok(self.open_conversation(k)?.conversation_id)
    }
    fn clear_and_rotate_conversation(&self, k: &str) -> std::io::Result<String> {
        let _guard = self.mutation_lock.lock();
        self.with_key_lock(k, || {
            let r = ChannelConversationRecord {
                conversation_id: uuid::Uuid::new_v4().to_string(),
                history: Vec::new(),
            };
            self.write_record_unlocked(k, &r)?;
            Ok(r.conversation_id)
        })
    }
    fn append_if_conversation_matches(
        &self,
        k: &str,
        id: &str,
        message: &ChatMessage,
    ) -> std::io::Result<ConditionalSessionWrite> {
        self.mutate_conversation_if_current(k, id, SessionMutation::Append(message))
    }
    fn remove_last_if_conversation_matches(
        &self,
        k: &str,
        id: &str,
    ) -> std::io::Result<ConditionalSessionWrite> {
        let _guard = self.mutation_lock.lock();
        self.with_key_lock(k, || {
            let Some(mut record) = self.read_record_unlocked(k, true)? else {
                return Ok(ConditionalSessionWrite::Deleted);
            };
            if !Self::valid_id(id) || record.conversation_id != id {
                return Ok(ConditionalSessionWrite::Stale);
            }
            if record.history.pop().is_some() {
                self.write_record_unlocked(k, &record)?;
            }
            Ok(ConditionalSessionWrite::Applied)
        })
    }
    fn update_last_if_conversation_matches(
        &self,
        k: &str,
        id: &str,
        message: &ChatMessage,
    ) -> std::io::Result<ConditionalSessionWrite> {
        self.mutate_conversation_if_current(k, id, SessionMutation::UpdateLast(message))
    }

    fn load(&self, k: &str) -> Vec<ChatMessage> {
        self.load(k)
    }
    fn load_fallible(&self, k: &str) -> std::io::Result<Vec<ChatMessage>> {
        self.load_fallible(k)
    }
    fn append(&self, k: &str, m: &ChatMessage) -> std::io::Result<()> {
        self.append(k, m)
    }
    fn remove_last(&self, k: &str) -> std::io::Result<bool> {
        self.remove_last(k)
    }
    fn list_sessions(&self) -> Vec<String> {
        self.list_sessions()
    }
    fn compact(&self, k: &str) -> std::io::Result<()> {
        self.compact(k)
    }
    fn clear_messages(&self, k: &str) -> std::io::Result<usize> {
        self.clear_messages(k)
    }
    fn delete_session(&self, k: &str) -> std::io::Result<bool> {
        self.delete_session(k)
    }
    fn session_exists(&self, k: &str) -> bool {
        self.session_path(k).exists()
    }
    fn current_conversation_id(&self, k: &str) -> std::io::Result<Option<String>> {
        // Read the header id directly rather than via get_session_metadata
        // (whose trait default leaves conversation_id unset for JSONL), so
        // durable is_current / existing_record_for_test work on the JSONL
        // backend.
        self.read_conversation_id(k)
    }
    fn open_conversation(&self, k: &str) -> std::io::Result<ChannelConversationRecord> {
        let _guard = self.mutation_lock.lock();
        self.with_key_lock(k, || {
            if let Some(r) = self.read_record_unlocked(k, true)? {
                return Ok(r);
            }
            let r = ChannelConversationRecord {
                conversation_id: uuid::Uuid::new_v4().to_string(),
                history: vec![],
            };
            self.write_record_unlocked(k, &r)?;
            Ok(r)
        })
    }
    fn mutate_conversation_if_current(
        &self,
        k: &str,
        id: &str,
        m: SessionMutation<'_>,
    ) -> std::io::Result<ConditionalSessionWrite> {
        let _guard = self.mutation_lock.lock();
        self.with_key_lock(k, || {
            let Some(mut r) = self.read_record_unlocked(k, true)? else {
                return Ok(ConditionalSessionWrite::Deleted);
            };
            if r.conversation_id != id {
                return Ok(ConditionalSessionWrite::Stale);
            }
            match m {
                SessionMutation::Append(x) => r.history.push(x.clone()),
                SessionMutation::RemoveLast {
                    expected_role,
                    expected_content,
                } => {
                    if r.history
                        .last()
                        .is_some_and(|x| x.role == expected_role && x.content == expected_content)
                    {
                        r.history.pop();
                    }
                }
                SessionMutation::UpdateLast(x) => {
                    if let Some(last) = r.history.last_mut() {
                        *last = x.clone()
                    }
                }
            }
            self.write_record_unlocked(k, &r)?;
            Ok(ConditionalSessionWrite::Applied)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn round_trip_append_and_load() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        store
            .append("telegram_user123", &ChatMessage::user("hello"))
            .unwrap();
        store
            .append("telegram_user123", &ChatMessage::assistant("hi there"))
            .unwrap();

        let messages = store.load("telegram_user123");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, "hello");
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[1].content, "hi there");
    }

    #[test]
    fn load_nonexistent_session_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        let messages = store.load("nonexistent");
        assert!(messages.is_empty());
    }

    #[test]
    fn fallible_load_distinguishes_missing_history_from_invalid_record() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        assert!(store.load_fallible("missing").unwrap().is_empty());
        std::fs::write(store.session_path("broken"), "not-json\n").unwrap();

        let error = store.load_fallible("broken").unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(store.load("broken").is_empty());
    }

    #[test]
    fn key_sanitization() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        store
            .append("slack/thread:123/user", &ChatMessage::user("test"))
            .unwrap();

        let messages = store.load("slack/thread:123/user");
        assert_eq!(messages.len(), 1);
    }

    #[test]
    fn sanitize_session_key_is_idempotent() {
        let raw = "slack_C123_1.2_user one";
        let once = sanitize_session_key(raw);
        let twice = sanitize_session_key(&once);
        assert_eq!(once, "slack_C123_1_2_user_one");
        assert_eq!(once, twice);
    }

    #[test]
    fn restart_simulation_matches_when_caller_pre_sanitizes() {
        let tmp = TempDir::new().unwrap();
        let runtime_key = sanitize_session_key("slack_C123_1.2_user one");

        {
            let store = SessionStore::new(tmp.path()).unwrap();
            store
                .append(&runtime_key, &ChatMessage::user("first"))
                .unwrap();
            store
                .append(&runtime_key, &ChatMessage::assistant("ack"))
                .unwrap();
        }

        let store = SessionStore::new(tmp.path()).unwrap();
        let listed = store.list_sessions();
        assert_eq!(listed, vec![runtime_key.clone()]);

        let msgs = store.load(&listed[0]);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "first");
        assert_eq!(msgs[1].content, "ack");
    }

    #[test]
    fn list_sessions_returns_keys() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        store
            .append("telegram_alice", &ChatMessage::user("hi"))
            .unwrap();
        store
            .append("discord_bob", &ChatMessage::user("hey"))
            .unwrap();

        let mut sessions = store.list_sessions();
        sessions.sort();
        assert_eq!(sessions.len(), 2);
        assert!(sessions.contains(&"discord_bob".to_string()));
        assert!(sessions.contains(&"telegram_alice".to_string()));
    }

    #[test]
    fn jsonl_formal_record_retains_header_across_appends() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        store.append("formal", &ChatMessage::user("one")).unwrap();
        store
            .append("formal", &ChatMessage::assistant("two"))
            .unwrap();
        let raw = std::fs::read_to_string(store.session_path("formal")).unwrap();
        let lines: Vec<_> = raw.lines().collect();
        let header: SessionHeader = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(header.kind, "session_meta");
        assert_eq!(header.version, 1);
        assert!(SessionStore::valid_id(&header.conversation_id));
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn remove_last_drops_final_message() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        store
            .append("rm_test", &ChatMessage::user("first"))
            .unwrap();
        store
            .append("rm_test", &ChatMessage::user("second"))
            .unwrap();

        assert!(store.remove_last("rm_test").unwrap());
        let messages = store.load("rm_test");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "first");
    }

    #[test]
    fn remove_last_empty_returns_false() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        assert!(!store.remove_last("nonexistent").unwrap());
    }

    #[test]
    fn jsonl_corrupt_or_duplicate_header_fails_closed() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let id = uuid::Uuid::new_v4();
        std::fs::write(store.session_path("corrupt"), "not-json\n").unwrap();
        assert!(store.open_conversation("corrupt").is_err());
        std::fs::write(
            store.session_path("duplicate"),
            format!(
                "{{\"type\":\"session_meta\",\"version\":1,\"conversation_id\":\"{id}\"}}\n{{\"type\":\"session_meta\",\"version\":1,\"conversation_id\":\"{id}\"}}\n"
            ),
        ).unwrap();
        assert!(store.open_conversation("duplicate").is_err());
        std::fs::write(
            store.session_path("unknown_message_field"),
            format!(
                "{{\"type\":\"session_meta\",\"version\":1,\"conversation_id\":\"{id}\"}}\n{{\"role\":\"user\",\"content\":\"hello\",\"unexpected\":true}}\n"
            ),
        )
        .unwrap();
        assert!(store.open_conversation("unknown_message_field").is_err());
    }

    #[test]
    fn session_backend_trait_works_via_dyn() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;

        backend
            .append("trait_test", &ChatMessage::user("hello"))
            .unwrap();
        let msgs = backend.load("trait_test");
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn jsonl_legacy_file_upgrades_to_header_and_preserves_messages() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        std::fs::write(
            store.session_path("legacy"),
            "{\"role\":\"user\",\"content\":\"hello\"}\n{\"role\":\"assistant\",\"content\":\"world\"}\n",
        ).unwrap();
        let record = store.open_conversation("legacy").unwrap();
        assert!(SessionStore::valid_id(&record.conversation_id));
        assert_eq!(record.history.len(), 2);
        let raw = std::fs::read_to_string(store.session_path("legacy")).unwrap();
        assert!(raw.lines().next().unwrap().contains("session_meta"));
    }

    #[test]
    fn clear_messages_truncates_file() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "clear_test";

        store.append(key, &ChatMessage::user("hello")).unwrap();
        store.append(key, &ChatMessage::assistant("world")).unwrap();

        let cleared = store.clear_messages(key).unwrap();
        assert_eq!(cleared, 2);
        assert!(store.load(key).is_empty());
        // File still exists — session key remains in list_sessions
        assert!(store.session_path(key).exists());
    }

    #[test]
    fn clear_messages_empty_returns_zero() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        assert_eq!(store.clear_messages("nonexistent").unwrap(), 0);
    }

    #[test]
    fn clear_messages_does_not_affect_other_sessions() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        store
            .append("alice", &ChatMessage::user("alice msg"))
            .unwrap();
        store.append("bob", &ChatMessage::user("bob msg")).unwrap();

        store.clear_messages("alice").unwrap();
        assert!(store.load("alice").is_empty());
        assert_eq!(store.load("bob").len(), 1);
    }

    #[test]
    fn clear_messages_then_append_works() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "reuse_test";

        store.append(key, &ChatMessage::user("old")).unwrap();
        store.clear_messages(key).unwrap();
        store.append(key, &ChatMessage::user("new")).unwrap();

        let messages = store.load(key);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "new");
    }

    #[test]
    fn delete_session_removes_jsonl_file() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "delete_test";

        store.append(key, &ChatMessage::user("hello")).unwrap();
        assert_eq!(store.load(key).len(), 1);

        let deleted = store.delete_session(key).unwrap();
        assert!(deleted);
        assert!(store.load(key).is_empty());
        assert!(!store.session_path(key).exists());
    }

    #[test]
    fn delete_session_nonexistent_returns_false() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        let deleted = store.delete_session("nonexistent").unwrap();
        assert!(!deleted);
    }

    #[test]
    fn delete_session_via_trait() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;

        backend
            .append("trait_delete", &ChatMessage::user("hello"))
            .unwrap();
        assert_eq!(backend.load("trait_delete").len(), 1);

        let deleted = backend.delete_session("trait_delete").unwrap();
        assert!(deleted);
        assert!(backend.load("trait_delete").is_empty());
    }

    // ── session_exists─────────────────────────────────────
    #[test]
    fn session_exists_tracks_lifecycle() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;

        assert!(!backend.session_exists("ghost"));

        backend
            .append("ghost", &ChatMessage::user("first"))
            .unwrap();
        assert!(backend.session_exists("ghost"));

        backend.delete_session("ghost").unwrap();
        assert!(!backend.session_exists("ghost"));
    }

    // ── get_session_metadata (trait default) tests ──────────────────

    #[test]
    fn get_session_metadata_returns_none_for_missing() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;
        assert!(backend.get_session_metadata("nonexistent").is_none());
    }

    #[test]
    fn get_session_metadata_returns_correct_count() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;

        backend
            .append("test_session", &ChatMessage::user("hello"))
            .unwrap();
        backend
            .append("test_session", &ChatMessage::assistant("hi"))
            .unwrap();

        let meta = backend.get_session_metadata("test_session").unwrap();
        assert_eq!(meta.key, "test_session");
        assert_eq!(meta.message_count, 2);
        assert!(meta.name.is_none());
    }

    // ── conversation_id (atomic channel identity) tests ───────────────

    #[test]
    fn jsonl_sidecar_is_folded_into_header_without_changing_id() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        std::fs::write(
            store.session_path("legacy"),
            "{\"role\":\"user\",\"content\":\"old\"}\n",
        )
        .unwrap();
        std::fs::write(
            store.meta_path("legacy"),
            format!("{{\"conversation_id\":\"{id}\"}}\n"),
        )
        .unwrap();
        let record = store.open_conversation("legacy").unwrap();
        assert_eq!(record.conversation_id, id);
        assert_eq!(record.history.len(), 1);
        assert!(!store.meta_path("legacy").exists());
        assert!(
            std::fs::read_to_string(store.session_path("legacy"))
                .unwrap()
                .lines()
                .next()
                .unwrap()
                .contains(&id)
        );
    }

    #[test]
    fn jsonl_corrupt_sidecar_fails_closed() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        std::fs::write(
            store.session_path("legacy"),
            "{\"role\":\"user\",\"content\":\"old\"}\n",
        )
        .unwrap();
        std::fs::write(store.meta_path("legacy"), "not-json\n").unwrap();
        assert!(store.open_conversation("legacy").is_err());
        let raw = std::fs::read_to_string(store.session_path("legacy")).unwrap();
        assert!(!raw.contains("session_meta"));
    }

    #[test]
    fn conversation_id_survives_reopen_jsonl() {
        let tmp = TempDir::new().unwrap();
        let id_before = {
            let store = SessionStore::new(tmp.path()).unwrap();
            store.resolve_or_create_conversation_id("persist").unwrap()
        };
        let store2 = SessionStore::new(tmp.path()).unwrap();
        let id_after = store2.resolve_or_create_conversation_id("persist").unwrap();
        assert_eq!(id_before, id_after);
    }

    #[test]
    fn conversation_id_clear_and_rotate_clears_history_and_mints_new_id_jsonl() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        store.append("rot", &ChatMessage::user("a")).unwrap();
        store.append("rot", &ChatMessage::assistant("b")).unwrap();
        let id1 = store.resolve_or_create_conversation_id("rot").unwrap();
        assert_eq!(store.load("rot").len(), 2);

        let id2 = store.clear_and_rotate_conversation("rot").unwrap();
        assert_ne!(id1, id2, "rotate must mint a fresh id");
        assert!(store.load("rot").is_empty(), "rotate must clear history");
        // The .jsonl is preserved (truncated) so the key stays listed.
        assert!(store.session_path("rot").exists());
        // The sidecar now holds the rotated id.
        let stored = store.read_conversation_id("rot").unwrap();
        assert_eq!(stored.as_deref(), Some(id2.as_str()));
        // Post-rotate resolve is stable on the new id.
        assert_eq!(store.resolve_or_create_conversation_id("rot").unwrap(), id2);
    }

    #[test]
    fn conversation_id_other_key_isolation_jsonl() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        let id_a = store.resolve_or_create_conversation_id("a").unwrap();
        let id_b = store.resolve_or_create_conversation_id("b").unwrap();
        assert_ne!(id_a, id_b);

        let id_a2 = store.clear_and_rotate_conversation("a").unwrap();
        assert_ne!(id_a, id_a2);
        assert_eq!(
            store.resolve_or_create_conversation_id("b").unwrap(),
            id_b,
            "other-key isolation: rotate(a) must not change b"
        );
    }

    #[test]
    fn conversation_id_delete_then_recreate_mints_new_id_jsonl() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        // Seed a data file + sidecar so both exist before delete.
        store.append("del", &ChatMessage::user("x")).unwrap();
        let id1 = store.resolve_or_create_conversation_id("del").unwrap();
        assert!(store.delete_session("del").unwrap());
        assert!(
            !store.meta_path("del").exists(),
            "delete must remove sidecar"
        );
        let id2 = store.resolve_or_create_conversation_id("del").unwrap();
        assert_ne!(id1, id2, "delete + recreate must mint a fresh id");
    }

    #[test]
    fn conversation_id_concurrent_resolve_converges_jsonl() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let tmp = TempDir::new().unwrap();
        // Two independent SessionStore instances on the same dir. The
        // per-key file lock must serialize them onto one id.
        let a = Arc::new(SessionStore::new(tmp.path()).unwrap());
        let b = SessionStore::new(tmp.path()).unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let key = "conv_concurrent";

        let bar = barrier.clone();
        let a_c = a.clone();
        let h1 = thread::spawn(move || {
            bar.wait();
            a_c.resolve_or_create_conversation_id(key).unwrap()
        });
        let bar2 = barrier.clone();
        let h2 = thread::spawn(move || {
            bar2.wait();
            b.resolve_or_create_conversation_id(key).unwrap()
        });
        let id1 = h1.join().unwrap();
        let id2 = h2.join().unwrap();

        assert!(!id1.is_empty() && !id2.is_empty());
        assert_eq!(
            id1, id2,
            "two concurrent first-access resolves must converge on one id"
        );

        // A third fresh instance reads the same persisted id.
        let c = SessionStore::new(tmp.path()).unwrap();
        assert_eq!(c.resolve_or_create_conversation_id(key).unwrap(), id1);
    }

    #[test]
    fn conversation_id_resolve_and_rotate_race_stays_consistent_jsonl() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let tmp = TempDir::new().unwrap();
        let a = Arc::new(SessionStore::new(tmp.path()).unwrap());
        let initial = a.resolve_or_create_conversation_id("race").unwrap();
        let b = SessionStore::new(tmp.path()).unwrap();
        let barrier = Arc::new(Barrier::new(2));

        let bar = barrier.clone();
        let a_c = a.clone();
        let h_res = thread::spawn(move || {
            bar.wait();
            let mut ids = Vec::new();
            for _ in 0..64 {
                ids.push(a_c.resolve_or_create_conversation_id("race").unwrap());
            }
            ids
        });

        let bar2 = barrier.clone();
        let h_rot = thread::spawn(move || {
            bar2.wait();
            b.clear_and_rotate_conversation("race").unwrap()
        });

        let rotated = h_rot.join().unwrap();
        let ids = h_res.join().unwrap();
        assert_ne!(rotated, initial);
        for id in &ids {
            assert!(!id.is_empty(), "race produced an empty id");
            assert!(
                *id == initial || *id == rotated,
                "race produced an id ({id}) that is neither the pre- nor post-rotate value"
            );
        }

        // After both threads joined the rotate has committed. A fresh
        // instance must observe post-rotate state.
        let c = SessionStore::new(tmp.path()).unwrap();
        assert_eq!(
            c.resolve_or_create_conversation_id("race").unwrap(),
            rotated,
            "final persisted id must be the rotated one"
        );
        assert!(
            c.load("race").is_empty(),
            "rotate must have cleared history"
        );
    }

    // ── crash / delete hardening tests ───────────────────────────────

    #[test]
    fn clear_and_rotate_mints_fresh_id_and_truncates() {
        // Asserts the observable post-state (new id persisted + history
        // cleared), NOT crash injection; the write-id-before-truncate ORDER
        // is statically guaranteed by the implementation structure -
        // `write_conversation_id` is a complete temp+sync+rename atomic write
        // that completes before `File::create` truncates, and
        // `file.sync_all()` provides truncate durability - so no
        // fault-injection seam is needed in production code.
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        store.append("rot", &ChatMessage::user("a")).unwrap();
        store.append("rot", &ChatMessage::assistant("b")).unwrap();
        let id_before = store.resolve_or_create_conversation_id("rot").unwrap();
        assert_eq!(store.load("rot").len(), 2);

        let id_after = store.clear_and_rotate_conversation("rot").unwrap();
        assert_ne!(id_after, id_before, "rotate must mint a fresh id");
        assert!(store.load("rot").is_empty(), "history must be truncated");
        assert_eq!(
            store.resolve_or_create_conversation_id("rot").unwrap(),
            id_after,
            "fresh id must be persisted to the sidecar"
        );
    }

    #[test]
    fn delete_session_removes_formal_record_despite_sidecar_cleanup_failure() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        store.open_conversation("del").unwrap();
        std::fs::create_dir(store.meta_path("del")).unwrap();
        assert!(store.delete_session("del").unwrap());
        assert!(!store.session_path("del").exists());
    }

    // ── conditional-write (conversation-id fence) tests ───────────────

    #[test]
    fn channel_conversation_contract_jsonl() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;
        crate::session_backend::assert_channel_conversation_contract(backend);
    }

    #[test]
    fn jsonl_stale_mutation_does_not_recreate_deleted_file() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let record = store.open_conversation("deleted").unwrap();
        store.delete_session("deleted").unwrap();
        assert_eq!(
            store
                .mutate_conversation_if_current(
                    "deleted",
                    &record.conversation_id,
                    SessionMutation::Append(&ChatMessage::user("stale")),
                )
                .unwrap(),
            ConditionalSessionWrite::Deleted,
        );
        assert!(!store.session_path("deleted").exists());
    }

    #[test]
    fn jsonl_leftover_temp_file_does_not_replace_formal_record() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let record = store.open_conversation("formal").unwrap();
        std::fs::write(store.sessions_dir.join(".formal.999.0.tmp"), "garbage\n").unwrap();
        let reopened = store.open_conversation("formal").unwrap();
        assert_eq!(reopened.conversation_id, record.conversation_id);
        assert!(reopened.history.is_empty());
    }
}
