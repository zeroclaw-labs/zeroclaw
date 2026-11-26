//! Restart-safe coordination of one committed ZeroCode session effect with its
//! exact Goal cleanup. Neither the intent nor a backend receipt grants access
//! to a session: the runtime must validate current authority before any effect.

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use zeroclaw_goal::control::GoalSessionKey;
use zeroclaw_infra::session_disposition::{SessionDispositionBackend, SessionDispositionIdentity};

use crate::control_plane::authority::is_authoritative_owner;
use crate::control_plane::task_registry::{TaskRecord, TaskStatus};

use super::{SqliteTaskStore, goal};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionDispositionKind {
    Replace,
    Close,
    Kill,
    Delete,
}

impl SessionDispositionKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Replace => "replace",
            Self::Close => "close",
            Self::Kill => "kill",
            Self::Delete => "delete",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SessionDispositionOrigin {
    Live,
    Durable {
        backend_key: String,
        identity: SessionDispositionIdentity,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionDispositionPhase {
    Prepared,
    SessionCommitted,
}

/// The runtime-owned operation, bound to one quiesced Goal by its exact task
/// identity and epoch. The owner is the session-effect actor, not the recorded
/// Goal executor. No credentials, principals or permission decisions are copied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionDisposition {
    pub(crate) operation_id: String,
    pub(crate) session_id: String,
    pub(crate) task_id: String,
    pub(crate) execution_epoch: i64,
    pub(crate) kind: SessionDispositionKind,
    pub(crate) origin: SessionDispositionOrigin,
    pub(crate) owner_pid: u32,
    pub(crate) owner_boot_id: String,
    pub(crate) live_generation: Option<u64>,
    pub(crate) phase: SessionDispositionPhase,
}

impl SessionDisposition {
    pub(crate) fn new(
        session_id: String,
        task: &TaskRecord,
        kind: SessionDispositionKind,
        origin: SessionDispositionOrigin,
        owner_boot_id: String,
        live_generation: Option<u64>,
    ) -> Self {
        Self {
            operation_id: uuid::Uuid::new_v4().to_string(),
            session_id,
            task_id: task.id.clone(),
            execution_epoch: task.execution_epoch,
            kind,
            origin,
            owner_pid: std::process::id(),
            owner_boot_id,
            live_generation,
            phase: SessionDispositionPhase::Prepared,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DispositionResult {
    /// The exact operation transitioned durably.
    Applied,
    /// The exact requested result already holds. This never authorizes replay
    /// of a live effect.
    AlreadySettled,
    /// Stale identity, wrong phase, foreign owner or an unsettled binding.
    Conflict,
}

pub(super) fn create_schema(conn: &Connection) -> Result<()> {
    // Intentionally not a foreign key: a missing old task is a state to
    // reconcile, not permission to forget a possibly committed operation.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS goal_session_dispositions (
            session_id TEXT PRIMARY KEY NOT NULL
                CHECK(substr(session_id,1,4) = 'rpc_' AND length(session_id) > 4),
            operation_id TEXT UNIQUE NOT NULL CHECK(length(operation_id) = 36),
            task_id TEXT NOT NULL CHECK(length(trim(task_id)) > 0),
            execution_epoch INTEGER NOT NULL
                CHECK(typeof(execution_epoch) = 'integer' AND execution_epoch >= 1),
            kind TEXT NOT NULL CHECK(kind IN ('replace','close','kill','delete')),
            backend TEXT CHECK(backend IN ('chat_sqlite','chat_jsonl','acp_sqlite')),
            backend_key TEXT CHECK(length(trim(backend_key)) > 0),
            backend_instance TEXT CHECK(length(trim(backend_instance)) > 0),
            owner_pid INTEGER NOT NULL
                CHECK(typeof(owner_pid) = 'integer' AND owner_pid BETWEEN 1 AND 4294967295),
            owner_boot_id TEXT NOT NULL CHECK(length(trim(owner_boot_id)) > 0),
            live_generation INTEGER
                CHECK(live_generation IS NULL OR
                    (typeof(live_generation) = 'integer' AND live_generation >= 0)),
            phase TEXT NOT NULL CHECK(phase IN ('prepared','session_committed')),
            CHECK((backend IS NULL AND backend_key IS NULL AND backend_instance IS NULL
                   AND live_generation IS NOT NULL)
               OR (backend IS NOT NULL AND backend_key IS NOT NULL AND backend_instance IS NOT NULL
                   AND kind IN ('kill','delete')))
        );",
    )
    .context("create Goal session disposition schema")
}

pub(super) fn is_outstanding(conn: &Connection, session_id: &str) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM goal_session_dispositions WHERE session_id = ?1)",
        params![session_id],
        |row| row.get(0),
    )
    .context("check Goal session disposition fence")
}

fn invalid_field(column: usize) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        rusqlite::types::Type::Text,
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid session disposition",
        )
        .into(),
    )
}

fn row_to_disposition(row: &Row<'_>) -> rusqlite::Result<SessionDisposition> {
    let kind = match row.get::<_, String>("kind")?.as_str() {
        "replace" => SessionDispositionKind::Replace,
        "close" => SessionDispositionKind::Close,
        "kill" => SessionDispositionKind::Kill,
        "delete" => SessionDispositionKind::Delete,
        _ => return Err(invalid_field(row.as_ref().column_index("kind")?)),
    };
    let backend = match row.get::<_, Option<String>>("backend")?.as_deref() {
        None => None,
        Some("chat_sqlite") => Some(SessionDispositionBackend::ChatSqlite),
        Some("chat_jsonl") => Some(SessionDispositionBackend::ChatJsonl),
        Some("acp_sqlite") => Some(SessionDispositionBackend::AcpSqlite),
        Some(_) => return Err(invalid_field(row.as_ref().column_index("backend")?)),
    };
    let origin = match backend {
        None => SessionDispositionOrigin::Live,
        Some(backend) => SessionDispositionOrigin::Durable {
            backend_key: row.get("backend_key")?,
            identity: SessionDispositionIdentity {
                backend,
                instance_id: row.get("backend_instance")?,
            },
        },
    };
    let phase = match row.get::<_, String>("phase")?.as_str() {
        "prepared" => SessionDispositionPhase::Prepared,
        "session_committed" => SessionDispositionPhase::SessionCommitted,
        _ => return Err(invalid_field(row.as_ref().column_index("phase")?)),
    };
    let intent = SessionDisposition {
        operation_id: row.get("operation_id")?,
        session_id: row.get("session_id")?,
        task_id: row.get("task_id")?,
        execution_epoch: row.get("execution_epoch")?,
        kind,
        origin,
        owner_pid: row.get("owner_pid")?,
        owner_boot_id: row.get("owner_boot_id")?,
        live_generation: row.get("live_generation")?,
        phase,
    };
    let session_column = row.as_ref().column_index("session_id")?;
    validate_binding(&intent).map_err(|_| invalid_field(session_column))?;
    Ok(intent)
}

fn read_disposition(conn: &Connection, session_id: &str) -> Result<Option<SessionDisposition>> {
    conn.query_row(
        "SELECT * FROM goal_session_dispositions WHERE session_id = ?1",
        params![session_id],
        row_to_disposition,
    )
    .optional()
    .context("read Goal session disposition")
}

pub(super) fn validate_zero_code_session(session_id: &str) -> Result<()> {
    let raw_id = session_id
        .strip_prefix("rpc_")
        .context("not a ZeroCode Goal session")?;
    ensure!(
        GoalSessionKey::zero_code(raw_id)?.durable_id() == session_id,
        "session disposition requires a canonical Goal session"
    );
    Ok(())
}

fn validate_binding(intent: &SessionDisposition) -> Result<()> {
    validate_zero_code_session(&intent.session_id)?;
    let operation_id =
        uuid::Uuid::parse_str(&intent.operation_id).context("invalid session disposition UUID")?;
    ensure!(
        operation_id.get_version() == Some(uuid::Version::Random)
            && operation_id.to_string() == intent.operation_id
            && intent.owner_pid > 0
            && !intent.owner_boot_id.trim().is_empty()
            && !intent.task_id.trim().is_empty()
            && intent.execution_epoch >= 1,
        "session disposition has invalid operation identity or binding"
    );
    match &intent.origin {
        SessionDispositionOrigin::Live => ensure!(
            intent.live_generation.is_some(),
            "live session disposition requires an exact generation"
        ),
        SessionDispositionOrigin::Durable {
            backend_key,
            identity,
        } => ensure!(
            matches!(
                intent.kind,
                SessionDispositionKind::Delete | SessionDispositionKind::Kill
            ) && !backend_key.trim().is_empty()
                && !identity.instance_id.trim().is_empty(),
            "durable session disposition requires an exact backend effect and identity"
        ),
    }
    Ok(())
}

fn authorize_owner(intent: &SessionDisposition, pid: u32, boot_id: &str) -> bool {
    pid == intent.owner_pid && boot_id == intent.owner_boot_id
        || is_authoritative_owner(intent.owner_pid, &intent.owner_boot_id)
}

fn same_operation(actual: &SessionDisposition, expected: &SessionDisposition) -> bool {
    actual.operation_id == expected.operation_id
        && actual.session_id == expected.session_id
        && actual.task_id == expected.task_id
        && actual.execution_epoch == expected.execution_epoch
        && actual.kind == expected.kind
        && actual.origin == expected.origin
        && actual.owner_pid == expected.owner_pid
        && actual.owner_boot_id == expected.owner_boot_id
        && actual.live_generation == expected.live_generation
}

#[derive(Eq, PartialEq)]
enum Target {
    Paused {
        owner_pid: u32,
        owner_boot_id: String,
    },
    Terminal,
    Absent,
    Incompatible,
}

fn inspect_target(conn: &Connection, intent: &SessionDisposition) -> Result<Target> {
    let task = conn
        .query_row(
            "SELECT t.id, t.execution_epoch, t.status, t.owner_pid, t.owner_boot_id,
                    g.pending_call_id IS NULL AND g.pending_call_epoch IS NULL,
                    g.accounting_state
               FROM tasks t LEFT JOIN goal_tasks g ON g.task_id = t.id
              WHERE t.kind = 'goal' AND t.session_id = ?1",
            params![intent.session_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, u32>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, bool>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            },
        )
        .optional()
        .context("check exact disposition Goal binding")?;
    let Some((id, epoch, status, pid, boot_id, settled, accounting)) = task else {
        let id_still_exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM tasks WHERE id = ?1)",
                params![intent.task_id],
                |row| row.get(0),
            )
            .context("rule out wrong disposition task binding")?;
        return Ok(if id_still_exists {
            Target::Incompatible
        } else {
            Target::Absent
        });
    };
    if id != intent.task_id || epoch != intent.execution_epoch || !settled {
        return Ok(Target::Incompatible);
    }
    let status = super::status_from_db(&status)?;
    Ok(if status.is_terminal() {
        Target::Terminal
    } else if status == TaskStatus::Paused && accounting.as_deref() == Some("complete") {
        Target::Paused {
            owner_pid: pid,
            owner_boot_id: boot_id,
        }
    } else {
        Target::Incompatible
    })
}

fn delete_intent(conn: &Connection, intent: &SessionDisposition) -> Result<()> {
    let changed = conn
        .execute(
            "DELETE FROM goal_session_dispositions
          WHERE session_id = ?1 AND operation_id = ?2",
            params![intent.session_id, intent.operation_id],
        )
        .context("clear exact Goal session disposition")?;
    ensure!(
        changed == 1,
        "session disposition changed inside its writer transaction"
    );
    Ok(())
}

fn mark_committed(conn: &Connection, intent: &SessionDisposition) -> Result<()> {
    let changed = conn
        .execute(
            "UPDATE goal_session_dispositions SET phase = 'session_committed'
          WHERE session_id = ?1 AND operation_id = ?2 AND phase = 'prepared'",
            params![intent.session_id, intent.operation_id],
        )
        .context("mark exact Goal session effect committed")?;
    ensure!(
        changed == 1,
        "session disposition changed inside its writer transaction"
    );
    Ok(())
}

fn finalize_exact(conn: &Connection, actual: &SessionDisposition, target: &Target) -> Result<()> {
    ensure!(
        *target != Target::Incompatible,
        "cannot finalize an incompatible Goal session disposition target"
    );
    let mut terminal_epoch = actual.execution_epoch;
    if matches!(target, Target::Paused { .. }) {
        terminal_epoch = terminal_epoch.saturating_add(1);
        let changed = conn
            .execute(
                "UPDATE tasks SET status = 'cancelled', execution_epoch = ?4,
                        finished_at = ?5, error = NULL
                  WHERE id = ?1 AND session_id = ?2 AND execution_epoch = ?3
                    AND kind = 'goal' AND status = 'paused'",
                params![
                    actual.task_id,
                    actual.session_id,
                    actual.execution_epoch,
                    terminal_epoch,
                    chrono::Utc::now().to_rfc3339()
                ],
            )
            .context("cancel exact paused Goal after session commit")?;
        ensure!(
            changed == 1,
            "paused Goal changed inside disposition writer transaction"
        );
    }
    if matches!(target, Target::Paused { .. } | Target::Terminal) {
        let changed = conn
            .execute(
                "DELETE FROM tasks
                  WHERE id = ?1 AND session_id = ?2 AND execution_epoch = ?3 AND kind = 'goal'
                    AND status IN ('completed','failed','cancelled','lost','timed_out')
                    AND NOT EXISTS (
                         SELECT 1 FROM goal_tasks WHERE task_id = tasks.id
                          AND (pending_call_id IS NOT NULL OR pending_call_epoch IS NOT NULL)
                    )",
                params![actual.task_id, actual.session_id, terminal_epoch],
            )
            .context("delete exact terminal Goal after session commit")?;
        ensure!(
            changed == 1,
            "terminal Goal changed inside disposition writer transaction"
        );
        goal::delete_settled_start_intents(conn, &actual.session_id)?;
    }
    delete_intent(conn, actual)
}

impl SqliteTaskStore {
    pub(crate) fn session_disposition(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionDisposition>> {
        read_disposition(&self.conn.lock(), session_id)
    }

    /// Decode every row conservatively. Unreadable operations are not absent.
    pub(crate) fn list_session_dispositions(&self) -> Result<Vec<SessionDisposition>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT * FROM goal_session_dispositions ORDER BY session_id")
            .context("prepare Goal session disposition listing")?;
        let rows = stmt
            .query_map([], row_to_disposition)
            .context("list Goal session dispositions")?;
        rows.collect::<rusqlite::Result<_>>()
            .context("decode Goal session dispositions")
    }

    pub(crate) fn prepare_session_disposition(
        &self,
        intent: &SessionDisposition,
    ) -> Result<DispositionResult> {
        validate_binding(intent)?;
        ensure!(
            intent.phase == SessionDispositionPhase::Prepared,
            "new intent must be prepared"
        );
        let (backend, backend_key, instance_id) = match &intent.origin {
            SessionDispositionOrigin::Live => (None, None, None),
            SessionDispositionOrigin::Durable {
                backend_key,
                identity,
            } => {
                let backend = match identity.backend {
                    SessionDispositionBackend::ChatSqlite => "chat_sqlite",
                    SessionDispositionBackend::ChatJsonl => "chat_jsonl",
                    SessionDispositionBackend::AcpSqlite => "acp_sqlite",
                };
                (
                    Some(backend),
                    Some(backend_key),
                    Some(&identity.instance_id),
                )
            }
        };
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("begin Goal session disposition preparation")?;
        if let Some(actual) = read_disposition(&tx, &intent.session_id)? {
            return Ok(
                if same_operation(&actual, intent)
                    && actual.phase == intent.phase
                    && matches!(
                        inspect_target(&tx, intent)?,
                        Target::Paused { .. } | Target::Terminal
                    )
                {
                    DispositionResult::AlreadySettled
                } else {
                    DispositionResult::Conflict
                },
            );
        }
        // A foreign live Goal owner cannot hand off its paused epoch. Once
        // prepared, writers cannot change the exact epoch and recovery guards
        // the operation's own effect actor, not a possibly reused Goal PID.
        match inspect_target(&tx, intent)? {
            Target::Terminal => {}
            Target::Paused {
                owner_pid,
                owner_boot_id,
            } if (owner_pid == intent.owner_pid && owner_boot_id == intent.owner_boot_id)
                || is_authoritative_owner(owner_pid, &owner_boot_id) => {}
            _ => return Ok(DispositionResult::Conflict),
        }
        tx.execute(
            "INSERT INTO goal_session_dispositions
                 (session_id, operation_id, task_id, execution_epoch, kind, backend, backend_key,
                  backend_instance, owner_pid, owner_boot_id, live_generation, phase)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'prepared')",
            params![
                intent.session_id,
                intent.operation_id,
                intent.task_id,
                intent.execution_epoch,
                intent.kind.as_str(),
                backend,
                backend_key,
                instance_id,
                intent.owner_pid,
                intent.owner_boot_id,
                intent.live_generation
            ],
        )
        .context("persist prepared Goal session disposition")?;
        tx.commit()
            .context("commit prepared Goal session disposition")?;
        Ok(DispositionResult::Applied)
    }

    /// Abort only after the runtime has determined that the effect did not
    /// commit. A foreign live owner must settle its own operation.
    pub(crate) fn abort_session_disposition(
        &self,
        expected: &SessionDisposition,
        owner_pid: u32,
        owner_boot_id: &str,
    ) -> Result<DispositionResult> {
        if !authorize_owner(expected, owner_pid, owner_boot_id) {
            return Ok(DispositionResult::Conflict);
        }
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("begin Goal session disposition abort")?;
        let Some(actual) = read_disposition(&tx, &expected.session_id)? else {
            return Ok(
                if expected.phase == SessionDispositionPhase::Prepared
                    && matches!(
                        inspect_target(&tx, expected)?,
                        Target::Paused { .. } | Target::Terminal
                    )
                {
                    DispositionResult::AlreadySettled
                } else {
                    DispositionResult::Conflict
                },
            );
        };
        if !same_operation(&actual, expected)
            || actual.phase != SessionDispositionPhase::Prepared
            || !matches!(
                inspect_target(&tx, expected)?,
                Target::Paused { .. } | Target::Terminal
            )
        {
            return Ok(DispositionResult::Conflict);
        }
        delete_intent(&tx, &actual)?;
        tx.commit()
            .context("commit Goal session disposition abort")?;
        Ok(DispositionResult::Applied)
    }

    /// Promote durable-backend work only after the runtime validates its exact
    /// positive receipt from the backend instance named by the intent.
    pub(crate) fn confirm_session_disposition(
        &self,
        expected: &SessionDisposition,
        owner_pid: u32,
        owner_boot_id: &str,
    ) -> Result<DispositionResult> {
        if !matches!(expected.origin, SessionDispositionOrigin::Durable { .. })
            || !authorize_owner(expected, owner_pid, owner_boot_id)
        {
            return Ok(DispositionResult::Conflict);
        }
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("begin durable Goal session disposition confirmation")?;
        let Some(actual) = read_disposition(&tx, &expected.session_id)? else {
            return Ok(DispositionResult::Conflict);
        };
        if !same_operation(&actual, expected)
            || inspect_target(&tx, expected)? == Target::Incompatible
        {
            return Ok(DispositionResult::Conflict);
        }
        if actual.phase == SessionDispositionPhase::SessionCommitted {
            return Ok(DispositionResult::AlreadySettled);
        }
        mark_committed(&tx, &actual)?;
        tx.commit()
            .context("commit durable Goal session disposition confirmation")?;
        Ok(DispositionResult::Applied)
    }

    /// Commit a live disposition while the caller holds the live session map,
    /// after every final predicate and lease. The caller must hold no
    /// task-store connection. Waiting for the SQLite writer here is
    /// intentional and bounded by the existing busy timeout.
    pub(crate) fn commit_live_session_disposition(
        &self,
        expected: &SessionDisposition,
    ) -> Result<DispositionResult> {
        if expected.origin != SessionDispositionOrigin::Live
            || expected.owner_pid != std::process::id()
            || expected.phase != SessionDispositionPhase::Prepared
        {
            return Ok(DispositionResult::Conflict);
        }
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("begin live Goal session disposition commit")?;
        let Some(actual) = read_disposition(&tx, &expected.session_id)? else {
            return Ok(DispositionResult::Conflict);
        };
        if !same_operation(&actual, expected) || actual.phase != SessionDispositionPhase::Prepared {
            return Ok(DispositionResult::Conflict);
        }
        let target = inspect_target(&tx, &actual)?;
        if !matches!(target, Target::Paused { .. } | Target::Terminal) {
            return Ok(DispositionResult::Conflict);
        }
        finalize_exact(&tx, &actual, &target)?;
        tx.commit()
            .context("commit live Goal session disposition")?;
        Ok(DispositionResult::Applied)
    }

    /// Clear a committed disposition and the exact Goal together. Neither a
    /// still-open or unsettled Goal nor a successor binding is deletable here.
    pub(crate) fn finalize_session_disposition(
        &self,
        expected: &SessionDisposition,
        owner_pid: u32,
        owner_boot_id: &str,
    ) -> Result<DispositionResult> {
        if !authorize_owner(expected, owner_pid, owner_boot_id) {
            return Ok(DispositionResult::Conflict);
        }
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("begin exact Goal session disposition cleanup")?;
        let Some(actual) = read_disposition(&tx, &expected.session_id)? else {
            return Ok(
                if expected.phase == SessionDispositionPhase::SessionCommitted
                    && inspect_target(&tx, expected)? == Target::Absent
                {
                    DispositionResult::AlreadySettled
                } else {
                    DispositionResult::Conflict
                },
            );
        };
        if !same_operation(&actual, expected)
            || actual.phase != SessionDispositionPhase::SessionCommitted
        {
            return Ok(DispositionResult::Conflict);
        }
        let target = inspect_target(&tx, &actual)?;
        if target == Target::Incompatible {
            return Ok(DispositionResult::Conflict);
        }
        finalize_exact(&tx, &actual, &target)?;
        tx.commit()
            .context("commit exact Goal session disposition cleanup")?;
        Ok(DispositionResult::Applied)
    }
}

#[cfg(test)]
mod tests;
