//! Shared MySQL / MariaDB session-persistence implementation.
//!
//! MySQL and MariaDB speak the same wire protocol and accept the
//! same SQL for every operation we use (CREATE TABLE / INSERT /
//! SELECT / MATCH … AGAINST …). The two backends exposed to the
//! factory (`MySqlSessionBackend`, `MariaDbSessionBackend`) are
//! thin distinct-type wrappers around this shared `MySqlBackend`
//! so callers can tell which engine they configured, but the
//! connection pool, schema DDL, and query / mutation logic are
//! genuinely identical and live here.

use chrono::{DateTime, NaiveDateTime, Utc};
use mysql::prelude::Queryable;
use mysql::{Opts, OptsBuilder, Pool, PoolConstraints, PoolOpts, Value};
use zeroclaw_api::model_provider::ChatMessage;

use crate::session_backend::{
    SessionBackend, SessionContext, SessionMetadata, SessionQuery, SessionState,
};

/// Tag marker — distinct zero-sized types so the two
/// SessionBackend newtypes (`MySqlSessionBackend`,
/// `MariaDbSessionBackend`) can be distinguished in error / log
/// messages even though they're both sharing the same underlying
/// `MySqlBackend<…>` data path.
pub trait EngineTag {
    const NAME: &'static str;
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // Used only under backend-mysql.
pub enum MySqlTag {}
impl EngineTag for MySqlTag {
    const NAME: &'static str = "mysql";
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // Used only under backend-mariadb.
pub enum MariaDbTag {}
impl EngineTag for MariaDbTag {
    const NAME: &'static str = "mariadb";
}

/// Shared `SessionBackend` implementation for both MySQL and
/// MariaDB. The `Tag` parameter exists purely for log /
/// error-message differentiation; all data access goes through
/// the single `pool` field below.
pub struct MySqlBackend<Tag: EngineTag> {
    pool: Pool,
    engine_name: &'static str,
    _tag: std::marker::PhantomData<Tag>,
}

impl<Tag: EngineTag + Send + Sync> MySqlBackend<Tag> {
    pub fn new_with(url: &str, pool_size: u32, engine_name: &'static str) -> std::io::Result<Self> {
        let pool_size = usize::try_from(pool_size).unwrap_or(1).max(1);
        let opts = build_opts(url, pool_size)?;
        let pool = Pool::new(opts).map_err(|e| map_mysql_err(engine_name, e))?;
        let backend = Self {
            pool,
            engine_name,
            _tag: std::marker::PhantomData,
        };
        backend.ensure_schema()?;
        Ok(backend)
    }

    fn engine_name(&self) -> &'static str {
        self.engine_name
    }

    /// Run `op` against a pooled connection. The closure is
    /// expected to return `std::io::Result<T>` so callers can
    /// use `?` to short-circuit and convert mysql errors via
    /// `map_mysql_err` inline.
    fn with_conn<F, T>(&self, op: F) -> std::io::Result<T>
    where
        F: FnOnce(&mut mysql::PooledConn) -> std::io::Result<T>,
    {
        let mut conn = self
            .pool
            .get_conn()
            .map_err(|e| map_mysql_err(self.engine_name, e))?;
        op(&mut conn)
    }

    fn ensure_schema(&self) -> std::io::Result<()> {
        let en = self.engine_name();
        self.with_conn(|conn| -> std::io::Result<()> {
            conn.query_drop(
                "CREATE TABLE IF NOT EXISTS sessions (
                    id          BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,
                    session_key VARCHAR(512) NOT NULL,
                    role        VARCHAR(64) NOT NULL,
                    content     MEDIUMTEXT NOT NULL,
                    created_at  DATETIME(3) NOT NULL,
                    KEY idx_sessions_key (session_key),
                    KEY idx_sessions_key_id (session_key, id)
                ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4",
            )
            .map_err(|e| map_mysql_err(en, e))?;
            conn.query_drop(
                "CREATE TABLE IF NOT EXISTS session_metadata (
                    session_key    VARCHAR(512) NOT NULL PRIMARY KEY,
                    created_at     DATETIME(3) NOT NULL,
                    last_activity  DATETIME(3) NOT NULL,
                    message_count  BIGINT NOT NULL DEFAULT 0,
                    name           VARCHAR(255) NULL,
                    state          VARCHAR(32) NOT NULL DEFAULT 'idle',
                    turn_id        VARCHAR(255) NULL,
                    turn_started_at DATETIME(3) NULL,
                    agent_alias    VARCHAR(255) NULL,
                    channel_id     VARCHAR(255) NULL,
                    room_id        VARCHAR(255) NULL,
                    sender_id      VARCHAR(255) NULL,
                    KEY idx_session_metadata_agent_alias (agent_alias),
                    KEY idx_session_metadata_channel_id (channel_id),
                    KEY idx_session_metadata_room_id (room_id),
                    KEY idx_session_metadata_sender_id (sender_id)
                ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4",
            )
            .map_err(|e| map_mysql_err(en, e))?;
            let _ = conn.query_drop(
                "CREATE FULLTEXT INDEX idx_sessions_content_fulltext ON sessions(content)",
            );
            Ok(())
        })
    }

    fn now_value() -> Value {
        let nd = Utc::now().naive_utc();
        let formatted = nd.format("%Y-%m-%d %H:%M:%S%.3f").to_string();
        Value::Bytes(formatted.into_bytes())
    }

    fn parse_dt(value: Value) -> Option<DateTime<Utc>> {
        match value {
            Value::Date(y, m, d, h, mi, s, us) => {
                let date = chrono::NaiveDate::from_ymd_opt(y.into(), m.into(), d.into())?;
                let time = chrono::NaiveTime::from_hms_micro_opt(
                    u32::from(h),
                    u32::from(mi),
                    u32::from(s),
                    us,
                )?;
                Some(chrono::NaiveDateTime::new(date, time).and_utc())
            }
            Value::Bytes(bytes) => {
                let s = std::str::from_utf8(&bytes).ok()?;
                for fmt in [
                    "%Y-%m-%d %H:%M:%S%.3f",
                    "%Y-%m-%d %H:%M:%S",
                    "%Y-%m-%dT%H:%M:%S%.3f",
                    "%Y-%m-%dT%H:%M:%S",
                ] {
                    if let Ok(nd) = NaiveDateTime::parse_from_str(s, fmt) {
                        return Some(nd.and_utc());
                    }
                }
                DateTime::parse_from_rfc3339(s)
                    .ok()
                    .map(|dt| dt.with_timezone(&Utc))
            }
            Value::NULL => None,
            _ => None,
        }
    }

    fn row_to_metadata(
        key: String,
        created: Value,
        activity: Value,
        count: i64,
        name: Option<String>,
        agent_alias: Option<String>,
        channel_id: Option<String>,
        room_id: Option<String>,
        sender_id: Option<String>,
    ) -> SessionMetadata {
        let created_at = Self::parse_dt(created).unwrap_or_else(Utc::now);
        let last_activity = Self::parse_dt(activity).unwrap_or_else(Utc::now);
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let message_count = count.max(0) as usize;
        SessionMetadata {
            key,
            name,
            created_at,
            last_activity,
            message_count,
            agent_alias,
            channel_id,
            room_id,
            sender_id,
        }
    }

    fn row_to_state(state: String, turn_id: Option<String>, started: Value) -> SessionState {
        SessionState {
            state,
            turn_id,
            turn_started_at: Self::parse_dt(started),
        }
    }
}

fn build_opts(url: &str, pool_size: usize) -> std::io::Result<Opts> {
    let constraints =
        PoolConstraints::new(pool_size, pool_size).unwrap_or(PoolConstraints::DEFAULT);
    let pool_opts = PoolOpts::default().with_constraints(constraints);
    let opts = Opts::from_url(url).map_err(map_url_err)?;
    let opts: Opts = OptsBuilder::from_opts(opts).pool_opts(pool_opts).into();
    Ok(opts)
}

fn map_url_err(e: mysql::UrlError) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("session URL is not a valid mysql:// URL: {e}"),
    )
}

fn map_mysql_err(engine: &'static str, e: mysql::Error) -> std::io::Error {
    // Map mysql::Error variants to std::io::ErrorKind. The
    // upstream `mysql::Error` enum is non-exhaustive across
    // upstream versions (new variants land in patch releases),
    // so we MUST include a wildcard arm to compile against
    // versions that introduce new variants. We pick Other for
    // every known variant that doesn't already have a more
    // specific category.
    let kind = match &e {
        mysql::Error::IoError(_) => std::io::ErrorKind::Other,
        mysql::Error::CodecError(_) => std::io::ErrorKind::Other,
        mysql::Error::MySqlError(_) => std::io::ErrorKind::Other,
        mysql::Error::DriverError(_) => std::io::ErrorKind::Other,
        mysql::Error::UrlError(_) => std::io::ErrorKind::InvalidInput,
        mysql::Error::FromValueError(_) => std::io::ErrorKind::InvalidData,
        mysql::Error::FromRowError(_) => std::io::ErrorKind::InvalidData,
        #[allow(unreachable_patterns)]
        _ => std::io::ErrorKind::Other,
    };
    std::io::Error::new(kind, format!("session_backend={engine}: {e}"))
}

impl<Tag: EngineTag + Send + Sync> SessionBackend for MySqlBackend<Tag> {
    fn load(&self, session_key: &str) -> Vec<ChatMessage> {
        let en = self.engine_name();
        let result = self.with_conn(|conn| -> std::io::Result<Vec<(String, String)>> {
            conn.exec(
                "SELECT role, content FROM sessions WHERE session_key = ? ORDER BY id ASC",
                (session_key,),
            )
            .map_err(|e| map_mysql_err(en, e))
        });
        match result {
            Ok(rows) => rows
                .into_iter()
                .map(|(role, content)| ChatMessage { role, content })
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    fn load_with_timestamps(
        &self,
        session_key: &str,
    ) -> Vec<crate::session_backend::TimestampedMessage> {
        use crate::session_backend::TimestampedMessage;
        let en = self.engine_name();
        let result = self.with_conn(|conn| -> std::io::Result<Vec<(String, String, Value)>> {
            conn.exec(
                "SELECT role, content, created_at FROM sessions \
                     WHERE session_key = ? ORDER BY id ASC",
                (session_key,),
            )
            .map_err(|e| map_mysql_err(en, e))
        });
        match result {
            Ok(rows) => rows
                .into_iter()
                .map(|(role, content, created_at)| TimestampedMessage {
                    message: ChatMessage { role, content },
                    created_at: MySqlBackend::<Tag>::parse_dt(created_at),
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    fn append(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<()> {
        let en = self.engine_name();
        let now = MySqlBackend::<Tag>::now_value();
        self.with_conn(|conn| -> std::io::Result<()> {
            conn.exec_drop(
                "INSERT INTO sessions (session_key, role, content, created_at) \
                 VALUES (?, ?, ?, ?)",
                (session_key, &message.role, &message.content, now.clone()),
            )
            .map_err(|e| map_mysql_err(en, e))?;
            conn.exec_drop(
                "INSERT INTO session_metadata \
                    (session_key, created_at, last_activity, message_count) \
                 VALUES (?, ?, ?, 1) \
                 ON DUPLICATE KEY UPDATE \
                    last_activity = VALUES(last_activity), \
                    message_count = message_count + 1",
                (session_key, now.clone(), now.clone()),
            )
            .map_err(|e| map_mysql_err(en, e))?;
            Ok(())
        })
    }

    fn remove_last(&self, session_key: &str) -> std::io::Result<bool> {
        let en = self.engine_name();
        let row_id: Option<i64> = self.with_conn(|conn| -> std::io::Result<Option<i64>> {
            conn.exec_first(
                "SELECT id FROM sessions WHERE session_key = ? ORDER BY id DESC LIMIT 1",
                (session_key,),
            )
            .map_err(|e| map_mysql_err(en, e))
        })?;
        let Some(id) = row_id else {
            return Ok(false);
        };
        self.with_conn(|conn| -> std::io::Result<()> {
            conn.exec_drop("DELETE FROM sessions WHERE id = ?", (id,))
                .map_err(|e| map_mysql_err(en, e))
        })?;
        let now = MySqlBackend::<Tag>::now_value();
        let _ = self.with_conn(|conn| -> std::io::Result<()> {
            conn.exec_drop(
                "UPDATE session_metadata SET message_count = GREATEST(message_count - 1, 0), \
                                       last_activity = ? \
                 WHERE session_key = ?",
                (now, session_key),
            )
            .map_err(|e| map_mysql_err(en, e))
        });
        Ok(true)
    }

    fn update_last(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<bool> {
        let en = self.engine_name();
        let row_id: Option<i64> = self.with_conn(|conn| -> std::io::Result<Option<i64>> {
            conn.exec_first(
                "SELECT id FROM sessions WHERE session_key = ? ORDER BY id DESC LIMIT 1",
                (session_key,),
            )
            .map_err(|e| map_mysql_err(en, e))
        })?;
        let Some(id) = row_id else {
            return Ok(false);
        };
        self.with_conn(|conn| -> std::io::Result<()> {
            conn.exec_drop(
                "UPDATE sessions SET role = ?, content = ? WHERE id = ?",
                (&message.role, &message.content, id),
            )
            .map_err(|e| map_mysql_err(en, e))
        })?;
        let now = MySqlBackend::<Tag>::now_value();
        let _ = self.with_conn(|conn| -> std::io::Result<()> {
            conn.exec_drop(
                "UPDATE session_metadata SET last_activity = ? WHERE session_key = ?",
                (now, session_key),
            )
            .map_err(|e| map_mysql_err(en, e))
        });
        Ok(true)
    }

    fn list_sessions(&self) -> Vec<String> {
        let en = self.engine_name();
        let result = self.with_conn(|conn| -> std::io::Result<Vec<String>> {
            conn.query("SELECT session_key FROM session_metadata ORDER BY last_activity DESC")
                .map_err(|e| map_mysql_err(en, e))
        });
        result.unwrap_or_default()
    }

    fn list_sessions_with_metadata(&self) -> Vec<SessionMetadata> {
        type Row = (
            String,
            Value,
            Value,
            i64,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        );
        let en = self.engine_name();
        let result = self.with_conn(|conn| -> std::io::Result<Vec<Row>> {
            conn.query(
                "SELECT session_key, created_at, last_activity, message_count, \
                        name, agent_alias, channel_id, room_id, sender_id \
                 FROM session_metadata ORDER BY last_activity DESC",
            )
            .map_err(|e| map_mysql_err(en, e))
        });
        match result {
            Ok(rows) => rows
                .into_iter()
                .map(
                    |(key, created, activity, count, name, agent, channel, room, sender)| {
                        MySqlBackend::<Tag>::row_to_metadata(
                            key, created, activity, count, name, agent, channel, room, sender,
                        )
                    },
                )
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    fn cleanup_stale(&self, ttl_hours: u32) -> std::io::Result<usize> {
        let en = self.engine_name();
        let cutoff_stmt = format!(
            "SELECT session_key FROM session_metadata \
             WHERE last_activity < DATE_SUB(UTC_TIMESTAMP(3), INTERVAL {ttl_hours} HOUR)"
        );
        let stale: Vec<String> = self.with_conn(|conn| -> std::io::Result<Vec<String>> {
            conn.query(cutoff_stmt.as_str())
                .map_err(|e| map_mysql_err(en, e))
        })?;
        let count = stale.len();
        for key in stale {
            let _ = self.with_conn(|conn| -> std::io::Result<()> {
                conn.exec_drop("DELETE FROM sessions WHERE session_key = ?", (key.clone(),))
                    .map_err(|e| map_mysql_err(en, e))
            });
            let _ = self.with_conn(|conn| -> std::io::Result<()> {
                conn.exec_drop("DELETE FROM session_metadata WHERE session_key = ?", (key,))
                    .map_err(|e| map_mysql_err(en, e))
            });
        }
        Ok(count)
    }

    fn clear_messages(&self, session_key: &str) -> std::io::Result<usize> {
        let en = self.engine_name();
        let affected: u64 = self.with_conn(|conn| -> std::io::Result<u64> {
            conn.exec_drop("DELETE FROM sessions WHERE session_key = ?", (session_key,))
                .map_err(|e| map_mysql_err(en, e))?;
            Ok(conn.affected_rows())
        })?;
        let count = affected as usize;
        if count > 0 {
            let now = MySqlBackend::<Tag>::now_value();
            let _ = self.with_conn(|conn| -> std::io::Result<()> {
                conn.exec_drop(
                    "UPDATE session_metadata SET message_count = 0, last_activity = ? \
                     WHERE session_key = ?",
                    (now, session_key),
                )
                .map_err(|e| map_mysql_err(en, e))
            });
        }
        Ok(count)
    }

    fn delete_session(&self, session_key: &str) -> std::io::Result<bool> {
        let en = self.engine_name();
        let exists: Option<i64> = self.with_conn(|conn| -> std::io::Result<Option<i64>> {
            conn.exec_first(
                "SELECT 1 FROM session_metadata WHERE session_key = ? LIMIT 1",
                (session_key,),
            )
            .map_err(|e| map_mysql_err(en, e))
        })?;
        if exists.is_none() {
            return Ok(false);
        }
        self.with_conn(|conn| -> std::io::Result<()> {
            conn.exec_drop("DELETE FROM sessions WHERE session_key = ?", (session_key,))
                .map_err(|e| map_mysql_err(en, e))
        })?;
        self.with_conn(|conn| -> std::io::Result<()> {
            conn.exec_drop(
                "DELETE FROM session_metadata WHERE session_key = ?",
                (session_key,),
            )
            .map_err(|e| map_mysql_err(en, e))
        })?;
        Ok(true)
    }

    fn clear_agent_attribution(&self, agent_alias: &str) -> std::io::Result<usize> {
        let en = self.engine_name();
        let affected: u64 = self.with_conn(|conn| -> std::io::Result<u64> {
            conn.exec_drop(
                "UPDATE session_metadata SET agent_alias = NULL WHERE agent_alias = ?",
                (agent_alias,),
            )
            .map_err(|e| map_mysql_err(en, e))?;
            Ok(conn.affected_rows())
        })?;
        Ok(affected as usize)
    }

    fn rename_agent_attribution(&self, from: &str, to: &str) -> std::io::Result<usize> {
        let en = self.engine_name();
        let affected: u64 = self.with_conn(|conn| -> std::io::Result<u64> {
            conn.exec_drop(
                "UPDATE session_metadata SET agent_alias = ? WHERE agent_alias = ?",
                (to, from),
            )
            .map_err(|e| map_mysql_err(en, e))?;
            Ok(conn.affected_rows())
        })?;
        Ok(affected as usize)
    }

    fn count_agent_attribution(&self, agent_alias: &str) -> std::io::Result<usize> {
        let en = self.engine_name();
        let count: Option<i64> = self.with_conn(|conn| -> std::io::Result<Option<i64>> {
            conn.exec_first(
                "SELECT COUNT(*) FROM session_metadata WHERE agent_alias = ?",
                (agent_alias,),
            )
            .map_err(|e| map_mysql_err(en, e))
        })?;
        Ok(count.unwrap_or(0).max(0) as usize)
    }

    fn session_exists(&self, session_key: &str) -> bool {
        let en = self.engine_name();
        let exists_result = self.with_conn(|conn| -> std::io::Result<Option<i64>> {
            conn.exec_first(
                "SELECT 1 FROM session_metadata WHERE session_key = ? LIMIT 1",
                (session_key,),
            )
            .map_err(|e| map_mysql_err(en, e))
        });
        // exists_result is `io::Result<Option<i64>>`. A DB
        // error returns Err (which we silently absorb per the
        // trait contract that `session_exists` cannot fail);
        // a missing row unwraps to None; a present row unwraps
        // to Some(1).
        exists_result.ok().and_then(|inner| inner).is_some()
    }

    fn set_session_name(&self, session_key: &str, name: &str) -> std::io::Result<()> {
        let en = self.engine_name();
        let name_val = if name.is_empty() { None } else { Some(name) };
        self.with_conn(|conn| -> std::io::Result<()> {
            conn.exec_drop(
                "UPDATE session_metadata SET name = ? WHERE session_key = ?",
                (name_val, session_key),
            )
            .map_err(|e| map_mysql_err(en, e))
        })
    }

    fn get_session_name(&self, session_key: &str) -> std::io::Result<Option<String>> {
        let en = self.engine_name();
        let name: Option<Option<String>> =
            self.with_conn(|conn| -> std::io::Result<Option<Option<String>>> {
                conn.exec_first(
                    "SELECT name FROM session_metadata WHERE session_key = ?",
                    (session_key,),
                )
                .map_err(|e| map_mysql_err(en, e))
            })?;
        // Flatten the Option<Option<String>>: outer Some means
        // a row matched, inner Option reflects NULL column.
        Ok(name.flatten())
    }

    fn get_session_metadata(&self, session_key: &str) -> Option<SessionMetadata> {
        type Row = (
            String,
            Value,
            Value,
            i64,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        );
        let en = self.engine_name();
        let row: Option<Row> = self
            .with_conn(|conn| -> std::io::Result<Option<Row>> {
                conn.exec_first(
                    "SELECT session_key, created_at, last_activity, message_count, \
                            name, agent_alias, channel_id, room_id, sender_id \
                     FROM session_metadata WHERE session_key = ?",
                    (session_key,),
                )
                .map_err(|e| map_mysql_err(en, e))
            })
            .ok()
            .flatten();
        let (key, created, activity, count, name, agent, channel, room, sender) = row?;
        Some(MySqlBackend::<Tag>::row_to_metadata(
            key, created, activity, count, name, agent, channel, room, sender,
        ))
    }

    fn set_session_state(
        &self,
        session_key: &str,
        state: &str,
        turn_id: Option<&str>,
    ) -> std::io::Result<()> {
        let en = self.engine_name();
        let now = MySqlBackend::<Tag>::now_value();
        let started: Option<Value> = if state == "running" {
            Some(now.clone())
        } else {
            None
        };
        // turn_id may be Some("") or Some(&str) — store NULL for empty.
        let turn_id_val: Option<String> = turn_id.filter(|s| !s.is_empty()).map(|s| s.to_string());
        self.with_conn(|conn| -> std::io::Result<()> {
            conn.exec_drop(
                "UPDATE session_metadata \
                 SET state = ?, turn_id = ?, turn_started_at = ? \
                 WHERE session_key = ?",
                (state, turn_id_val, started, session_key),
            )
            .map_err(|e| map_mysql_err(en, e))
        })
    }

    fn get_session_state(&self, session_key: &str) -> std::io::Result<Option<SessionState>> {
        type Row = (String, Option<String>, Option<Value>);
        let en = self.engine_name();
        let row: Option<Row> = self
            .with_conn(|conn| -> std::io::Result<Option<Row>> {
                conn.exec_first(
                    "SELECT state, turn_id, turn_started_at FROM session_metadata \
                     WHERE session_key = ?",
                    (session_key,),
                )
                .map_err(|e| map_mysql_err(en, e))
            })
            .ok()
            .flatten();
        let (state, turn_id, started) = match row {
            Some(r) => r,
            None => return Ok(None),
        };
        let started = started.unwrap_or(Value::NULL);
        Ok(Some(MySqlBackend::<Tag>::row_to_state(
            state, turn_id, started,
        )))
    }

    fn list_running_sessions(&self) -> Vec<SessionMetadata> {
        type Row = (
            String,
            Value,
            Value,
            i64,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        );
        let en = self.engine_name();
        let result = self.with_conn(|conn| -> std::io::Result<Vec<Row>> {
            conn.query(
                "SELECT session_key, created_at, last_activity, message_count, \
                        name, agent_alias, channel_id, room_id, sender_id \
                 FROM session_metadata \
                 WHERE state = 'running' \
                 ORDER BY turn_started_at DESC",
            )
            .map_err(|e| map_mysql_err(en, e))
        });
        match result {
            Ok(rows) => rows
                .into_iter()
                .map(
                    |(key, created, activity, count, name, agent, channel, room, sender)| {
                        MySqlBackend::<Tag>::row_to_metadata(
                            key, created, activity, count, name, agent, channel, room, sender,
                        )
                    },
                )
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    fn list_stuck_sessions(&self, threshold_secs: u64) -> Vec<SessionMetadata> {
        type Row = (
            String,
            Value,
            Value,
            i64,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        );
        let en = self.engine_name();
        let stmt = format!(
            "SELECT session_key, created_at, last_activity, message_count, \
                    name, agent_alias, channel_id, room_id, sender_id \
             FROM session_metadata \
             WHERE state = 'running' \
               AND turn_started_at < DATE_SUB(UTC_TIMESTAMP(3), INTERVAL {threshold_secs} SECOND) \
             ORDER BY turn_started_at ASC"
        );
        let result = self.with_conn(|conn| -> std::io::Result<Vec<Row>> {
            conn.query(stmt.as_str()).map_err(|e| map_mysql_err(en, e))
        });
        match result {
            Ok(rows) => rows
                .into_iter()
                .map(
                    |(key, created, activity, count, name, agent, channel, room, sender)| {
                        MySqlBackend::<Tag>::row_to_metadata(
                            key, created, activity, count, name, agent, channel, room, sender,
                        )
                    },
                )
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    fn search(&self, query: &SessionQuery) -> Vec<SessionMetadata> {
        let Some(keyword) = query.keyword.as_deref() else {
            return self.list_sessions_with_metadata();
        };
        let limit = query.limit.unwrap_or(50) as i64;
        let en = self.engine_name();
        let fts_query = build_fts_query(keyword);
        let stmt = format!(
            "SELECT DISTINCT session_key \
             FROM sessions \
             WHERE MATCH(content) AGAINST (? IN NATURAL LANGUAGE MODE) \
             LIMIT {limit}"
        );
        let keys: Vec<String> = match self.with_conn(|conn| -> std::io::Result<Vec<String>> {
            conn.exec(stmt.as_str(), (fts_query.as_str(),))
                .map_err(|e| map_mysql_err(en, e))
        }) {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        };
        if keys.is_empty() {
            return Vec::new();
        }
        let placeholders = keys.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let meta_stmt = format!(
            "SELECT session_key, created_at, last_activity, message_count, \
                    name, agent_alias, channel_id, room_id, sender_id \
             FROM session_metadata WHERE session_key IN ({placeholders})"
        );
        type Row = (
            String,
            Value,
            Value,
            i64,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        );
        let params: mysql::Params = mysql::Params::from(keys);
        let result = self.with_conn(|conn| -> std::io::Result<Vec<Row>> {
            conn.exec(meta_stmt.as_str(), params)
                .map_err(|e| map_mysql_err(en, e))
        });
        match result {
            Ok(rows) => rows
                .into_iter()
                .map(
                    |(key, created, activity, count, name, agent, channel, room, sender)| {
                        MySqlBackend::<Tag>::row_to_metadata(
                            key, created, activity, count, name, agent, channel, room, sender,
                        )
                    },
                )
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    fn set_session_agent_alias(&self, session_key: &str, agent_alias: &str) -> std::io::Result<()> {
        let en = self.engine_name();
        let alias_val = if agent_alias.is_empty() {
            None
        } else {
            Some(agent_alias)
        };
        let now = MySqlBackend::<Tag>::now_value();
        self.with_conn(|conn| -> std::io::Result<()> {
            conn.exec_drop(
                "INSERT INTO session_metadata \
                    (session_key, created_at, last_activity, message_count, agent_alias) \
                 VALUES (?, ?, ?, 0, ?) \
                 ON DUPLICATE KEY UPDATE agent_alias = VALUES(agent_alias)",
                (session_key, now.clone(), now.clone(), alias_val),
            )
            .map_err(|e| map_mysql_err(en, e))
        })
    }

    fn get_session_agent_alias(&self, session_key: &str) -> std::io::Result<Option<String>> {
        let en = self.engine_name();
        let alias: Option<Option<String>> =
            self.with_conn(|conn| -> std::io::Result<Option<Option<String>>> {
                conn.exec_first(
                    "SELECT agent_alias FROM session_metadata WHERE session_key = ?",
                    (session_key,),
                )
                .map_err(|e| map_mysql_err(en, e))
            })?;
        Ok(alias.flatten())
    }

    fn set_session_context(
        &self,
        session_key: &str,
        context: SessionContext<'_>,
    ) -> std::io::Result<()> {
        let en = self.engine_name();
        let normalize = |v: Option<&str>| -> Option<String> {
            v.map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        };
        let channel_id = normalize(context.channel_id);
        let room_id = normalize(context.room_id);
        let sender_id = normalize(context.sender_id);
        let now = MySqlBackend::<Tag>::now_value();
        self.with_conn(|conn| -> std::io::Result<()> {
            conn.exec_drop(
                "INSERT INTO session_metadata \
                    (session_key, created_at, last_activity, message_count, \
                     channel_id, room_id, sender_id) \
                 VALUES (?, ?, ?, 0, ?, ?, ?) \
                 ON DUPLICATE KEY UPDATE \
                    channel_id = COALESCE(VALUES(channel_id), channel_id), \
                    room_id    = COALESCE(VALUES(room_id),    room_id), \
                    sender_id  = COALESCE(VALUES(sender_id),  sender_id)",
                (
                    session_key,
                    now.clone(),
                    now.clone(),
                    channel_id,
                    room_id,
                    sender_id,
                ),
            )
            .map_err(|e| map_mysql_err(en, e))
        })
    }
}

/// Build a MATCH … AGAINST clause payload from a free-text
/// keyword.
fn build_fts_query(keyword: &str) -> String {
    keyword
        .split_whitespace()
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{w}\""))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Resolve the shared `pool_size` config field, mirroring the
/// foundation PR's `default_session_pool_size` of 5. Operators
/// who want to override set `ZEROCLAW_channels__pool_size=<int>`.
/// This is the same dotted-path env override
/// `crate::env_overrides` injects into `channels.pool_size`;
/// reading it directly here matches that contract.
pub(crate) fn read_pool_size() -> u32 {
    std::env::var("ZEROCLAW_channels__pool_size")
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(5)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_dt_handles_textual_form() {
        let v = Value::Bytes(b"2026-07-21 20:00:00.123".to_vec());
        let dt = MySqlBackend::<MySqlTag>::parse_dt(v).expect("parses textual");
        assert_eq!(dt.to_rfc3339(), "2026-07-21T20:00:00.123+00:00");
    }

    #[test]
    fn parse_dt_handles_binary_form() {
        let v = Value::Date(2026, 7, 21, 20, 0, 0, 0);
        let dt = MySqlBackend::<MySqlTag>::parse_dt(v).expect("parses binary");
        assert_eq!(dt.to_rfc3339(), "2026-07-21T20:00:00+00:00");
    }

    #[test]
    fn parse_dt_handles_null() {
        let dt = MySqlBackend::<MySqlTag>::parse_dt(Value::NULL);
        assert!(dt.is_none());
    }

    #[test]
    fn fts_query_quotes_each_token() {
        let q = build_fts_query("rust async (best)");
        assert_eq!(q, "\"rust\" \"async\" \"(best)\"");
    }

    #[test]
    fn fts_query_empty_yields_empty_string() {
        let q = build_fts_query("   \t  \n ");
        assert_eq!(q, "");
    }
}
