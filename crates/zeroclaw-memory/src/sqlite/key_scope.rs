//! SQLite physical-key encoding. Existing writers mint UUID row IDs; only new
//! encoded rows carry this ID tag. A key-shaped legacy string alone is never
//! decoded. The session/category columns remain the scope authority, including
//! when an older binary updates a row or startup normalizes a legacy session.
use rusqlite::{Connection, params};
use std::fmt::Write as _;
use uuid::Uuid;

pub(super) use crate::point_scope::PointScope;

const TAG: &str = "zc-key-v1:";

fn key_prefix(key: &str) -> String {
    let mut prefix = String::from(TAG);
    for byte in key.as_bytes() {
        let _ = write!(prefix, "{byte:02x}");
    }
    prefix.push(':');
    prefix
}

pub(super) fn encode(key: &str, session: Option<&str>) -> anyhow::Result<(String, String)> {
    let uuid = Uuid::new_v4().to_string();
    let physical = format!(
        "{}{}",
        key_prefix(key),
        serde_json::to_string(&(key, session, &uuid))?
    );
    Ok((format!("{TAG}{uuid}"), physical))
}

pub(crate) fn decode(id: &str, physical: String) -> String {
    let Some(uuid) = id.strip_prefix(TAG) else {
        return physical;
    };
    let Some(rest) = physical.strip_prefix(TAG) else {
        return physical;
    };
    let Some((_, payload)) = rest.split_once(':') else {
        return physical;
    };
    let Ok((key, _session, nonce)) =
        serde_json::from_str::<(String, Option<String>, String)>(payload)
    else {
        return physical;
    };
    if nonce == uuid && Uuid::parse_str(uuid).is_ok() && physical.starts_with(&key_prefix(&key)) {
        key
    } else {
        physical
    }
}

pub(super) fn row_key(row: &rusqlite::Row<'_>) -> rusqlite::Result<String> {
    Ok(decode(&row.get::<_, String>(0)?, row.get(1)?))
}

pub(super) struct KeyRow {
    pub id: String,
    pub physical: String,
    pub category: String,
    pub session: Option<String>,
    pub agent: String,
    pub namespace: String,
}

impl KeyRow {
    pub fn conversation_session(&self) -> Option<&str> {
        if self.category == "conversation" {
            self.session.as_deref()
        } else {
            None
        }
    }
}

impl crate::point_scope::PointRow for KeyRow {
    fn is_conversation(&self) -> bool {
        self.category == "conversation"
    }

    fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    fn agent(&self) -> &str {
        &self.agent
    }

    fn namespace(&self) -> &str {
        &self.namespace
    }
}

pub(super) fn rows(
    conn: &Connection,
    key: &str,
    agent: Option<&str>,
) -> anyhow::Result<Vec<KeyRow>> {
    // The prefix contains only our literal tag and hex digits: user glob
    // metacharacters cannot enter the pattern. SQLite can use its key index.
    let pattern = format!("{}*", key_prefix(key));
    let mut stmt = conn.prepare(
        "SELECT id, key, category, session_id, agent_id, COALESCE(namespace, 'default')
         FROM memories WHERE (?1 IS NULL OR agent_id = ?1)
         AND (key = ?2 OR key GLOB ?3) ORDER BY id",
    )?;
    let candidates = stmt.query_map(params![agent, key, pattern], |row| {
        Ok(KeyRow {
            id: row.get(0)?,
            physical: row.get(1)?,
            category: row.get(2)?,
            session: row.get(3)?,
            agent: row.get(4)?,
            namespace: row.get(5)?,
        })
    })?;
    let mut result = Vec::new();
    for row in candidates {
        let row = row?;
        if decode(&row.id, row.physical.clone()) == key {
            result.push(row);
        }
    }
    Ok(result)
}
