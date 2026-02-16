use crate::aria::db::AriaDb;
use crate::config::schema::registry_db_path_for_workspace;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
struct RuntimeCronState {
    next_run_ms: Option<i64>,
    last_run_ms: Option<i64>,
    last_status: Option<String>,
}

fn parse_rfc3339_ms(raw: Option<&str>) -> Option<i64> {
    raw.and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc).timestamp_millis())
}

pub fn jobs_file_path() -> PathBuf {
    if let Ok(raw) = std::env::var("ARIA_JOBS_FILE_PATH") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    crate::config::schema::aria_home_dir().join("jobs.json")
}

fn read_runtime_states(workspace_dir: &Path) -> Result<HashMap<String, RuntimeCronState>> {
    let mut states = HashMap::new();
    let db_path = workspace_dir.join("cron").join("jobs.db");
    if !db_path.exists() {
        return Ok(states);
    }

    let conn = Connection::open(&db_path)
        .with_context(|| format!("Failed to open runtime cron DB: {}", db_path.display()))?;
    let mut stmt = conn.prepare(
        "SELECT id, next_run, last_run, last_status
         FROM cron_jobs",
    )?;
    let rows = stmt.query_map([], |row| {
        let id: String = row.get(0)?;
        let next_run: Option<String> = row.get(1)?;
        let last_run: Option<String> = row.get(2)?;
        let last_status: Option<String> = row.get(3)?;
        Ok((id, next_run, last_run, last_status))
    })?;

    for row in rows {
        let (id, next_run, last_run, last_status) = row?;
        states.insert(
            id,
            RuntimeCronState {
                next_run_ms: parse_rfc3339_ms(next_run.as_deref()),
                last_run_ms: parse_rfc3339_ms(last_run.as_deref()),
                last_status,
            },
        );
    }

    Ok(states)
}

fn write_json_atomic(path: &Path, payload: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!("Failed to create jobs file directory: {}", parent.display())
        })?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, payload)
        .with_context(|| format!("Failed to write temporary jobs file: {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("Failed to rename jobs file into place: {}", path.display()))?;
    Ok(())
}

pub fn export_jobs_file(workspace_dir: &Path) -> Result<usize> {
    let db = AriaDb::open(&registry_db_path_for_workspace(workspace_dir))?;
    let runtime_states = read_runtime_states(workspace_dir)?;

    let rows = db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, tenant_id, name, description, schedule_kind, schedule_data,
                    session_target, wake_mode, payload_kind, payload_data, isolation,
                    enabled, status, created_at, updated_at, cron_job_id
             FROM aria_cron_functions
             ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, Option<String>>(10)?,
                row.get::<_, i64>(11)?,
                row.get::<_, String>(12)?,
                row.get::<_, String>(13)?,
                row.get::<_, String>(14)?,
                row.get::<_, Option<String>>(15)?,
            ))
        })?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })?;

    let mut jobs = Vec::with_capacity(rows.len());
    for (
        id,
        tenant_id,
        name,
        description,
        schedule_kind,
        schedule_data,
        session_target,
        wake_mode,
        payload_kind,
        payload_data,
        isolation,
        enabled,
        status,
        created_at,
        updated_at,
        cron_job_id,
    ) in rows
    {
        let schedule_data_json: Value =
            serde_json::from_str(&schedule_data).unwrap_or_else(|_| json!({}));
        let schedule = match schedule_kind.as_str() {
            "cron" => json!({
                "kind": "cron",
                "expr": schedule_data_json.get("expr").and_then(Value::as_str).unwrap_or("* * * * *"),
                "tz": schedule_data_json.get("tz").and_then(Value::as_str),
            }),
            "every" => json!({
                "kind": "every",
                "everyMs": schedule_data_json.get("every_ms").and_then(Value::as_i64).unwrap_or(60000),
            }),
            "at" => json!({
                "kind": "at",
                "atMs": schedule_data_json.get("at_ms").and_then(Value::as_i64).unwrap_or(0),
            }),
            _ => json!({"kind": "cron", "expr": "* * * * *"}),
        };

        let mut payload: Value = serde_json::from_str(&payload_data).unwrap_or_else(|_| json!({}));
        if payload.get("kind").is_none() {
            payload["kind"] = Value::String(payload_kind);
        }

        let isolation_json = isolation
            .as_deref()
            .and_then(|v| serde_json::from_str::<Value>(v).ok());

        let created_at_ms = parse_rfc3339_ms(Some(&created_at)).unwrap_or_default();
        let updated_at_ms = parse_rfc3339_ms(Some(&updated_at)).unwrap_or_default();
        let runtime = cron_job_id
            .as_ref()
            .and_then(|jid| runtime_states.get(jid).cloned());

        jobs.push(json!({
            "id": id,
            "tenantId": tenant_id,
            "name": name,
            "description": description,
            "enabled": enabled != 0,
            "status": status,
            "createdAtMs": created_at_ms,
            "updatedAtMs": updated_at_ms,
            "schedule": schedule,
            "sessionTarget": session_target,
            "wakeMode": wake_mode,
            "payload": payload,
            "isolation": isolation_json,
            "state": {
                "nextRunAtMs": runtime.as_ref().and_then(|s| s.next_run_ms),
                "lastRunAtMs": runtime.as_ref().and_then(|s| s.last_run_ms),
                "lastStatus": runtime.and_then(|s| s.last_status),
            }
        }));
    }

    let payload = json!({
        "version": 1,
        "generatedAt": Utc::now().to_rfc3339(),
        "jobs": jobs,
    });
    let content = serde_json::to_string_pretty(&payload)?;
    write_json_atomic(&jobs_file_path(), &content)?;
    Ok(payload["jobs"].as_array().map_or(0, Vec::len))
}

fn boolish(v: &Value, default: bool) -> bool {
    if let Some(b) = v.as_bool() {
        b
    } else if let Some(n) = v.as_i64() {
        n != 0
    } else {
        default
    }
}

pub fn import_jobs_file(workspace_dir: &Path) -> Result<usize> {
    let path = jobs_file_path();
    if !path.exists() {
        return Ok(0);
    }
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read jobs file: {}", path.display()))?;
    let root: Value = serde_json::from_str(&raw)
        .with_context(|| format!("Invalid jobs JSON: {}", path.display()))?;
    let jobs = root
        .get("jobs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let db = AriaDb::open(&registry_db_path_for_workspace(workspace_dir))?;
    let now = Utc::now().to_rfc3339();
    let mut imported = 0usize;

    db.with_conn(|conn| {
        for job in &jobs {
            let id = job
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let tenant_id = job
                .get("tenantId")
                .and_then(Value::as_str)
                .unwrap_or("dev-tenant");
            let name = job.get("name").and_then(Value::as_str).unwrap_or("cron-job");
            let description = job
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let enabled = boolish(job.get("enabled").unwrap_or(&Value::Bool(true)), true);
            let status = if enabled { "active" } else { "paused" };

            let schedule = job.get("schedule").cloned().unwrap_or_else(|| json!({}));
            let schedule_kind = schedule
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("cron");
            let schedule_data = match schedule_kind {
                "cron" => json!({
                    "expr": schedule.get("expr").and_then(Value::as_str).unwrap_or("* * * * *"),
                    "tz": schedule.get("tz").and_then(Value::as_str),
                }),
                "every" => json!({
                    "every_ms": schedule.get("everyMs").and_then(Value::as_i64).unwrap_or(60000),
                }),
                "at" => json!({
                    "at_ms": schedule.get("atMs").and_then(Value::as_i64).unwrap_or(0),
                }),
                _ => json!({"expr":"* * * * *"}),
            };

            let session_target = job
                .get("sessionTarget")
                .and_then(Value::as_str)
                .unwrap_or("main");
            let wake_mode = job
                .get("wakeMode")
                .and_then(Value::as_str)
                .unwrap_or("next-heartbeat");

            let payload = job.get("payload").cloned().unwrap_or_else(|| json!({}));
            let payload_kind = payload
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("systemEvent");
            let mut payload_data = payload.clone();
            if let Some(obj) = payload_data.as_object_mut() {
                obj.remove("kind");
            }
            let isolation = job.get("isolation").cloned();

            let changed = conn.execute(
                "UPDATE aria_cron_functions
                 SET tenant_id = ?1, name = ?2, description = ?3,
                     schedule_kind = ?4, schedule_data = ?5, session_target = ?6,
                     wake_mode = ?7, payload_kind = ?8, payload_data = ?9,
                     isolation = ?10, enabled = ?11, status = ?12, cron_job_id = NULL,
                     updated_at = ?13
                 WHERE id = ?14",
                params![
                    tenant_id,
                    name,
                    description,
                    schedule_kind,
                    serde_json::to_string(&schedule_data)?,
                    session_target,
                    wake_mode,
                    payload_kind,
                    serde_json::to_string(&payload_data)?,
                    isolation.as_ref().map(serde_json::to_string).transpose()?,
                    i64::from(enabled),
                    status,
                    now,
                    id,
                ],
            )?;

            if changed == 0 {
                conn.execute(
                    "INSERT INTO aria_cron_functions
                     (id, tenant_id, name, description, schedule_kind, schedule_data,
                      session_target, wake_mode, payload_kind, payload_data, isolation,
                      enabled, delete_after_run, cron_job_id, status, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, NULL, ?14, ?15, ?15)",
                    params![
                        id,
                        tenant_id,
                        name,
                        description,
                        schedule_kind,
                        serde_json::to_string(&schedule_data)?,
                        session_target,
                        wake_mode,
                        payload_kind,
                        serde_json::to_string(&payload_data)?,
                        isolation.as_ref().map(serde_json::to_string).transpose()?,
                        i64::from(enabled),
                        i64::from(schedule_kind == "at"),
                        status,
                        now,
                    ],
                )?;
            }
            imported += 1;
        }
        Ok(())
    })?;

    Ok(imported)
}
