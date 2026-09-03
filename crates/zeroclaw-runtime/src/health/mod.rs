use chrono::Utc;
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::OnceLock;
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
    started_at_wall: chrono::DateTime<chrono::Utc>,
    components: Mutex<BTreeMap<String, ComponentHealth>>,
}

static REGISTRY: OnceLock<HealthRegistry> = OnceLock::new();

fn registry() -> &'static HealthRegistry {
    REGISTRY.get_or_init(|| HealthRegistry {
        started_at: Instant::now(),
        started_at_wall: Utc::now(),
        components: Mutex::new(BTreeMap::new()),
    })
}

/// Daemon start time as RFC 3339 UTC. Stable across the daemon's
/// lifetime so the dashboard can implement "since daemon start"
/// log queries without drift.
pub fn daemon_started_at() -> String {
    registry().started_at_wall.to_rfc3339()
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

fn upsert_component<F>(component: &str, update: F)
where
    F: FnOnce(&mut ComponentHealth),
{
    let mut map = registry().components.lock();
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

pub fn mark_component_ok(component: &str) {
    upsert_component(component, |entry| {
        entry.status = "ok".into();
        entry.last_ok = Some(now_rfc3339());
        entry.last_error = None;
    });
}

/// Register the component as present but not yet proven healthy.
///
/// Distinct from both `ok` and `error`: the component exists and has not
/// failed, but nothing has succeeded *for its current incarnation*. Callers use
/// this when they know a component is running and know they have no evidence it
/// works.
///
/// Two things make this more than a no-op, because the registry only creates an
/// entry when a mutation reaches it:
///
/// - On first call the entry is created, so the component appears in
///   [`snapshot`] as `starting` instead of being absent from `/health`
///   altogether.
/// - On a later call it *invalidates* a `last_ok` left by a previous
///   incarnation. The registry lives in a process-wide `OnceLock` and outlives
///   a daemon reload, so a replacement component reusing the same name would
///   otherwise inherit its predecessor's `ok` and keep reporting healthy while
///   it has never once succeeded.
///
/// `last_error` is deliberately preserved: erasing a recorded failure is how
/// a status becomes misleading, and "starting" is not evidence that a prior
/// error has been resolved. `restart_count` is untouched.
pub fn mark_component_starting(component: &str) {
    upsert_component(component, |entry| {
        entry.status = "starting".into();
        entry.last_ok = None;
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
    let components = registry().components.lock().clone();

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

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_component(prefix: &str) -> String {
        format!("{prefix}-{}", uuid::Uuid::new_v4())
    }

    #[test]
    fn mark_component_ok_initializes_component_state() {
        let component = unique_component("health-ok");

        mark_component_ok(&component);

        let snapshot = snapshot();
        let entry = snapshot
            .components
            .get(&component)
            .expect("component should be present after mark_component_ok");

        assert_eq!(entry.status, "ok");
        assert!(entry.last_ok.is_some());
        assert!(entry.last_error.is_none());
    }

    #[test]
    fn mark_component_error_then_ok_clears_last_error() {
        let component = unique_component("health-error");

        mark_component_error(&component, "first failure");
        let error_snapshot = snapshot();
        let errored = error_snapshot
            .components
            .get(&component)
            .expect("component should exist after mark_component_error");
        assert_eq!(errored.status, "error");
        assert_eq!(errored.last_error.as_deref(), Some("first failure"));

        mark_component_ok(&component);
        let recovered_snapshot = snapshot();
        let recovered = recovered_snapshot
            .components
            .get(&component)
            .expect("component should exist after recovery");
        assert_eq!(recovered.status, "ok");
        assert!(recovered.last_error.is_none());
        assert!(recovered.last_ok.is_some());
    }

    #[test]
    fn bump_component_restart_increments_counter() {
        let component = unique_component("health-restart");

        bump_component_restart(&component);
        bump_component_restart(&component);

        let snapshot = snapshot();
        let entry = snapshot
            .components
            .get(&component)
            .expect("component should exist after restart bump");

        assert_eq!(entry.restart_count, 2);
    }

    #[test]
    fn mark_component_starting_publishes_a_component_that_has_not_succeeded() {
        // The registry only creates an entry when a mutation reaches it, so a
        // component with nothing to report would otherwise be absent from
        // `/health` rather than visibly not-yet-healthy.
        let component = unique_component("health-starting");

        assert!(
            !snapshot().components.contains_key(&component),
            "precondition: the component should not exist yet"
        );

        mark_component_starting(&component);

        let snapshot = snapshot();
        let entry = snapshot
            .components
            .get(&component)
            .expect("component should be present after mark_component_starting");

        assert_eq!(entry.status, "starting");
        assert!(entry.last_ok.is_none());
    }

    #[test]
    fn mark_component_starting_invalidates_a_previous_ok() {
        // The registry outlives a daemon reload, so a replacement incarnation
        // reuses the name its predecessor wrote to. Inheriting that `ok` would
        // report a component as healthy on evidence it never produced.
        let component = unique_component("health-starting-reset");

        mark_component_ok(&component);
        let healthy = snapshot();
        let healthy = healthy
            .components
            .get(&component)
            .expect("component should exist after mark_component_ok");
        assert_eq!(healthy.status, "ok");
        assert!(healthy.last_ok.is_some());

        mark_component_starting(&component);

        let restarted = snapshot();
        let restarted = restarted
            .components
            .get(&component)
            .expect("component should still exist");
        assert_eq!(restarted.status, "starting");
        assert!(
            restarted.last_ok.is_none(),
            "the previous incarnation's last_ok must not carry forward"
        );
    }

    #[test]
    fn mark_component_starting_preserves_a_recorded_error_and_restart_count() {
        // Erasing a recorded failure is how a status becomes misleading:
        // "starting" is not evidence that a prior error has been resolved.
        let component = unique_component("health-starting-keeps-error");

        bump_component_restart(&component);
        mark_component_error(&component, "boom");

        mark_component_starting(&component);

        let snapshot = snapshot();
        let entry = snapshot
            .components
            .get(&component)
            .expect("component should exist");

        assert_eq!(entry.status, "starting");
        assert_eq!(entry.last_error.as_deref(), Some("boom"));
        assert_eq!(entry.restart_count, 1);
    }

    #[test]
    fn snapshot_json_contains_registered_component_fields() {
        let component = unique_component("health-json");

        mark_component_ok(&component);

        let json = snapshot_json();
        let component_json = &json["components"][&component];

        assert_eq!(component_json["status"], "ok");
        assert!(component_json["updated_at"].as_str().is_some());
        assert!(component_json["last_ok"].as_str().is_some());
        assert!(json["uptime_seconds"].as_u64().is_some());
    }
}
