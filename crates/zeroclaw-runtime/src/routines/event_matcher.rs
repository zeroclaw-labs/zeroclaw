//! Event pattern matching for the routines engine.
//!
//! Supports three match strategies: exact, glob, and regex.  Each routine
//! declares one or more [`EventPattern`]s; an incoming [`RoutineEvent`] fires
//! the routine when **any** pattern matches.

use serde::{Deserialize, Serialize};

/// How a pattern string should be interpreted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MatchStrategy {
    /// Case-sensitive exact string comparison.
    #[default]
    Exact,
    /// Unix-style glob (supports `*`, `?`, `[…]`).
    Glob,
    /// Full regular expression (Rust `regex` crate syntax).
    Regex,
}

/// A single event pattern attached to a routine trigger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventPattern {
    /// The source type this pattern applies to (e.g. `"channel"`, `"webhook"`,
    /// `"cron"`, `"system"`).  Must match `RoutineEvent::source` exactly.
    pub source: String,

    /// Pattern to match against `RoutineEvent::topic`.
    /// Interpretation depends on `strategy`.
    pub pattern: String,

    /// How to interpret `pattern`.
    #[serde(default)]
    pub strategy: MatchStrategy,
}

/// An event emitted by the system that may trigger routines.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutineEvent {
    /// Source type: `"channel"`, `"webhook"`, `"cron"`, `"system"`.
    pub source: String,
    /// Topic / identifier to match against (channel name, webhook path, cron
    /// label, system event name).
    pub topic: String,
    /// Optional payload (JSON string, message text, etc.).
    #[serde(default)]
    pub payload: Option<String>,
    /// ISO-8601 timestamp.
    pub timestamp: String,
}

/// Check whether an event matches a single pattern.
pub fn matches(pattern: &EventPattern, event: &RoutineEvent) -> bool {
    if pattern.source != event.source {
        return false;
    }
    match pattern.strategy {
        MatchStrategy::Exact => pattern.pattern == event.topic,
        MatchStrategy::Glob => glob_match(&pattern.pattern, &event.topic),
        MatchStrategy::Regex => regex_match(&pattern.pattern, &event.topic),
    }
}

/// Check whether an event matches **any** of the given patterns.
pub fn matches_any(patterns: &[EventPattern], event: &RoutineEvent) -> bool {
    patterns.iter().any(|p| matches(p, event))
}

fn glob_match(pattern: &str, text: &str) -> bool {
    glob::Pattern::new(pattern).is_ok_and(|g| g.matches(text))
}

fn regex_match(pattern: &str, text: &str) -> bool {
    regex::Regex::new(pattern).is_ok_and(|re| re.is_match(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(source: &str, topic: &str) -> RoutineEvent {
        RoutineEvent {
            source: source.into(),
            topic: topic.into(),
            payload: None,
            timestamp: "2026-03-24T00:00:00Z".into(),
        }
    }

    #[test]
    fn exact_match_works() {
        let pat = EventPattern {
            source: "webhook".into(),
            pattern: "/api/deploy".into(),
            strategy: MatchStrategy::Exact,
        };
        assert!(matches(&pat, &event("webhook", "/api/deploy")));
        assert!(!matches(&pat, &event("webhook", "/api/deploy/staging")));
        assert!(!matches(&pat, &event("channel", "/api/deploy")));
    }

    #[test]
    fn glob_match_works() {
        let pat = EventPattern {
            source: "channel".into(),
            pattern: "telegram-*".into(),
            strategy: MatchStrategy::Glob,
        };
        assert!(matches(&pat, &event("channel", "telegram-main")));
        assert!(matches(&pat, &event("channel", "telegram-alerts")));
        assert!(!matches(&pat, &event("channel", "discord-main")));
    }

    #[test]
    fn regex_match_works() {
        let pat = EventPattern {
            source: "system".into(),
            pattern: r"^build\.(success|failure)$".into(),
            strategy: MatchStrategy::Regex,
        };
        assert!(matches(&pat, &event("system", "build.success")));
        assert!(matches(&pat, &event("system", "build.failure")));
        assert!(!matches(&pat, &event("system", "build.pending")));
    }

    #[test]
    fn matches_any_returns_true_on_first_hit() {
        let patterns = vec![
            EventPattern {
                source: "webhook".into(),
                pattern: "/deploy".into(),
                strategy: MatchStrategy::Exact,
            },
            EventPattern {
                source: "channel".into(),
                pattern: "slack-*".into(),
                strategy: MatchStrategy::Glob,
            },
        ];
        assert!(matches_any(&patterns, &event("channel", "slack-general")));
        assert!(!matches_any(
            &patterns,
            &event("channel", "discord-general")
        ));
    }

    #[test]
    fn source_mismatch_never_matches() {
        let pat = EventPattern {
            source: "cron".into(),
            pattern: "*".into(),
            strategy: MatchStrategy::Glob,
        };
        assert!(!matches(&pat, &event("webhook", "anything")));
    }

    #[test]
    fn invalid_regex_returns_false() {
        let pat = EventPattern {
            source: "system".into(),
            pattern: "[invalid".into(),
            strategy: MatchStrategy::Regex,
        };
        assert!(!matches(&pat, &event("system", "anything")));
    }

    #[test]
    fn invalid_glob_returns_false() {
        let pat = EventPattern {
            source: "system".into(),
            pattern: "[!invalid".into(),
            strategy: MatchStrategy::Glob,
        };
        // glob::Pattern::new will fail for malformed patterns
        assert!(!matches(&pat, &event("system", "anything")));
    }

    #[test]
    fn default_strategy_is_exact() {
        assert_eq!(MatchStrategy::default(), MatchStrategy::Exact);
    }
}
