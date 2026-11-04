//! Read-only master-format inputs for installation migration. Native runtime
//! code does not construct a legacy session backend or initialize its schema.
use super::super::import::TimestampedMessage;
use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::io::{BufRead, Read};
use std::path::Path;
use zeroclaw_api::model_provider::ChatMessage;

pub(super) struct SessionMetadata {
    pub title: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub agent_alias: Option<String>,
    pub channel_id: Option<String>,
    pub room_id: Option<String>,
    pub sender_id: Option<String>,
    pub principal_id: Option<String>,
}

pub(super) struct LegacySqliteReader {
    conn: Connection,
}
impl LegacySqliteReader {
    pub(super) fn open(directory: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(
            directory.join("sessions.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Self { conn })
    }
    fn has_metadata(&self) -> rusqlite::Result<bool> {
        self.conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='session_metadata')", [], |row| row.get(0))
    }
    fn metadata_column<'a>(&self, name: &'a str) -> rusqlite::Result<&'a str> {
        let present: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('session_metadata') WHERE name=?1)",
            [name],
            |row| row.get(0),
        )?;
        Ok(if present { name } else { "NULL" })
    }
    pub(super) fn get_session_trim_breadcrumb(&self, key: &str) -> Result<Option<bool>> {
        if !self.has_metadata()? || self.metadata_column("trim_breadcrumb")? == "NULL" {
            return Ok(None);
        }
        Ok(self
            .conn
            .query_row(
                "SELECT trim_breadcrumb FROM session_metadata WHERE session_key=?1",
                [key],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten()
            .map(|v| v != 0))
    }
    /// Read original rows and clocks without swallowing migration errors.
    pub(crate) fn upgrade_messages(&self, session_key: &str) -> Result<Vec<TimestampedMessage>> {
        let conn = &self.conn;
        let mut statement = conn.prepare(
            "SELECT role, content, created_at FROM sessions WHERE session_key=?1 ORDER BY id ASC",
        )?;
        let rows = statement.query_map(params![session_key], |row| {
            let raw: Option<String> = row.get(2)?;
            Ok(TimestampedMessage {
                message: ChatMessage {
                    timestamp: None,
                    tool_calls: Vec::new(),
                    tool_result: None,
                    reasoning_content: None,
                    role: row.get(0)?,
                    content: row.get(1)?,

                    attachments: Vec::new(),
                },
                created_at: raw
                    .as_deref()
                    .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                    .map(|time| time.with_timezone(&Utc)),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub(crate) fn upgrade_keys(&self) -> Result<Vec<String>> {
        let conn = &self.conn;
        let sql = if self.has_metadata()? {
            "SELECT session_key FROM session_metadata UNION SELECT session_key FROM sessions ORDER BY session_key"
        } else {
            "SELECT DISTINCT session_key FROM sessions ORDER BY session_key"
        };
        let mut statement = conn.prepare(sql)?;
        let keys = statement
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(keys)
    }

    pub(crate) fn upgrade_metadata(&self, session_key: &str) -> Result<Option<SessionMetadata>> {
        let conn = &self.conn;
        if !self.has_metadata()? {
            return Ok(None);
        }
        let columns = [
            "name",
            "agent_alias",
            "channel_id",
            "room_id",
            "sender_id",
            "principal_id",
        ]
        .into_iter()
        .map(|name| self.metadata_column(name))
        .collect::<rusqlite::Result<Vec<_>>>()?
        .join(",");
        let sql = format!(
            "SELECT created_at,last_activity,message_count,{columns} FROM session_metadata WHERE session_key=?1"
        );
        let raw = conn
            .query_row(&sql, params![session_key], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                ))
            })
            .optional()?;
        raw.map(
            |(
                created,
                activity,
                count,
                title,
                agent_alias,
                channel_id,
                room_id,
                sender_id,
                principal_id,
            )| {
                let _ = usize::try_from(count)?;
                Ok(SessionMetadata {
                    title,
                    agent_alias,
                    channel_id,
                    room_id,
                    sender_id,
                    principal_id,
                    created_at: DateTime::parse_from_rfc3339(&created)?.with_timezone(&Utc),
                    last_activity: DateTime::parse_from_rfc3339(&activity)?.with_timezone(&Utc),
                })
            },
        )
        .transpose()
    }
}

pub(super) fn read_jsonl(file: std::fs::File) -> std::io::Result<Vec<ChatMessage>> {
    std::io::BufReader::new(file)
        .lines()
        .filter_map(|line| match line {
            Ok(line) if line.trim().is_empty() => None,
            Ok(line) => Some(
                serde_json::from_str(&line)
                    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
            ),
            Err(error) => Some(Err(error)),
        })
        .collect()
}

pub(super) fn read_trim_breadcrumb(path: &Path) -> std::io::Result<Option<bool>> {
    let mut file = match super::super::filesystem::reader(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    match bytes.as_slice() {
        [b'0'] => Ok(Some(false)),
        [b'1'] => Ok(Some(true)),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "corrupt legacy trim breadcrumb",
        )),
    }
}

/// Settle the old master's run-state column before its history is migrated.
/// Does not create a database or initialize/upgrade its schema.
pub(crate) fn recover_abandoned_turns_at(data_dir: &Path) -> std::io::Result<usize> {
    let path = data_dir.join("sessions/sessions.db");
    if !path.try_exists()? {
        return Ok(0);
    }
    let _file = super::super::filesystem::reader(&path)?;
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(std::io::Error::other)?;
    let has_state: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('session_metadata') WHERE name='state')",
            [],
            |row| row.get(0),
        )
        .map_err(std::io::Error::other)?;
    if !has_state {
        return Ok(0);
    }
    conn.execute(
        "UPDATE session_metadata SET state='error',turn_started_at=NULL WHERE state='running'",
        [],
    )
    .map_err(std::io::Error::other)
}
