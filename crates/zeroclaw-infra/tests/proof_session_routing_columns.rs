//! End-to-end proof that the new `channel_id` / `room_id` / `sender_id`
//! columns on `session_metadata` actually exist after the migration runs
//! and that `set_session_context` writes through to disk where a raw
//! sqlite reader can see them.

use std::path::PathBuf;

use rusqlite::Connection;
use zeroclaw_api::model_provider::ChatMessage;
use zeroclaw_infra::session_backend::{SessionBackend, SessionContext};
use zeroclaw_infra::session_sqlite::SqliteSessionBackend;

#[test]
fn migration_adds_routing_columns_and_set_session_context_persists() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let sessions_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("mkdir");
    let db_path = sessions_dir.join("sessions.db");

    if let Ok(live) = std::env::var("ZEROCLAW_LIVE_SESSIONS_DB") {
        let live_path = PathBuf::from(&live);
        if live_path.exists() {
            std::fs::copy(&live_path, &db_path).expect("copy live db");
            let bytes = std::fs::metadata(&db_path).unwrap().len();
            println!(
                "Seeded tempdir from live daemon DB ({live}, {bytes} bytes) -> {}",
                db_path.display()
            );
        }
    } else {
        println!("ZEROCLAW_LIVE_SESSIONS_DB unset; running migration over empty file");
    }

    println!("\n=== BEFORE migration ===");
    if db_path.exists() {
        let conn = Connection::open(&db_path).expect("open before");
        dump_table_info(&conn, "session_metadata");
    } else {
        println!("  (no DB yet)");
    }

    let backend = SqliteSessionBackend::new(tmp.path()).expect("open backend (runs migration)");

    println!("\n=== AFTER migration (PRAGMA table_info session_metadata) ===");
    let columns = {
        let conn = Connection::open(&db_path).expect("open after");
        dump_table_info(&conn, "session_metadata")
    };
    assert!(
        columns.contains(&"channel_id".to_string()),
        "channel_id missing"
    );
    assert!(columns.contains(&"room_id".to_string()), "room_id missing");
    assert!(
        columns.contains(&"sender_id".to_string()),
        "sender_id missing"
    );

    let session_key = "proof_session_42";
    backend
        .append(session_key, &ChatMessage::user("ping"))
        .expect("append");
    backend
        .set_session_context(
            session_key,
            SessionContext {
                channel_id: Some("discord.clamps"),
                room_id: Some("1234567890"),
                sender_id: Some("singlerider"),
            },
        )
        .expect("set_session_context");
    backend
        .set_session_agent_alias(session_key, "clamps")
        .expect("set_session_agent_alias");

    println!("\n=== Wrote one row via SessionBackend trait ===");
    println!(
        "  session_key={session_key:?} agent_alias=clamps channel_id=discord.clamps room_id=1234567890 sender_id=singlerider"
    );

    println!("\n=== Reading back via raw rusqlite (bypasses backend code) ===");
    let conn = Connection::open(&db_path).expect("open read");
    let row = conn
        .query_row(
            "SELECT session_key, agent_alias, channel_id, room_id, sender_id, message_count \
             FROM session_metadata WHERE session_key = ?1",
            rusqlite::params![session_key],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .expect("row exists");
    println!(
        "  session_key={:?} agent_alias={:?} channel_id={:?} room_id={:?} sender_id={:?} message_count={}",
        row.0, row.1, row.2, row.3, row.4, row.5
    );
    assert_eq!(row.1.as_deref(), Some("clamps"));
    assert_eq!(row.2.as_deref(), Some("discord.clamps"));
    assert_eq!(row.3.as_deref(), Some("1234567890"));
    assert_eq!(row.4.as_deref(), Some("singlerider"));
    assert_eq!(row.5, 1);

    println!("\n=== Indexes on session_metadata (proves CREATE INDEX ran) ===");
    let mut stmt = conn
        .prepare(
            "SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='session_metadata' \
             ORDER BY name",
        )
        .expect("prepare");
    let mut rows = stmt.query([]).expect("query");
    let mut idx_names: Vec<String> = Vec::new();
    while let Some(row) = rows.next().expect("next") {
        let name: String = row.get(0).expect("name");
        println!("  {name}");
        idx_names.push(name);
    }
    for required in [
        "idx_session_metadata_channel_id",
        "idx_session_metadata_room_id",
        "idx_session_metadata_sender_id",
    ] {
        assert!(
            idx_names.iter().any(|n| n == required),
            "expected index {required} to exist; got {idx_names:?}"
        );
    }

    println!("\n=== Additive COALESCE behaviour (second call with None must not clobber) ===");
    backend
        .set_session_context(
            session_key,
            SessionContext {
                channel_id: None,
                room_id: Some("changed-room"),
                sender_id: None,
            },
        )
        .expect("second call");
    let after = conn
        .query_row(
            "SELECT channel_id, room_id, sender_id FROM session_metadata WHERE session_key = ?1",
            rusqlite::params![session_key],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .expect("second read");
    println!(
        "  channel_id={:?} room_id={:?} sender_id={:?}",
        after.0, after.1, after.2
    );
    assert_eq!(
        after.0.as_deref(),
        Some("discord.clamps"),
        "channel_id should not have been cleared by a None passthrough"
    );
    assert_eq!(after.1.as_deref(), Some("changed-room"));
    assert_eq!(
        after.2.as_deref(),
        Some("singlerider"),
        "sender_id should not have been cleared by a None passthrough"
    );

    println!("\n=== PROOF COMPLETE: migration ran, columns exist, writes persisted ===");
}

fn dump_table_info(conn: &Connection, table: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .expect("prepare");
    let mut rows = stmt.query([]).expect("query");
    while let Some(row) = rows.next().expect("next") {
        let name: String = row.get(1).expect("name");
        let ty: String = row.get(2).expect("type");
        let notnull: i64 = row.get(3).expect("notnull");
        let dflt: Option<String> = row.get(4).expect("default");
        let pk: i64 = row.get(5).expect("pk");
        println!(
            "  {name:<22} {ty:<10} notnull={notnull} default={} pk={pk}",
            dflt.as_deref().unwrap_or("-")
        );
        names.push(name);
    }
    names
}
