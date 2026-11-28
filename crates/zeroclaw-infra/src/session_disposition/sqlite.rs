//! The two SQLite session owners share the receipt format, not session policy.

use super::{
    SessionDispositionBackend, SessionDispositionEffect, SessionDispositionIdentity,
    SessionDispositionObservation, SessionDispositionWitness,
};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use std::io;
use std::path::Path;

pub(crate) fn initialize(
    tx: &Transaction<'_>,
    backend: SessionDispositionBackend,
) -> io::Result<()> {
    match schema_presence(tx).map_err(io::Error::other)? {
        (true, true) => return identity(tx, backend).map(drop),
        (false, false) => {}
        // Do not rebuild a missing receipt table while retaining an original
        // identity: that would turn lost evidence into false non-commit.
        _ => return Err(indeterminate()),
    }
    tx.execute_batch(
        "CREATE TABLE session_disposition_backend (
             singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
             instance_id TEXT NOT NULL
         );
         INSERT INTO session_disposition_backend (singleton, instance_id)
             VALUES (1, lower(hex(randomblob(16))));
         CREATE TABLE session_disposition_receipts (
             operation_id TEXT PRIMARY KEY NOT NULL,
             effect TEXT NOT NULL CHECK (effect IN ('delete', 'kill')),
             backend_key TEXT NOT NULL CHECK (length(backend_key) > 0)
         );",
    )
    .map_err(io::Error::other)
}

fn schema_presence(conn: &Connection) -> rusqlite::Result<(bool, bool)> {
    conn.query_row(
        "SELECT
             EXISTS(SELECT 1 FROM sqlite_schema
                     WHERE type = 'table' AND name = 'session_disposition_backend'),
             EXISTS(SELECT 1 FROM sqlite_schema
                     WHERE type = 'table' AND name = 'session_disposition_receipts')",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
}

pub(crate) fn identity(
    conn: &Connection,
    backend: SessionDispositionBackend,
) -> io::Result<SessionDispositionIdentity> {
    let instance_id: String = conn
        .query_row(
            "SELECT instance_id FROM session_disposition_backend WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .map_err(io::Error::other)?;
    if instance_id.len() != 32 || !instance_id.bytes().all(is_lower_hex) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid session disposition backend identity",
        ));
    }
    Ok(SessionDispositionIdentity {
        backend,
        instance_id,
    })
}

fn is_lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}

fn valid_key(witness: &SessionDispositionWitness) -> bool {
    let uuid = &witness.operation_id;
    let uuid_valid = uuid.len() == 36
        && uuid.bytes().enumerate().all(|(i, byte)| match i {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => is_lower_hex(byte),
        });
    uuid_valid
        && match witness.identity.backend {
            SessionDispositionBackend::AcpSqlite => witness
                .backend_key
                .parse::<i64>()
                .is_ok_and(|id| id > 0 && id.to_string() == witness.backend_key),
            _ => !witness.backend_key.is_empty(),
        }
}

fn inspect(
    conn: &Connection,
    backend: SessionDispositionBackend,
    witness: &SessionDispositionWitness,
) -> io::Result<SessionDispositionObservation> {
    use SessionDispositionObservation::{Committed, Indeterminate, NotCommitted};
    if witness.identity.backend != backend || !valid_key(witness) {
        return Ok(Indeterminate);
    }
    if schema_presence(conn).map_err(io::Error::other)? != (true, true)
        || identity(conn, backend)? != witness.identity
    {
        return Ok(Indeterminate);
    }
    // Query by the operation alone: a different tuple under the same operation
    // ID is conflicting evidence, not evidence that the effect did not commit.
    let receipt: Option<(String, String)> = conn
        .query_row(
            "SELECT effect, backend_key FROM session_disposition_receipts WHERE operation_id = ?1",
            [&witness.operation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(io::Error::other)?;
    Ok(match receipt {
        None => NotCommitted,
        Some((effect, key)) if effect == witness.effect.as_str() && key == witness.backend_key => {
            Committed
        }
        Some(_) => Indeterminate,
    })
}

pub(crate) fn ensure_new(
    tx: &Transaction<'_>,
    backend: SessionDispositionBackend,
    expected_effect: SessionDispositionEffect,
    witness: &SessionDispositionWitness,
) -> io::Result<()> {
    if witness.effect != expected_effect {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid session disposition witness",
        ));
    }
    match inspect(tx, backend, witness)? {
        SessionDispositionObservation::NotCommitted => Ok(()),
        SessionDispositionObservation::Committed => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "session disposition witness already committed",
        )),
        SessionDispositionObservation::Indeterminate => Err(indeterminate()),
    }
}

pub(crate) fn record(
    tx: &Transaction<'_>,
    witness: &SessionDispositionWitness,
) -> rusqlite::Result<()> {
    let changed = tx.execute(
        "INSERT INTO session_disposition_receipts (operation_id, effect, backend_key)
         VALUES (?1, ?2, ?3)",
        params![
            witness.operation_id,
            witness.effect.as_str(),
            witness.backend_key
        ],
    )?;
    if changed != 1 {
        return Err(rusqlite::Error::StatementChangedRows(changed));
    }
    Ok(())
}

pub(crate) fn observe(
    conn: &mut Connection,
    backend: SessionDispositionBackend,
    witness: &SessionDispositionWitness,
) -> io::Result<SessionDispositionObservation> {
    let tx = conn.transaction().map_err(io::Error::other)?;
    inspect(&tx, backend, witness)
}

pub(crate) fn acknowledge(
    conn: &mut Connection,
    backend: SessionDispositionBackend,
    witness: &SessionDispositionWitness,
) -> io::Result<()> {
    with_full_sync(conn, |conn| {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io::Error::other)?;
        match inspect(&tx, backend, witness)? {
            SessionDispositionObservation::Committed => {
                let changed = tx
                    .execute(
                        "DELETE FROM session_disposition_receipts
                         WHERE operation_id = ?1 AND effect = ?2 AND backend_key = ?3",
                        params![
                            witness.operation_id,
                            witness.effect.as_str(),
                            witness.backend_key
                        ],
                    )
                    .map_err(io::Error::other)?;
                if changed != 1 {
                    return Err(indeterminate());
                }
                tx.commit().map_err(io::Error::other)
            }
            SessionDispositionObservation::NotCommitted => Ok(()),
            SessionDispositionObservation::Indeterminate => Err(indeterminate()),
        }
    })
}

pub(crate) fn observe_at(
    path: &Path,
    backend: SessionDispositionBackend,
    witness: &SessionDispositionWitness,
) -> io::Result<SessionDispositionObservation> {
    if witness.identity.backend != backend || !path.try_exists()? {
        return Ok(SessionDispositionObservation::Indeterminate);
    }
    // Never use a store constructor here: recovery cannot create, migrate, or
    // import into whichever database currently happens to occupy this path.
    let mut conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(io::Error::other)?;
    observe(&mut conn, backend, witness)
}

pub(crate) fn acknowledge_at(
    path: &Path,
    backend: SessionDispositionBackend,
    witness: &SessionDispositionWitness,
) -> io::Result<()> {
    match observe_at(path, backend, witness)? {
        SessionDispositionObservation::Committed => {
            let mut conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
                .map_err(io::Error::other)?;
            // Check identity again inside the write transaction: the file
            // opened for observation need not be the file we opened to write.
            acknowledge(&mut conn, backend, witness)
        }
        SessionDispositionObservation::NotCommitted => Ok(()),
        SessionDispositionObservation::Indeterminate => Err(indeterminate()),
    }
}

// These writes hand authority across independently committing databases.
// Flush the WAL at COMMIT, then restore the connection's ordinary setting.
pub(crate) fn with_full_sync<T, E: From<io::Error>>(
    conn: &mut Connection,
    effect: impl FnOnce(&mut Connection) -> Result<T, E>,
) -> Result<T, E> {
    let prior: i64 = conn
        .pragma_query_value(None, "synchronous", |row| row.get(0))
        .map_err(io::Error::other)?;
    if prior >= 2 {
        return effect(conn);
    }
    conn.pragma_update(None, "synchronous", "FULL")
        .map_err(io::Error::other)?;
    let result = effect(conn);
    let restore = conn
        .pragma_update(None, "synchronous", prior)
        .map_err(io::Error::other);
    // An effect error is primary. A failed restoration leaves FULL enabled.
    result.and_then(|value| restore.map(|()| value).map_err(E::from))
}

fn indeterminate() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "session disposition evidence is indeterminate",
    )
}
