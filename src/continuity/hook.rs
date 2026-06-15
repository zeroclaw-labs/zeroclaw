//! HookHandler glue that wires cross-session continuity into the agent
//! runtime without the runtime crate depending on this binary-local
//! module. Mirrors the conscience hook's factory-registry pattern.
//!
//! Wiring path:
//! ```text
//! main.rs -> continuity::hook::register_hook_factory()
//!     -> zeroclaw_runtime::hooks::registry::register_factory(...)
//! per Agent build (only when `[continuity].enabled`):
//! ContinuityHook::with_persistence loads <data_dir>/continuity/preferences.json
//! before every LLM call:
//! before_llm_call -> prepend a system message carrying learned preferences
//! after every successful tool call:
//! on_after_tool_call -> extract_tool_preference -> autosave when a new
//! affinity is learned
//! ```
//!
//! The hook holds its `PreferenceModel` behind an `Arc<Mutex<…>>` because
//! the `HookHandler` methods take `&self`; the lock is only held for the
//! brief snapshot/update, never across an await.

use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use zeroclaw_api::model_provider::ChatMessage;
use zeroclaw_api::tool::ToolResult;
use zeroclaw_config::schema::Config;
use zeroclaw_runtime::hooks::{HookHandler, HookResult};

use super::PreferenceModel;
use super::extraction::extract_tool_preference;
use super::persistence::{continuity_dir, load_preferences, save_preferences};
use super::types::DriftLimits;

/// Per-Agent hook that loads, applies, and persists the agent's learned
/// preferences across sessions.
pub struct ContinuityHook {
    preferences: Arc<Mutex<PreferenceModel>>,
    /// Continuity directory the preferences autosave to. `None` for
    /// in-memory-only operation (tests and the `new` convenience ctor).
    dir: Option<PathBuf>,
}

impl ContinuityHook {
    /// In-memory hook with no disk persistence. Used by tests; production
    /// goes through [`Self::with_persistence`].
    pub fn new() -> Self {
        Self {
            preferences: Arc::new(Mutex::new(PreferenceModel::new(DriftLimits::default()))),
            dir: None,
        }
    }

    /// Load the persisted `PreferenceModel` from `<data_dir>/continuity/`
    /// and autosave back to it. Missing or unreadable persistence falls
    /// back to a fresh, empty model so first boot can't fail closed.
    pub fn with_persistence(data_dir: &Path) -> Self {
        let dir = continuity_dir(data_dir, None).ok();
        let model = match dir.as_deref().map(load_preferences) {
            Some(Ok(prefs)) => PreferenceModel::from_preferences(prefs, DriftLimits::default()),
            _ => PreferenceModel::new(DriftLimits::default()),
        };
        Self {
            preferences: Arc::new(Mutex::new(model)),
            dir,
        }
    }

    /// Borrow the model for diagnostics or testing.
    #[cfg(test)]
    pub(super) fn preferences(&self) -> Arc<Mutex<PreferenceModel>> {
        Arc::clone(&self.preferences)
    }
}

impl Default for ContinuityHook {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HookHandler for ContinuityHook {
    fn name(&self) -> &str {
        "continuity"
    }

    async fn before_llm_call(
        &self,
        mut messages: Vec<ChatMessage>,
        model: String,
    ) -> HookResult<(Vec<ChatMessage>, String)> {
        // Inject the learned preferences as a leading system message so the
        // model reuses them this turn. `to_prompt_context` already filters
        // to confidence ≥ 0.3 and returns empty when there's nothing worth
        // surfacing, in which case we leave the history untouched.
        let context = match self.preferences.lock() {
            Ok(guard) => guard.to_prompt_context(),
            Err(poisoned) => poisoned.into_inner().to_prompt_context(),
        };
        if !context.is_empty() {
            messages.insert(0, ChatMessage::system(context));
        }
        HookResult::Continue((messages, model))
    }

    async fn on_after_tool_call(&self, tool: &str, result: &ToolResult, _duration: Duration) {
        // Learn a tool-affinity preference from successful calls. Save only
        // when a genuinely new affinity is recorded — repeat calls update an
        // existing key to the same value and are no-ops, so the preference
        // count growing is a reliable "something new" signal that avoids
        // disk writes on the hot path.
        let learned = match self.preferences.lock() {
            Ok(mut guard) => {
                let before = guard.preferences().len();
                let _ = extract_tool_preference(&mut guard, tool, result.success);
                guard.preferences().len() != before
            }
            Err(_) => false,
        };

        if learned && let Some(dir) = self.dir.as_deref() {
            let snapshot: Option<Vec<_>> = self
                .preferences
                .lock()
                .ok()
                .map(|g| g.preferences().to_vec());
            if let Some(prefs) = snapshot
                && let Err(err) = save_preferences(dir, &prefs)
            {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "dir": dir.display().to_string(),
                            "error": err.to_string(),
                        })),
                    "continuity: preference autosave failed; in-memory state still authoritative"
                );
            }
        }
    }
}

/// Install the continuity hook factory on the runtime's global registry.
/// Called once from binary startup (gated on `x0-extended`). The factory
/// inspects `[continuity].enabled` at Agent-build time.
pub fn register_hook_factory() {
    zeroclaw_runtime::hooks::registry::register_factory(Box::new(|cfg: &Config| {
        if cfg.continuity.enabled {
            vec![Box::new(ContinuityHook::with_persistence(&cfg.data_dir))]
        } else {
            Vec::new()
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::continuity::types::PreferenceCategory;

    fn tool_ok() -> ToolResult {
        ToolResult {
            success: true,
            output: "ok".into(),
            error: None,
        }
    }

    fn tool_fail() -> ToolResult {
        ToolResult {
            success: false,
            output: String::new(),
            error: Some("boom".into()),
        }
    }

    #[tokio::test]
    async fn before_llm_call_injects_learned_preferences() {
        let hook = ContinuityHook::new();
        hook.preferences()
            .lock()
            .unwrap()
            .update(
                "tool_affinity:shell",
                "preferred",
                0.5,
                PreferenceCategory::Technical,
            )
            .unwrap();

        match hook
            .before_llm_call(vec![ChatMessage::user("hello")], "m".into())
            .await
        {
            HookResult::Continue((out, _)) => {
                assert_eq!(
                    out.len(),
                    2,
                    "a system preference message must be prepended"
                );
                assert_eq!(out[0].role, "system");
                assert!(
                    out[0].content.contains("tool_affinity:shell"),
                    "injected context must carry the learned preference, got: {}",
                    out[0].content
                );
            }
            HookResult::Cancel(_) => panic!("continuity hook must never cancel"),
        }
    }

    #[tokio::test]
    async fn before_llm_call_is_noop_without_preferences() {
        let hook = ContinuityHook::new();
        match hook
            .before_llm_call(vec![ChatMessage::user("hi")], "m".into())
            .await
        {
            HookResult::Continue((out, _)) => {
                assert_eq!(out.len(), 1, "an empty model must inject nothing");
                assert_eq!(out[0].role, "user");
            }
            HookResult::Cancel(_) => panic!("continuity hook must never cancel"),
        }
    }

    #[tokio::test]
    async fn learns_tool_affinity_on_success() {
        let hook = ContinuityHook::new();
        hook.on_after_tool_call("file_read", &tool_ok(), Duration::ZERO)
            .await;
        assert!(
            hook.preferences()
                .lock()
                .unwrap()
                .get("tool_affinity:file_read")
                .is_some(),
            "a successful tool call must record an affinity"
        );
    }

    #[tokio::test]
    async fn ignores_failed_tool_calls() {
        let hook = ContinuityHook::new();
        hook.on_after_tool_call("shell", &tool_fail(), Duration::ZERO)
            .await;
        assert!(
            hook.preferences()
                .lock()
                .unwrap()
                .get("tool_affinity:shell")
                .is_none(),
            "a failed tool call must not record an affinity"
        );
    }

    #[tokio::test]
    async fn preferences_persist_across_hook_instances() {
        let tmp = tempfile::tempdir().unwrap();
        {
            let hook = ContinuityHook::with_persistence(tmp.path());
            hook.on_after_tool_call("glob_search", &tool_ok(), Duration::ZERO)
                .await;
        }
        // A fresh hook over the same data dir must reload the saved affinity.
        let hook2 = ContinuityHook::with_persistence(tmp.path());
        assert!(
            hook2
                .preferences()
                .lock()
                .unwrap()
                .get("tool_affinity:glob_search")
                .is_some(),
            "a learned affinity must survive a simulated restart"
        );
    }
}
