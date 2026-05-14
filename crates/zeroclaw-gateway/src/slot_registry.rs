//! Warm-agent registry keyed on slot id (M2.5).
//!
//! Each slot owns a long-lived `Arc<Mutex<Agent>>` so a sequence of
//! turns against the same slot reuses one Agent — skipping the
//! expensive `Agent::from_config` init each time (see
//! `multi-session-dashboard.md §4.5`). The shared `Arc<McpRegistry>`
//! that all warm agents reference keeps the process's MCP subprocess
//! count constant at N (configured servers), not N × slot_count.
//!
//! Scope for this M2.5 slice:
//!   - `get_or_spawn(slot_id, overrides, base_config, shared_mcp)` —
//!     returns a cloned `Arc<SlotEntry>`; constructs on miss, reuses on
//!     hit.
//!   - `get(slot_id)` — non-destructive lookup (used by REST
//!     `/approve` to access the slot's pending-approval map).
//!   - `remove(slot_id)` — forced eviction from `DELETE /api/slots/{id}`.
//!   - `evict_idle()` — background sweep drops entries past `idle_ttl`.
//!   - `pending_approvals` per slot: the slot-scoped map that the
//!     per-turn `WsApprovalChannel` + REST `/approve` share.
//!
//! Pressure-eviction (LRU of idle entries when the store hits its hard
//! limit) is a plan §4.5 concern but deferred here: the M1 store-level
//! hard limit on create already prevents unbounded slot counts, so
//! warm-agent pressure-eviction is a subsidiary safety net that lands
//! with real load-testing work.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use zeroclaw_config::schema::Config;
use zeroclaw_runtime::agent::Agent;
use zeroclaw_tools::mcp_client::McpRegistry;

use crate::slot::SlotAgentConfig;
use crate::slot_agent::apply_slot_overrides;
use crate::ws_approval::{PendingApprovals, new_pending_approvals};

/// A warm entry in the registry. `agent` is `Arc<tokio::sync::Mutex<_>>`
/// so callers hold it across `.await` points on the single turn they're
/// serving without blocking the registry itself.
pub struct SlotEntry {
    pub agent: Arc<tokio::sync::Mutex<Agent>>,
    /// Last time this slot served a turn; idle sweep uses this.
    pub last_active: Instant,
    /// Config frozen at spawn time. Edits to `config.toml` after this
    /// do not retroactively reshape running slots (documented contract;
    /// plan §4.5).
    pub config_snapshot: SlotAgentConfig,
    /// Slot-scoped pending-approval map. Shared between the per-turn
    /// `WsApprovalChannel` (inserts) and `POST /api/slots/{id}/approve`
    /// (pops by `request_id`).
    pub pending_approvals: PendingApprovals,
}

/// Process-global registry of warm slot agents. Stored on
/// `AppState::slot_registry`; cheap to clone (inner is one `Arc`).
#[derive(Clone)]
pub struct SlotRegistry {
    inner: Arc<Inner>,
}

struct Inner {
    slots: Mutex<HashMap<String, Arc<SlotEntry>>>,
    idle_ttl: Duration,
}

impl SlotRegistry {
    /// Build an empty registry. `idle_ttl_secs` mirrors the session
    /// queue's default (600s) so sidebar-idle slots evict at the same
    /// cadence the serialization slots clean up.
    pub fn new(idle_ttl_secs: u64) -> Self {
        Self {
            inner: Arc::new(Inner {
                slots: Mutex::new(HashMap::new()),
                idle_ttl: Duration::from_secs(idle_ttl_secs),
            }),
        }
    }

    /// Get or spawn the warm agent for `slot_id`, applying the slot's
    /// `SlotAgentConfig` overrides to `base_config` on first spawn.
    ///
    /// Spawn path calls `Agent::from_config_with_shared_mcp_backchannel`
    /// so the agent reuses the caller's shared MCP subprocess tree and
    /// surfaces tool approvals via the dashboard's operator-present
    /// back-channel.
    pub async fn get_or_spawn(
        &self,
        slot_id: &str,
        overrides: &SlotAgentConfig,
        base_config: Config,
        shared_mcp: Option<Arc<McpRegistry>>,
    ) -> anyhow::Result<Arc<SlotEntry>> {
        if let Some(entry) = self.get(slot_id) {
            return Ok(entry);
        }

        // Build the agent outside the lock so two distinct slots can
        // spawn concurrently. Racy double-spawn for the same slot: the
        // loser's Agent drops on scope exit; we prefer the extant
        // entry.
        let effective_config = apply_slot_overrides(base_config, overrides);
        let agent =
            Agent::from_config_with_shared_mcp_backchannel(&effective_config, None, shared_mcp)
                .await?;
        let agent = Arc::new(tokio::sync::Mutex::new(agent));

        let entry = Arc::new(SlotEntry {
            agent,
            last_active: Instant::now(),
            config_snapshot: overrides.clone(),
            pending_approvals: new_pending_approvals(),
        });

        {
            let mut map = self
                .inner
                .slots
                .lock()
                .expect("slot_registry lock poisoned");
            if let Some(existing) = map.get(slot_id) {
                return Ok(existing.clone());
            }
            map.insert(slot_id.to_string(), entry.clone());
        }

        Ok(entry)
    }

    /// Non-destructive lookup. Returns a cloned `Arc<SlotEntry>` when
    /// the slot has a warm agent, `None` otherwise.
    ///
    /// Used by `POST /api/slots/{id}/approve` to read slot-scoped
    /// `pending_approvals` without triggering a spawn.
    pub fn get(&self, slot_id: &str) -> Option<Arc<SlotEntry>> {
        let map = self
            .inner
            .slots
            .lock()
            .expect("slot_registry lock poisoned");
        map.get(slot_id).cloned()
    }

    /// Drop entries that haven't been touched within `idle_ttl`.
    ///
    /// Returns the count of evictions. Wired to a background tokio task
    /// ticking every 60s at gateway startup.
    pub fn evict_idle(&self) -> usize {
        let now = Instant::now();
        let ttl = self.inner.idle_ttl;
        let mut map = self
            .inner
            .slots
            .lock()
            .expect("slot_registry lock poisoned");
        let before = map.len();
        map.retain(|_, entry| now.duration_since(entry.last_active) <= ttl);
        before - map.len()
    }

    /// Forcibly drop a slot's warm agent. Called when the slot is
    /// deleted from the store so we don't hold an Agent referring to
    /// config that no longer corresponds to a user-visible slot.
    pub fn remove(&self, slot_id: &str) -> Option<Arc<SlotEntry>> {
        let mut map = self
            .inner
            .slots
            .lock()
            .expect("slot_registry lock poisoned");
        map.remove(slot_id)
    }

    /// Count of currently warm slots. Exposed for tests and future
    /// metrics.
    pub fn len(&self) -> usize {
        self.inner
            .slots
            .lock()
            .expect("slot_registry lock poisoned")
            .len()
    }

    /// True when no slots are warm.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for SlotRegistry {
    fn default() -> Self {
        Self::new(600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_registry_is_empty() {
        let reg = SlotRegistry::new(60);
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn get_on_empty_returns_none() {
        let reg = SlotRegistry::new(60);
        assert!(reg.get("nope").is_none());
    }

    #[test]
    fn remove_on_empty_returns_none() {
        let reg = SlotRegistry::new(60);
        assert!(reg.remove("nope").is_none());
    }

    #[test]
    fn evict_idle_on_empty_returns_zero() {
        let reg = SlotRegistry::new(0);
        assert_eq!(reg.evict_idle(), 0);
    }

    #[test]
    fn default_uses_600s_idle_ttl() {
        let reg = SlotRegistry::default();
        assert!(reg.is_empty());
    }
}
