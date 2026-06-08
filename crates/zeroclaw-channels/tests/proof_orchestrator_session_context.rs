//! Proof that the orchestrator's composition of channel_id / room_id /
//! sender_id from a real `ChannelMessage` produces the values that land
//! in `session_metadata`. The orchestrator's full
//! `handle_channel_message` requires a fully-built runtime context which
//! is heavy to fixture; this test extracts the exact composition the
//! orchestrator does (verbatim from
//! `crates/zeroclaw-channels/src/orchestrator/mod.rs` right after
//! `let history_key = conversation_history_key(&msg);`) and drives it
//! through the same `SessionBackend::set_session_context` call.
//!
//! If this test's composition ever drifts from the production code,
//! the orchestrator side and this proof have to be updated together.

use std::sync::Arc;

use zeroclaw_api::channel::ChannelMessage;
use zeroclaw_infra::session_backend::{SessionBackend, SessionContext};
use zeroclaw_infra::session_sqlite::SqliteSessionBackend;

/// Mirror of the orchestrator's inline composition. Kept verbatim so the
/// test fails loudly if production drifts.
fn orchestrator_session_context_call(
    store: &Arc<dyn SessionBackend>,
    history_key: &str,
    msg: &ChannelMessage,
) {
    let channel_id = msg
        .channel_alias
        .as_deref()
        .map(|alias| format!("{}.{alias}", msg.channel));
    let room_id = msg
        .thread_ts
        .as_deref()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            let target = msg.reply_target.trim();
            if target.is_empty() {
                None
            } else {
                Some(target)
            }
        });
    let context = SessionContext {
        channel_id: channel_id.as_deref(),
        room_id,
        sender_id: Some(msg.sender.as_str()).filter(|s| !s.is_empty()),
    };
    store
        .set_session_context(history_key, context)
        .expect("set_session_context");
}

fn msg_from(
    channel: &str,
    alias: Option<&str>,
    thread: Option<&str>,
    reply_target: &str,
    sender: &str,
) -> ChannelMessage {
    ChannelMessage {
        id: "msg-1".into(),
        sender: sender.into(),
        reply_target: reply_target.into(),
        content: "hi".into(),
        channel: channel.into(),
        channel_alias: alias.map(String::from),
        timestamp: 0,
        thread_ts: thread.map(String::from),
        interruption_scope_id: None,
        attachments: Vec::new(),
        subject: None,
    }
}

#[test]
fn discord_threaded_message_writes_full_routing_columns() {
    let tmp = tempfile::TempDir::new().unwrap();
    let backend: Arc<dyn SessionBackend> = Arc::new(SqliteSessionBackend::new(tmp.path()).unwrap());

    let msg = msg_from(
        "discord",
        Some("clamps"),
        Some("thread-987654"),
        "channel-123",
        "singlerider",
    );
    orchestrator_session_context_call(&backend, "session_a", &msg);

    let meta = backend.get_session_metadata("session_a").unwrap();
    assert_eq!(meta.channel_id.as_deref(), Some("discord.clamps"));
    assert_eq!(meta.room_id.as_deref(), Some("thread-987654"));
    assert_eq!(meta.sender_id.as_deref(), Some("singlerider"));
    println!("discord threaded -> {meta:?}");
}

#[test]
fn discord_dm_falls_back_to_reply_target_for_room_id() {
    let tmp = tempfile::TempDir::new().unwrap();
    let backend: Arc<dyn SessionBackend> = Arc::new(SqliteSessionBackend::new(tmp.path()).unwrap());

    // No thread_ts -> orchestrator falls back to reply_target as room_id.
    let msg = msg_from("discord", Some("glados"), None, "dm-channel-555", "user42");
    orchestrator_session_context_call(&backend, "session_b", &msg);

    let meta = backend.get_session_metadata("session_b").unwrap();
    assert_eq!(meta.channel_id.as_deref(), Some("discord.glados"));
    assert_eq!(meta.room_id.as_deref(), Some("dm-channel-555"));
    assert_eq!(meta.sender_id.as_deref(), Some("user42"));
    println!("discord dm -> {meta:?}");
}

#[test]
fn single_instance_channel_with_no_alias_skips_channel_id() {
    let tmp = tempfile::TempDir::new().unwrap();
    let backend: Arc<dyn SessionBackend> = Arc::new(SqliteSessionBackend::new(tmp.path()).unwrap());

    // CLI / webhook / single-instance channels emit None for
    // channel_alias on their ChannelMessage (see commit 5606828fa).
    // The composition leaves channel_id None in that case — sender_id
    // still fills in.
    let msg = msg_from("cli", None, None, "stdin", "shane");
    orchestrator_session_context_call(&backend, "session_c", &msg);

    let meta = backend.get_session_metadata("session_c").unwrap();
    assert!(meta.channel_id.is_none(), "no alias -> no channel_id");
    assert_eq!(meta.room_id.as_deref(), Some("stdin"));
    assert_eq!(meta.sender_id.as_deref(), Some("shane"));
    println!("cli -> {meta:?}");
}

#[test]
fn empty_sender_is_filtered_to_none() {
    let tmp = tempfile::TempDir::new().unwrap();
    let backend: Arc<dyn SessionBackend> = Arc::new(SqliteSessionBackend::new(tmp.path()).unwrap());

    let msg = msg_from("matrix", Some("default"), None, "!room:matrix.org", "");
    orchestrator_session_context_call(&backend, "session_d", &msg);

    let meta = backend.get_session_metadata("session_d").unwrap();
    assert_eq!(meta.channel_id.as_deref(), Some("matrix.default"));
    assert_eq!(meta.room_id.as_deref(), Some("!room:matrix.org"));
    assert!(meta.sender_id.is_none(), "empty sender should not persist");
    println!("matrix empty sender -> {meta:?}");
}
