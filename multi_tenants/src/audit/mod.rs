use crate::db::pool::DbPool;

/// Log an audit event to the audit_log table.
///
/// Use [`log_with_ip`] when the client IP address is available (e.g. from request extensions).
pub fn log(
    db: &DbPool,
    action: &str,
    resource: &str,
    resource_id: &str,
    actor_id: Option<&str>,
    details: Option<&str>,
) -> anyhow::Result<()> {
    log_with_ip(db, action, resource, resource_id, actor_id, details, None)
}

/// Log an audit event including the client IP address.
pub fn log_with_ip(
    db: &DbPool,
    action: &str,
    resource: &str,
    resource_id: &str,
    actor_id: Option<&str>,
    details: Option<&str>,
    ip_address: Option<&str>,
) -> anyhow::Result<()> {
    let id = uuid::Uuid::new_v4().to_string();
    db.write(|conn| {
        conn.execute(
            "INSERT INTO audit_log (id, actor_id, action, resource, resource_id, details, ip_address)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![id, actor_id, action, resource, resource_id, details, ip_address],
        )?;
        Ok(())
    })
}
