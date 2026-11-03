//! Master-format routing metadata must survive installation migration without
//! initializing another legacy backend or changing the backed-up schema.

use rusqlite::Connection;
use zeroclaw_infra::session_events::{
    catalog::SessionCatalog, installation::NativeBackend, scoped::EventStorePool,
    upgrade::upgrade_installation,
};

#[test]
fn legacy_routing_metadata_is_imported_to_the_native_catalog() {
    for backend in [NativeBackend::Sqlite, NativeBackend::Jsonl] {
        if backend == NativeBackend::Jsonl && !cfg!(unix) {
            continue;
        }
        for routing_columns in [false, true] {
            let directory = tempfile::tempdir().expect("directory");
            let sessions = directory.path().join("sessions");
            std::fs::create_dir(&sessions).expect("sessions directory");
            let db_path = sessions.join("sessions.db");
            let conn = Connection::open(&db_path).expect("old database");
            conn.execute_batch(
                "CREATE TABLE sessions (id INTEGER PRIMARY KEY, session_key TEXT, role TEXT,
                    content TEXT, created_at TEXT);
                 CREATE TABLE session_metadata (session_key TEXT PRIMARY KEY, name TEXT,
                    created_at TEXT, last_activity TEXT, message_count INTEGER);
                 INSERT INTO sessions VALUES (1,'gw_migrated','user','retained input','2020-01-02T03:04:05Z');
                 INSERT INTO session_metadata VALUES ('gw_migrated','Migrated title',
                    '2020-01-02T03:04:05Z','2020-01-02T03:04:05Z',1);",
            ).expect("master schema");
            if routing_columns {
                conn.execute_batch(
                    "ALTER TABLE session_metadata ADD COLUMN agent_alias TEXT;
                     ALTER TABLE session_metadata ADD COLUMN channel_id TEXT;
                     ALTER TABLE session_metadata ADD COLUMN room_id TEXT;
                     ALTER TABLE session_metadata ADD COLUMN sender_id TEXT;
                     ALTER TABLE session_metadata ADD COLUMN principal_id TEXT;
                     UPDATE session_metadata SET agent_alias='fixture', channel_id='discord.fixture',
                        room_id='fixture-room', sender_id='fixture-sender', principal_id='fixture-owner';",
                ).expect("routing metadata");
            }
            drop(conn);
            let original = std::fs::read(&db_path).expect("original bytes");
            upgrade_installation(directory.path(), backend, |_| "fallback".into())
                .expect("native migration");
            assert!(!db_path.exists());
            assert_eq!(
                std::fs::read(sessions.join("sessions.db.backup")).expect("backup"),
                original
            );

            let pool = EventStorePool::default();
            let catalog = SessionCatalog::open(directory.path(), &pool)
                .expect("catalog")
                .expect("native installation");
            let records = catalog.query().index_records().expect("index");
            assert_eq!(records.len(), 1);
            let record = &records[0];
            assert_eq!(record.session_id.as_str(), "gw_migrated");
            assert_eq!(record.title.as_deref(), Some("Migrated title"));
            assert_eq!(record.message_count, 1);
            assert_eq!(record.creation_source.as_deref(), Some("legacy"));
            assert_eq!(record.first_user_preview.as_deref(), Some("retained input"));
            assert_eq!(
                record.owning_agent.as_deref(),
                Some(if routing_columns {
                    "fixture"
                } else {
                    "fallback"
                })
            );
            if routing_columns {
                assert_eq!(record.metadata["channel_id"], "discord.fixture");
                assert_eq!(record.metadata["room_id"], "fixture-room");
                assert_eq!(record.metadata["sender_id"], "fixture-sender");
                assert_eq!(record.metadata["principal_id"], "fixture-owner");
            }
        }
    }
}
