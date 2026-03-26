//! Routines engine — event-triggered automation with pattern matching and
//! cooldown enforcement.
//!
//! A **routine** is a lightweight automation rule: when an event matches one of
//! its patterns, the associated action fires (provided cooldown has elapsed).
//! The engine bridges channel messages, cron ticks, webhooks, and system events
//! into the existing SOP pipeline.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::event_matcher::{matches_any, EventPattern, RoutineEvent};

/// What happens when a routine fires.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RoutineAction {
    /// Trigger an SOP by name.
    Sop { name: String },
    /// Execute a shell command.
    Shell { command: String },
    /// Send a message to a channel.
    Message { channel: String, text: String },
    /// Run a cron job by name.
    CronJob { job_name: String },
}

/// A single automation routine definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Routine {
    /// Unique name for this routine.
    pub name: String,
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
    /// Event patterns that trigger this routine.
    pub patterns: Vec<EventPattern>,
    /// Action to execute when triggered.
    pub action: RoutineAction,
    /// Minimum seconds between firings (0 = no cooldown).
    #[serde(default)]
    pub cooldown_secs: u64,
    /// Whether this routine is enabled.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

/// TOML manifest for a routines file.
#[derive(Debug, Clone, Deserialize)]
pub struct RoutinesManifest {
    #[serde(default)]
    pub routines: Vec<Routine>,
}

/// Result of dispatching an event through the routines engine.
#[derive(Debug, Clone)]
pub enum RoutineDispatchResult {
    /// The routine fired successfully.
    Fired {
        routine_name: String,
        action: RoutineAction,
    },
    /// The routine matched but is in cooldown.
    Cooldown {
        routine_name: String,
        remaining_secs: u64,
    },
    /// The routine matched but is disabled.
    Disabled { routine_name: String },
    /// No routine matched the event.
    NoMatch,
}

/// The routines engine: holds all loaded routines and tracks cooldowns.
pub struct RoutinesEngine {
    routines: Vec<Routine>,
    /// Last-fired timestamp per routine name.
    cooldowns: HashMap<String, Instant>,
}

impl RoutinesEngine {
    /// Create a new engine with the given routines.
    pub fn new(routines: Vec<Routine>) -> Self {
        Self {
            routines,
            cooldowns: HashMap::new(),
        }
    }

    /// Create an empty engine.
    pub fn empty() -> Self {
        Self::new(Vec::new())
    }

    /// Number of loaded routines.
    pub fn len(&self) -> usize {
        self.routines.len()
    }

    /// Whether the engine has no routines.
    pub fn is_empty(&self) -> bool {
        self.routines.is_empty()
    }

    /// Get all loaded routines.
    pub fn routines(&self) -> &[Routine] {
        &self.routines
    }

    /// Add a routine at runtime.
    pub fn add_routine(&mut self, routine: Routine) {
        self.routines.push(routine);
    }

    /// Remove a routine by name. Returns `true` if removed.
    pub fn remove_routine(&mut self, name: &str) -> bool {
        let before = self.routines.len();
        self.routines.retain(|r| r.name != name);
        self.cooldowns.remove(name);
        self.routines.len() < before
    }

    /// Dispatch an event to all matching routines.
    ///
    /// Returns a result for each matching routine (fired, cooldown, or
    /// disabled).  If no routine matches, returns `[NoMatch]`.
    pub fn dispatch(&mut self, event: &RoutineEvent) -> Vec<RoutineDispatchResult> {
        let mut results = Vec::new();
        let now = Instant::now();

        for routine in &self.routines {
            if !matches_any(&routine.patterns, event) {
                continue;
            }

            if !routine.enabled {
                debug!(routine = %routine.name, "routine matched but disabled");
                results.push(RoutineDispatchResult::Disabled {
                    routine_name: routine.name.clone(),
                });
                continue;
            }

            // Check cooldown
            if routine.cooldown_secs > 0 {
                if let Some(last_fired) = self.cooldowns.get(&routine.name) {
                    let elapsed = now.saturating_duration_since(*last_fired);
                    let cooldown = Duration::from_secs(routine.cooldown_secs);
                    if elapsed < cooldown {
                        let remaining = cooldown.saturating_sub(elapsed).as_secs();
                        debug!(
                            routine = %routine.name,
                            remaining_secs = remaining,
                            "routine in cooldown"
                        );
                        results.push(RoutineDispatchResult::Cooldown {
                            routine_name: routine.name.clone(),
                            remaining_secs: remaining,
                        });
                        continue;
                    }
                }
            }

            info!(routine = %routine.name, source = %event.source, topic = %event.topic, "routine fired");
            self.cooldowns.insert(routine.name.clone(), now);
            results.push(RoutineDispatchResult::Fired {
                routine_name: routine.name.clone(),
                action: routine.action.clone(),
            });
        }

        if results.is_empty() {
            results.push(RoutineDispatchResult::NoMatch);
        }

        results
    }

    /// Clear all cooldown state.
    pub fn reset_cooldowns(&mut self) {
        self.cooldowns.clear();
    }
}

/// Load routines from a TOML file.
pub fn load_routines_from_file(path: &std::path::Path) -> Vec<Routine> {
    match std::fs::read_to_string(path) {
        Ok(content) => match toml::from_str::<RoutinesManifest>(&content) {
            Ok(manifest) => manifest.routines,
            Err(e) => {
                warn!("Failed to parse routines file {}: {e}", path.display());
                Vec::new()
            }
        },
        Err(e) => {
            debug!("Routines file not found at {}: {e}", path.display());
            Vec::new()
        }
    }
}

/// Load routines from the workspace `routines.toml` file.
pub fn load_routines(workspace_dir: &std::path::Path) -> Vec<Routine> {
    let path = workspace_dir.join("routines.toml");
    load_routines_from_file(&path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routines::event_matcher::{EventPattern, MatchStrategy, RoutineEvent};

    fn test_event(source: &str, topic: &str) -> RoutineEvent {
        RoutineEvent {
            source: source.into(),
            topic: topic.into(),
            payload: None,
            timestamp: "2026-03-24T00:00:00Z".into(),
        }
    }

    fn test_routine(name: &str, source: &str, pattern: &str, strategy: MatchStrategy) -> Routine {
        Routine {
            name: name.into(),
            description: String::new(),
            patterns: vec![EventPattern {
                source: source.into(),
                pattern: pattern.into(),
                strategy,
            }],
            action: RoutineAction::Sop {
                name: "test-sop".into(),
            },
            cooldown_secs: 0,
            enabled: true,
        }
    }

    #[test]
    fn dispatch_fires_matching_routine() {
        let mut engine = RoutinesEngine::new(vec![test_routine(
            "deploy-hook",
            "webhook",
            "/deploy",
            MatchStrategy::Exact,
        )]);

        let results = engine.dispatch(&test_event("webhook", "/deploy"));
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0], RoutineDispatchResult::Fired { .. }));
    }

    #[test]
    fn dispatch_returns_no_match() {
        let mut engine = RoutinesEngine::new(vec![test_routine(
            "deploy-hook",
            "webhook",
            "/deploy",
            MatchStrategy::Exact,
        )]);

        let results = engine.dispatch(&test_event("channel", "slack-main"));
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0], RoutineDispatchResult::NoMatch));
    }

    #[test]
    fn dispatch_skips_disabled_routine() {
        let mut routine = test_routine("disabled", "webhook", "/deploy", MatchStrategy::Exact);
        routine.enabled = false;
        let mut engine = RoutinesEngine::new(vec![routine]);

        let results = engine.dispatch(&test_event("webhook", "/deploy"));
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0], RoutineDispatchResult::Disabled { .. }));
    }

    #[test]
    fn dispatch_enforces_cooldown() {
        let mut routine = test_routine("deploy-hook", "webhook", "/deploy", MatchStrategy::Exact);
        routine.cooldown_secs = 3600; // 1 hour
        let mut engine = RoutinesEngine::new(vec![routine]);

        // First dispatch should fire
        let results = engine.dispatch(&test_event("webhook", "/deploy"));
        assert!(matches!(results[0], RoutineDispatchResult::Fired { .. }));

        // Second dispatch should be in cooldown
        let results = engine.dispatch(&test_event("webhook", "/deploy"));
        assert!(matches!(results[0], RoutineDispatchResult::Cooldown { .. }));
    }

    #[test]
    fn dispatch_multiple_routines_match() {
        let mut engine = RoutinesEngine::new(vec![
            test_routine("exact-deploy", "webhook", "/deploy", MatchStrategy::Exact),
            test_routine("glob-deploy", "webhook", "/deploy*", MatchStrategy::Glob),
        ]);

        let results = engine.dispatch(&test_event("webhook", "/deploy"));
        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .all(|r| matches!(r, RoutineDispatchResult::Fired { .. })));
    }

    #[test]
    fn reset_cooldowns_clears_state() {
        let mut routine = test_routine("deploy", "webhook", "/deploy", MatchStrategy::Exact);
        routine.cooldown_secs = 3600;
        let mut engine = RoutinesEngine::new(vec![routine]);

        engine.dispatch(&test_event("webhook", "/deploy")); // fires
        engine.reset_cooldowns();
        let results = engine.dispatch(&test_event("webhook", "/deploy")); // should fire again
        assert!(matches!(results[0], RoutineDispatchResult::Fired { .. }));
    }

    #[test]
    fn add_and_remove_routine() {
        let mut engine = RoutinesEngine::empty();
        assert!(engine.is_empty());

        engine.add_routine(test_routine("r1", "channel", "test", MatchStrategy::Exact));
        assert_eq!(engine.len(), 1);

        assert!(engine.remove_routine("r1"));
        assert!(engine.is_empty());
        assert!(!engine.remove_routine("nonexistent"));
    }

    #[test]
    fn load_routines_from_toml_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("routines.toml");
        std::fs::write(
            &path,
            r#"
[[routines]]
name = "deploy-notify"
description = "Notify on deploy"
cooldown_secs = 60

[[routines.patterns]]
source = "webhook"
pattern = "/deploy"
strategy = "exact"

[routines.action]
type = "message"
channel = "slack-general"
text = "Deploy triggered!"

[[routines]]
name = "build-monitor"
description = "Monitor builds"

[[routines.patterns]]
source = "system"
pattern = "build.*"
strategy = "glob"

[routines.action]
type = "sop"
name = "check-build"
"#,
        )
        .unwrap();

        let routines = load_routines_from_file(&path);
        assert_eq!(routines.len(), 2);
        assert_eq!(routines[0].name, "deploy-notify");
        assert_eq!(routines[0].cooldown_secs, 60);
        assert_eq!(routines[1].name, "build-monitor");
    }

    #[test]
    fn load_routines_missing_file() {
        let routines = load_routines_from_file(std::path::Path::new("/nonexistent/routines.toml"));
        assert!(routines.is_empty());
    }

    #[test]
    fn glob_pattern_dispatch() {
        let mut engine = RoutinesEngine::new(vec![test_routine(
            "channel-watcher",
            "channel",
            "telegram-*",
            MatchStrategy::Glob,
        )]);

        assert!(matches!(
            engine.dispatch(&test_event("channel", "telegram-main"))[0],
            RoutineDispatchResult::Fired { .. }
        ));
        assert!(matches!(
            engine.dispatch(&test_event("channel", "discord-main"))[0],
            RoutineDispatchResult::NoMatch
        ));
    }

    #[test]
    fn regex_pattern_dispatch() {
        let mut engine = RoutinesEngine::new(vec![test_routine(
            "error-watcher",
            "system",
            r"^error\.(critical|fatal)$",
            MatchStrategy::Regex,
        )]);

        assert!(matches!(
            engine.dispatch(&test_event("system", "error.critical"))[0],
            RoutineDispatchResult::Fired { .. }
        ));
        assert!(matches!(
            engine.dispatch(&test_event("system", "error.warning"))[0],
            RoutineDispatchResult::NoMatch
        ));
    }

    #[test]
    fn routine_action_serde_roundtrip() {
        let action = RoutineAction::Sop {
            name: "test-sop".into(),
        };
        let json = serde_json::to_string(&action).unwrap();
        let parsed: RoutineAction = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, RoutineAction::Sop { name } if name == "test-sop"));
    }
}
