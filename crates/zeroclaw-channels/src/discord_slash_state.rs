//! Persisted Discord slash-command reconcile state.
//!
//! The Discord channel reconciles its application's global slash commands with
//! the desired (skill-derived) set on every gateway `READY`. Discord's daily
//! command-create budget is finite, so re-running a full GET + per-command diff
//! on every process start — and re-hammering after a rate-limit — risks the
//! daily reconcile-429 burst.
//!
//! This module persists, per application id, the fingerprint of the last
//! *successful* reconcile and any active `Retry-After` cooldown, so that a
//! restart skips an unchanged set and honours a server-imposed back-off instead
//! of immediately retrying. State lives at `<workspace_dir>/state/slash-commands.json`
//! (the same `state/` convention the model cache uses); when no workspace dir is
//! configured, persistence degrades to a no-op and the channel behaves as it did
//! before — a full reconcile each start.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const STATE_FILE: &str = "slash-commands.json";

/// Persisted reconcile state for one Discord application.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SlashReconcileState {
    /// Application id this state belongs to. State recorded for a different
    /// application (e.g. a swapped bot token) is ignored on load, degrading to
    /// a full reconcile rather than trusting a stale fingerprint.
    pub app_id: Option<String>,
    /// Fingerprint of the last successfully reconciled command set.
    pub fingerprint: Option<u64>,
    /// Unix seconds of the last successful reconcile (diagnostic).
    pub last_success_at: Option<i64>,
    /// Unix-seconds deadline before which no reconcile should be attempted,
    /// recorded from a `429` `Retry-After`. `None` once a reconcile succeeds.
    pub retry_after_until: Option<i64>,
}

impl SlashReconcileState {
    fn dir(workspace_dir: &Path) -> PathBuf {
        workspace_dir.join("state")
    }

    fn path(workspace_dir: &Path) -> PathBuf {
        Self::dir(workspace_dir).join(STATE_FILE)
    }

    /// Load persisted state for `app_id`. Returns the default (empty) state —
    /// which forces a full reconcile — when no workspace dir is configured, the
    /// file is absent or unreadable, or the recorded application id does not
    /// match. Never fails; a corrupt or foreign file simply yields "no state".
    pub fn load(workspace_dir: Option<&Path>, app_id: &str) -> Self {
        let Some(ws) = workspace_dir else {
            return Self::default();
        };
        let Ok(bytes) = std::fs::read(Self::path(ws)) else {
            return Self::default();
        };
        match serde_json::from_slice::<Self>(&bytes) {
            Ok(state) if state.app_id.as_deref() == Some(app_id) => state,
            _ => Self::default(),
        }
    }

    /// Record a successful reconcile: store the fingerprint, clear any
    /// `Retry-After` cooldown. Best-effort — a write failure is logged at debug
    /// and otherwise ignored (the in-process flow already succeeded).
    pub fn record_success(workspace_dir: Option<&Path>, app_id: &str, fingerprint: u64, now: i64) {
        Self {
            app_id: Some(app_id.to_string()),
            fingerprint: Some(fingerprint),
            last_success_at: Some(now),
            retry_after_until: None,
        }
        .write(workspace_dir);
    }

    /// Record a rate-limit cooldown observed during reconcile. The prior
    /// fingerprint is preserved (the desired set did not change, only the
    /// server asked us to wait), so the cooldown — not a forced re-diff — gates
    /// the next attempt.
    pub fn record_retry_after(workspace_dir: Option<&Path>, app_id: &str, prev: &Self, until: i64) {
        Self {
            app_id: Some(app_id.to_string()),
            fingerprint: prev.fingerprint,
            last_success_at: prev.last_success_at,
            retry_after_until: Some(until),
        }
        .write(workspace_dir);
    }

    /// True when a recorded `Retry-After` cooldown is still in the future.
    pub fn rate_limited(&self, now: i64) -> bool {
        self.retry_after_until.is_some_and(|until| until > now)
    }

    fn write(&self, workspace_dir: Option<&Path>) {
        let Some(ws) = workspace_dir else {
            return;
        };
        let dir = Self::dir(ws);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"error": e.to_string()})),
                "could not create slash-command state dir; reconcile state not persisted"
            );
            return;
        }
        // The state dir may hold per-channel material; keep it owner-only.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        let Ok(bytes) = serde_json::to_vec_pretty(self) else {
            return;
        };
        // Write to a unique temp file then atomically rename, so overlapping
        // READY tasks (a reconnect storm) can never observe a torn file — a
        // half-written file would just load as "no state" and force a needless
        // reconcile, the very churn this persistence removes.
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let tmp = dir.join(format!(
            "{STATE_FILE}.{}.{}.tmp",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        if let Err(e) =
            std::fs::write(&tmp, &bytes).and_then(|()| std::fs::rename(&tmp, Self::path(ws)))
        {
            let _ = std::fs::remove_file(&tmp);
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"error": e.to_string()})),
                "could not persist slash-command reconcile state"
            );
        }
    }
}

/// Current unix time in seconds.
pub fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Derive a `Retry-After` deadline (unix seconds) from a `429` response.
///
/// Discord returns the wait in the JSON body's `retry_after` (seconds, may be
/// fractional); the `Retry-After` and `X-RateLimit-Reset-After` headers carry
/// the same in seconds. We take the first present, in that order, round up, and
/// floor at one second so a missing/zero value still yields a real cooldown.
pub fn retry_after_deadline(
    headers: &reqwest::header::HeaderMap,
    body: Option<&serde_json::Value>,
    now: i64,
) -> i64 {
    let secs = body
        .and_then(|b| b.get("retry_after"))
        .and_then(serde_json::Value::as_f64)
        .or_else(|| header_secs(headers, "retry-after"))
        .or_else(|| header_secs(headers, "x-ratelimit-reset-after"))
        .unwrap_or(1.0);
    now + (secs.ceil() as i64).max(1)
}

fn header_secs(headers: &reqwest::header::HeaderMap, name: &str) -> Option<f64> {
    headers.get(name)?.to_str().ok()?.trim().parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderMap;

    #[test]
    fn round_trip_persists_and_loads_for_matching_app() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        SlashReconcileState::record_success(Some(ws), "app-1", 42, 1000);
        let loaded = SlashReconcileState::load(Some(ws), "app-1");
        assert_eq!(loaded.fingerprint, Some(42));
        assert_eq!(loaded.last_success_at, Some(1000));
        assert_eq!(loaded.retry_after_until, None);
    }

    #[test]
    fn load_ignores_state_for_a_different_app() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        SlashReconcileState::record_success(Some(ws), "app-1", 42, 1000);
        // A different application id (swapped token) must not trust the file.
        assert_eq!(
            SlashReconcileState::load(Some(ws), "app-2"),
            SlashReconcileState::default()
        );
    }

    #[test]
    fn missing_workspace_dir_is_a_noop_and_loads_default() {
        // No panic, no persistence, full-reconcile default.
        SlashReconcileState::record_success(None, "app-1", 42, 1000);
        assert_eq!(
            SlashReconcileState::load(None, "app-1"),
            SlashReconcileState::default()
        );
    }

    #[test]
    fn retry_after_is_preserved_with_prior_fingerprint() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        SlashReconcileState::record_success(Some(ws), "app-1", 7, 1000);
        let prev = SlashReconcileState::load(Some(ws), "app-1");
        SlashReconcileState::record_retry_after(Some(ws), "app-1", &prev, 2000);
        let loaded = SlashReconcileState::load(Some(ws), "app-1");
        assert_eq!(
            loaded.fingerprint,
            Some(7),
            "fingerprint kept across back-off"
        );
        assert_eq!(loaded.retry_after_until, Some(2000));
        assert!(loaded.rate_limited(1999));
        assert!(!loaded.rate_limited(2000), "deadline is exclusive");
        assert!(!loaded.rate_limited(2001));
    }

    #[test]
    fn corrupt_file_loads_default() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        std::fs::create_dir_all(ws.join("state")).unwrap();
        std::fs::write(ws.join("state").join(STATE_FILE), b"not json").unwrap();
        assert_eq!(
            SlashReconcileState::load(Some(ws), "app-1"),
            SlashReconcileState::default()
        );
    }

    #[test]
    fn retry_after_prefers_body_then_headers() {
        let now = 1000;
        // Body wins, fractional rounds up.
        let body = serde_json::json!({"retry_after": 1.2});
        assert_eq!(
            retry_after_deadline(&HeaderMap::new(), Some(&body), now),
            1002
        );

        // Header used when body absent.
        let mut h = HeaderMap::new();
        h.insert("retry-after", "3".parse().unwrap());
        assert_eq!(retry_after_deadline(&h, None, now), 1003);

        // X-RateLimit-Reset-After as last resort.
        let mut h2 = HeaderMap::new();
        h2.insert("x-ratelimit-reset-after", "2.5".parse().unwrap());
        assert_eq!(retry_after_deadline(&h2, None, now), 1003);

        // Nothing present → a minimum 1s cooldown, never zero.
        assert_eq!(retry_after_deadline(&HeaderMap::new(), None, now), 1001);
    }
}
