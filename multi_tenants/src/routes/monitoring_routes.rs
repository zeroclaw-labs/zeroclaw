use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::auth::middleware::AuthUser;
use crate::error::AppError;
use crate::rbac::{self, Role};
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct DashboardResponse {
    pub total_tenants: i64,
    pub running_tenants: i64,
    pub stopped_tenants: i64,
    pub error_tenants: i64,
    pub total_users: i64,
    pub total_channels: i64,
}

#[derive(Serialize)]
pub struct TenantHealthResponse {
    pub tenant_id: String,
    pub slug: String,
    pub status: String,
    pub port: u16,
}

#[derive(Serialize)]
pub struct UsageResponse {
    pub tenant_id: String,
    pub period: String,
    pub messages: i64,
    pub tokens_in: i64,
    pub tokens_out: i64,
}

#[derive(Serialize)]
pub struct AuditEntry {
    pub id: String,
    pub actor_id: Option<String>,
    pub action: String,
    pub resource: String,
    pub resource_id: Option<String>,
    pub details: Option<String>,
    pub created_at: String,
}

#[derive(Serialize)]
pub struct AuditPageResponse {
    pub entries: Vec<AuditEntry>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
}

#[derive(Serialize)]
pub struct ResourceSnapshot {
    pub tenant_id: String,
    pub ts: String,
    pub cpu_pct: f64,
    pub mem_bytes: i64,
    pub mem_limit: i64,
    pub disk_bytes: i64,
    pub net_in_bytes: i64,
    pub net_out_bytes: i64,
    pub pids: i64,
}

#[derive(Serialize)]
pub struct TenantResourcesResponse {
    pub current: Option<ResourceSnapshot>,
    pub history: Vec<ResourceSnapshot>,
}

#[derive(Serialize)]
pub struct ResourceSummaryEntry {
    pub tenant_id: String,
    pub slug: String,
    pub name: String,
    pub status: String,
    pub cpu_pct: f64,
    pub mem_bytes: i64,
    pub mem_limit: i64,
    pub disk_bytes: i64,
    pub pids: i64,
    pub ts: String,
}

// ---------------------------------------------------------------------------
// Query param types
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct ResourceQuery {
    pub range: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct UsageQuery {
    pub tenant_id: Option<String>,
    pub days: Option<u32>,
}

#[derive(Deserialize, Default)]
pub struct AuditQuery {
    pub page: Option<u32>,
    pub per_page: Option<u32>,
    pub action: Option<String>,
    pub actor_id: Option<String>,
    pub since: Option<String>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /monitoring/dashboard — super_admin only.
/// Returns aggregated counts across all tenants, users, and channels.
pub async fn dashboard(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
) -> Result<Json<DashboardResponse>, AppError> {
    rbac::require_super_admin(&user)?;

    let response = state.db.read(|conn| {
        let total_tenants: i64 =
            conn.query_row("SELECT COUNT(*) FROM tenants", [], |r| r.get(0))?;
        let running_tenants: i64 = conn.query_row(
            "SELECT COUNT(*) FROM tenants WHERE status = 'running'",
            [],
            |r| r.get(0),
        )?;
        let stopped_tenants: i64 = conn.query_row(
            "SELECT COUNT(*) FROM tenants WHERE status = 'stopped'",
            [],
            |r| r.get(0),
        )?;
        let error_tenants: i64 = conn.query_row(
            "SELECT COUNT(*) FROM tenants WHERE status = 'error'",
            [],
            |r| r.get(0),
        )?;
        let total_users: i64 = conn.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?;
        let total_channels: i64 =
            conn.query_row("SELECT COUNT(*) FROM channels", [], |r| r.get(0))?;

        Ok(DashboardResponse {
            total_tenants,
            running_tenants,
            stopped_tenants,
            error_tenants,
            total_users,
            total_channels,
        })
    })?;

    Ok(Json(response))
}

/// GET /monitoring/health — super_admin only.
/// Returns health status for every tenant.
pub async fn health(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
) -> Result<Json<Vec<TenantHealthResponse>>, AppError> {
    rbac::require_super_admin(&user)?;

    let rows = state.db.read(|conn| {
        let mut stmt =
            conn.prepare("SELECT id, slug, status, port FROM tenants ORDER BY slug ASC")?;
        let items = stmt
            .query_map([], |row| {
                Ok(TenantHealthResponse {
                    tenant_id: row.get(0)?,
                    slug: row.get(1)?,
                    status: row.get(2)?,
                    port: row.get::<_, i64>(3)? as u16,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(items)
    })?;

    Ok(Json(rows))
}

/// GET /monitoring/usage — super_admin sees all; tenant Viewer sees own only.
/// Query params: tenant_id (optional), days (default 30, max 90).
pub async fn usage(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Query(query): Query<UsageQuery>,
) -> Result<Json<Vec<UsageResponse>>, AppError> {
    let days = query.days.unwrap_or(30).min(90);
    let since = chrono::Utc::now() - chrono::Duration::days(days as i64);
    let since_str = since.format("%Y-%m-%dT%H:%M:%SZ").to_string();

    // Determine which tenant(s) to fetch.
    let effective_tenant_id: Option<String> = if user.is_super_admin {
        // Super-admin can filter by any tenant_id or see all.
        query.tenant_id.clone()
    } else {
        // Non-super-admin: must specify a tenant_id and must hold at least Viewer role.
        let tid = query
            .tenant_id
            .clone()
            .ok_or_else(|| AppError::Forbidden("tenant_id required for non-super-admin".into()))?;
        rbac::require_tenant_role(&user, &tid, Role::Viewer, &state.db)?;
        Some(tid)
    };

    let rows = state.db.read(|conn| {
        match effective_tenant_id {
            Some(ref tid) => {
                let mut stmt = conn.prepare(
                    "SELECT tenant_id, period, messages, tokens_in, tokens_out
                     FROM usage_metrics
                     WHERE tenant_id = ?1 AND period >= ?2
                     ORDER BY period DESC",
                )?;
                let items = stmt
                    .query_map(rusqlite::params![tid, since_str], |row| {
                        Ok(UsageResponse {
                            tenant_id: row.get(0)?,
                            period: row.get(1)?,
                            messages: row.get(2)?,
                            tokens_in: row.get(3)?,
                            tokens_out: row.get(4)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(items)
            }
            None => {
                // Super-admin, no tenant filter.
                let mut stmt = conn.prepare(
                    "SELECT tenant_id, period, messages, tokens_in, tokens_out
                     FROM usage_metrics
                     WHERE period >= ?1
                     ORDER BY period DESC",
                )?;
                let items = stmt
                    .query_map(rusqlite::params![since_str], |row| {
                        Ok(UsageResponse {
                            tenant_id: row.get(0)?,
                            period: row.get(1)?,
                            messages: row.get(2)?,
                            tokens_in: row.get(3)?,
                            tokens_out: row.get(4)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(items)
            }
        }
    })?;

    Ok(Json(rows))
}

/// GET /monitoring/audit — super_admin only.
/// Query params: page (default 1), per_page (default 50, max 200), action, actor_id, since (optional filters).
pub async fn audit(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Query(query): Query<AuditQuery>,
) -> Result<Json<AuditPageResponse>, AppError> {
    rbac::require_super_admin(&user)?;

    let per_page = query.per_page.unwrap_or(50).min(200) as i64;
    let page = query.page.unwrap_or(1).max(1) as i64;
    let offset = (page - 1) * per_page;

    // Build WHERE clause from optional filters.
    // We track the SQL param index and collect string values separately
    // so we can re-use them for both the COUNT and data queries.
    let mut conditions: Vec<String> = Vec::new();
    let mut filter_values: Vec<String> = Vec::new();
    let mut idx = 1usize;

    if let Some(ref v) = query.action {
        conditions.push(format!("action = ?{}", idx));
        filter_values.push(v.clone());
        idx += 1;
    }
    if let Some(ref v) = query.actor_id {
        conditions.push(format!("actor_id = ?{}", idx));
        filter_values.push(v.clone());
        idx += 1;
    }
    if let Some(ref v) = query.since {
        conditions.push(format!("created_at >= ?{}", idx));
        filter_values.push(v.clone());
        idx += 1;
    }

    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conditions.join(" AND "))
    };

    // Pagination params are appended after the filter params
    let limit_idx = idx;
    let offset_idx = idx + 1;

    let count_sql = format!("SELECT COUNT(*) FROM audit_log {}", where_clause);
    let data_sql = format!(
        "SELECT id, actor_id, action, resource, resource_id, details, created_at
         FROM audit_log {}
         ORDER BY created_at DESC
         LIMIT ?{} OFFSET ?{}",
        where_clause, limit_idx, offset_idx
    );

    let (total, entries) = state.db.read(|conn| {
        // COUNT query — filter params only
        let count_refs: Vec<&dyn rusqlite::ToSql> = filter_values
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();
        let total: i64 = conn.query_row(&count_sql, count_refs.as_slice(), |r| r.get(0))?;

        // Data query — filter params + pagination
        let mut data_params: Vec<Box<dyn rusqlite::ToSql>> = filter_values
            .iter()
            .map(|s| Box::new(s.clone()) as Box<dyn rusqlite::ToSql>)
            .collect();
        data_params.push(Box::new(per_page));
        data_params.push(Box::new(offset));
        let data_refs: Vec<&dyn rusqlite::ToSql> = data_params.iter().map(|p| p.as_ref()).collect();

        let mut stmt = conn.prepare(&data_sql)?;
        let items = stmt
            .query_map(data_refs.as_slice(), map_audit_row)?
            .collect::<Result<Vec<_>, _>>()?;

        Ok((total, items))
    })?;

    Ok(Json(AuditPageResponse {
        entries,
        total,
        page,
        per_page,
    }))
}

/// GET /tenants/{id}/resources?range=1h|6h|24h|7d — Viewer role
pub async fn tenant_resources(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(tenant_id): Path<String>,
    Query(query): Query<ResourceQuery>,
) -> Result<Json<TenantResourcesResponse>, AppError> {
    rbac::require_tenant_role(&user, &tenant_id, Role::Viewer, &state.db)?;

    let range = query.range.as_deref().unwrap_or("1h");
    let interval = match range {
        "6h" => "-6 hours",
        "24h" => "-1 day",
        "7d" => "-7 days",
        _ => "-1 hour", // default 1h
    };

    let (current, history) = state.db.read(|conn| {
        // Latest snapshot
        let current = conn
            .query_row(
                "SELECT tenant_id, ts, cpu_pct, mem_bytes, mem_limit, disk_bytes, \
                 net_in_bytes, net_out_bytes, pids \
                 FROM resource_snapshots WHERE tenant_id = ?1 ORDER BY ts DESC LIMIT 1",
                [&tenant_id],
                map_resource_row,
            )
            .ok();

        // Historical snapshots
        let mut stmt = conn.prepare(
            "SELECT tenant_id, ts, cpu_pct, mem_bytes, mem_limit, disk_bytes, \
             net_in_bytes, net_out_bytes, pids \
             FROM resource_snapshots \
             WHERE tenant_id = ?1 AND ts >= datetime('now', ?2) \
             ORDER BY ts ASC",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![tenant_id, interval], map_resource_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok((current, rows))
    })?;

    Ok(Json(TenantResourcesResponse { current, history }))
}

/// GET /monitoring/resources — super_admin only, latest snapshot per tenant
pub async fn admin_resources(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
) -> Result<Json<Vec<ResourceSummaryEntry>>, AppError> {
    rbac::require_super_admin(&user)?;

    let entries = state.db.read(|conn| {
        let mut stmt = conn.prepare(
            "SELECT t.id, t.slug, t.name, t.status, \
             rs.cpu_pct, rs.mem_bytes, rs.mem_limit, rs.disk_bytes, rs.pids, rs.ts \
             FROM tenants t \
             LEFT JOIN resource_snapshots rs ON rs.tenant_id = t.id \
             AND rs.ts = (SELECT MAX(ts) FROM resource_snapshots WHERE tenant_id = t.id) \
             WHERE t.status IN ('running', 'error', 'starting') \
             ORDER BY t.slug ASC",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ResourceSummaryEntry {
                    tenant_id: row.get(0)?,
                    slug: row.get(1)?,
                    name: row.get(2)?,
                    status: row.get(3)?,
                    cpu_pct: row.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
                    mem_bytes: row.get::<_, Option<i64>>(5)?.unwrap_or(0),
                    mem_limit: row.get::<_, Option<i64>>(6)?.unwrap_or(0),
                    disk_bytes: row.get::<_, Option<i64>>(7)?.unwrap_or(0),
                    pids: row.get::<_, Option<i64>>(8)?.unwrap_or(0),
                    ts: row.get::<_, Option<String>>(9)?.unwrap_or_default(),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;

    Ok(Json(entries))
}

fn map_resource_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ResourceSnapshot> {
    Ok(ResourceSnapshot {
        tenant_id: row.get(0)?,
        ts: row.get(1)?,
        cpu_pct: row.get(2)?,
        mem_bytes: row.get(3)?,
        mem_limit: row.get(4)?,
        disk_bytes: row.get(5)?,
        net_in_bytes: row.get(6)?,
        net_out_bytes: row.get(7)?,
        pids: row.get(8)?,
    })
}

fn map_audit_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AuditEntry> {
    Ok(AuditEntry {
        id: row.get(0)?,
        actor_id: row.get(1)?,
        action: row.get(2)?,
        resource: row.get(3)?,
        resource_id: row.get(4)?,
        details: row.get(5)?,
        created_at: row.get(6)?,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_usage_query_days_cap() {
        // days > 90 should be capped at 90 in the handler.
        let input: u32 = 200;
        let days = input.min(90);
        assert_eq!(days, 90);
    }

    #[test]
    fn test_audit_query_per_page_cap() {
        let input: u32 = 500;
        let per_page: i64 = input.min(200) as i64;
        assert_eq!(per_page, 200);
    }

    #[test]
    fn test_audit_query_page_minimum() {
        // page=0 should be treated as page=1.
        let input: u32 = 0;
        let page: i64 = input.max(1) as i64;
        assert_eq!(page, 1);
    }

    #[test]
    fn test_dashboard_response_serialization() {
        let resp = DashboardResponse {
            total_tenants: 10,
            running_tenants: 7,
            stopped_tenants: 2,
            error_tenants: 1,
            total_users: 25,
            total_channels: 50,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["total_tenants"], 10);
        assert_eq!(json["running_tenants"], 7);
        assert_eq!(json["error_tenants"], 1);
    }

    #[test]
    fn test_tenant_health_response_serialization() {
        let resp = TenantHealthResponse {
            tenant_id: "tid-1".into(),
            slug: "my-tenant".into(),
            status: "running".into(),
            port: 8080,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["port"], 8080);
        assert_eq!(json["status"], "running");
    }

    #[test]
    fn test_usage_response_serialization() {
        let resp = UsageResponse {
            tenant_id: "tid-1".into(),
            period: "2026-02-20T12:00:00Z".into(),
            messages: 100,
            tokens_in: 500,
            tokens_out: 300,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["messages"], 100);
        assert_eq!(json["tokens_in"], 500);
    }

    #[test]
    fn test_resource_snapshot_serialization() {
        let snap = ResourceSnapshot {
            tenant_id: "tid-1".into(),
            ts: "2026-02-21T12:00:00Z".into(),
            cpu_pct: 1.5,
            mem_bytes: 50_000_000,
            mem_limit: 268_435_456,
            disk_bytes: 1_000_000,
            net_in_bytes: 5000,
            net_out_bytes: 3000,
            pids: 5,
        };
        let json = serde_json::to_value(&snap).unwrap();
        assert_eq!(json["cpu_pct"], 1.5);
        assert_eq!(json["pids"], 5);
    }

    #[test]
    fn test_resource_query_default_range() {
        let q = ResourceQuery { range: None };
        let range = q.range.as_deref().unwrap_or("1h");
        assert_eq!(range, "1h");
    }

    #[test]
    fn test_audit_entry_optional_fields_serialize_null() {
        let entry = AuditEntry {
            id: "id-1".into(),
            actor_id: None,
            action: "tenant_created".into(),
            resource: "tenant".into(),
            resource_id: Some("tid-1".into()),
            details: None,
            created_at: "2026-02-20T00:00:00Z".into(),
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert!(json["actor_id"].is_null());
        assert!(json["details"].is_null());
        assert_eq!(json["action"], "tenant_created");
    }
}
