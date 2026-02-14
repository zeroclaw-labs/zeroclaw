use chrono::Utc;
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

#[derive(Debug, Clone, Serialize)]
pub struct ComponentHealth {
    pub status: String,
    pub updated_at: String,
    pub last_ok: Option<String>,
    pub last_error: Option<String>,
    pub restart_count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthSnapshot {
    pub pid: u32,
    pub updated_at: String,
    pub uptime_seconds: u64,
    pub components: BTreeMap<String, ComponentHealth>,
}

struct HealthRegistry {
    started_at: Instant,
    components: Mutex<BTreeMap<String, ComponentHealth>>,
}

static REGISTRY: OnceLock<HealthRegistry> = OnceLock::new();

fn registry() -> &'static HealthRegistry {
    REGISTRY.get_or_init(|| HealthRegistry {
        started_at: Instant::now(),
        components: Mutex::new(BTreeMap::new()),
    })
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

fn upsert_component<F>(component: &str, update: F)
where
    F: FnOnce(&mut ComponentHealth),
{
    if let Ok(mut map) = registry().components.lock() {
        let now = now_rfc3339();
        let entry = map
            .entry(component.to_string())
            .or_insert_with(|| ComponentHealth {
                status: "starting".into(),
                updated_at: now.clone(),
                last_ok: None,
                last_error: None,
                restart_count: 0,
            });
        update(entry);
        entry.updated_at = now;
    }
}

pub fn mark_component_ok(component: &str) {
    upsert_component(component, |entry| {
        entry.status = "ok".into();
        entry.last_ok = Some(now_rfc3339());
        entry.last_error = None;
    });
}

#[allow(clippy::needless_pass_by_value)]
pub fn mark_component_error(component: &str, error: impl ToString) {
    let err = error.to_string();
    upsert_component(component, move |entry| {
        entry.status = "error".into();
        entry.last_error = Some(err);
    });
}

pub fn bump_component_restart(component: &str) {
    upsert_component(component, |entry| {
        entry.restart_count = entry.restart_count.saturating_add(1);
    });
}

pub fn snapshot() -> HealthSnapshot {
    let components = registry()
        .components
        .lock()
        .map_or_else(|_| BTreeMap::new(), |map| map.clone());

    HealthSnapshot {
        pid: std::process::id(),
        updated_at: now_rfc3339(),
        uptime_seconds: registry().started_at.elapsed().as_secs(),
        components,
    }
}

pub fn snapshot_json() -> serde_json::Value {
    serde_json::to_value(snapshot()).unwrap_or_else(|_| {
        serde_json::json!({
            "status": "error",
            "message": "failed to serialize health snapshot"
        })
    })
}
