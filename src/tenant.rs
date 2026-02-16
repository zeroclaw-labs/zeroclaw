use crate::aria::db::AriaDb;

/// Resolve tenant from a bearer token string.
///
/// Rules:
/// - Empty token => `dev-tenant`
/// - `tenant:rest` => `tenant`
/// - `zc_*` token => most active non-`zc_*` tenant in registry DB (fallback `dev-tenant`)
/// - Otherwise => token itself
pub fn resolve_tenant_from_token(db: &AriaDb, token: &str) -> String {
    if token.is_empty() {
        return "dev-tenant".to_string();
    }

    if let Some((tenant, _)) = token.split_once(':') {
        return tenant.to_string();
    }

    if token.starts_with("zc_") {
        if let Some(primary) = detect_primary_tenant(db) {
            return primary;
        }
        return "dev-tenant".to_string();
    }

    token.to_string()
}

/// Infer the primary tenant from most-populated registry rows.
pub fn detect_primary_tenant(db: &AriaDb) -> Option<String> {
    db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            r#"
            SELECT tenant_id, score
            FROM (
              SELECT tenant_id, COUNT(*) AS score FROM aria_tools GROUP BY tenant_id
              UNION ALL
              SELECT tenant_id, COUNT(*) AS score FROM aria_agents GROUP BY tenant_id
              UNION ALL
              SELECT tenant_id, COUNT(*) AS score FROM aria_feeds GROUP BY tenant_id
              UNION ALL
              SELECT tenant_id, COUNT(*) AS score FROM aria_tasks GROUP BY tenant_id
            )
            WHERE tenant_id NOT LIKE 'zc_%'
            ORDER BY score DESC, tenant_id ASC
            LIMIT 1
            "#,
        )?;

        let tenant = stmt.query_row([], |row| row.get::<_, String>(0)).ok();
        Ok(tenant)
    })
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_empty_to_dev_tenant() {
        let db = AriaDb::open_in_memory().unwrap();
        assert_eq!(resolve_tenant_from_token(&db, ""), "dev-tenant");
    }

    #[test]
    fn resolves_prefixed_tenant() {
        let db = AriaDb::open_in_memory().unwrap();
        assert_eq!(resolve_tenant_from_token(&db, "acme:abc"), "acme");
    }

    #[test]
    fn resolves_raw_token_as_tenant() {
        let db = AriaDb::open_in_memory().unwrap();
        assert_eq!(resolve_tenant_from_token(&db, "my-tenant"), "my-tenant");
    }
}
