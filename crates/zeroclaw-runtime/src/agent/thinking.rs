//! Thinking/Reasoning Level Control
//!
//! Allows users to control how deeply the model reasons per message,
//! trading speed for depth. Levels range from `Off` (fastest, most concise)
//! to `Max` (deepest reasoning, slowest).
//!
//! Users can set the level via:
//! - Inline directive: `/think:high` at the start of a message
//! - Agent config: `[agent.thinking]` section with `default_level`
//!
//! Resolution hierarchy (highest priority first):
//! 1. Inline directive (`/think:<level>`)
//! 2. Session override (reserved for future use)
//! 3. Agent config (`agent.thinking.default_level`)
//! 4. Global default (`Medium`)

// Re-exported from zeroclaw-config.
pub use zeroclaw_config::scattered_types::{ThinkingConfig, ThinkingLevel};

/// Parameters derived from a thinking level, applied to the LLM request.
#[derive(Debug, Clone, PartialEq)]
pub struct ThinkingParams {
    /// Temperature adjustment (added to the base temperature, clamped to 0.0..=2.0).
    pub temperature_adjustment: f64,
    /// Maximum tokens adjustment (added to any existing max_tokens setting).
    pub max_tokens_adjustment: i64,
    /// Optional system prompt prefix injected before the existing system prompt.
    pub system_prompt_prefix: Option<String>,
    /// Native extended thinking parameters, populated when the config enables
    /// native thinking and the level has a `budget_tokens` value.
    pub native_thinking: Option<zeroclaw_config::scattered_types::NativeThinkingParams>,
}

/// Parse a `/think:<level>` directive from the start of a message.
///
/// Returns `Some((level, remaining_message))` if a directive is found,
/// or `None` if no directive is present. The remaining message has
/// leading whitespace after the directive trimmed.
pub fn parse_thinking_directive(message: &str) -> Option<(ThinkingLevel, String)> {
    let trimmed = message.trim_start();
    if !trimmed.starts_with("/think:") {
        return None;
    }

    // Extract the level token (everything between `/think:` and the next whitespace or end).
    let after_prefix = &trimmed["/think:".len()..];
    let level_end = after_prefix
        .find(|c: char| c.is_whitespace())
        .unwrap_or(after_prefix.len());
    let level_str = &after_prefix[..level_end];

    let level = ThinkingLevel::from_str_insensitive(level_str)?;

    let remaining = after_prefix[level_end..].trim_start().to_string();
    Some((level, remaining))
}

/// Convert a `ThinkingLevel` into concrete parameters for the LLM request.
pub fn apply_thinking_level(level: ThinkingLevel) -> ThinkingParams {
    match level {
        ThinkingLevel::Off => ThinkingParams {
            temperature_adjustment: -0.2,
            max_tokens_adjustment: -1000,
            system_prompt_prefix: Some(
                "Be extremely concise. Give direct answers without explanation \
                 unless explicitly asked. No preamble."
                    .into(),
            ),
            native_thinking: None,
        },
        ThinkingLevel::Minimal => ThinkingParams {
            temperature_adjustment: -0.1,
            max_tokens_adjustment: -500,
            system_prompt_prefix: Some(
                "Be concise and fast. Keep explanations brief. \
                 Prioritize speed over thoroughness."
                    .into(),
            ),
            native_thinking: None,
        },
        ThinkingLevel::Low => ThinkingParams {
            temperature_adjustment: -0.05,
            max_tokens_adjustment: 0,
            system_prompt_prefix: Some("Keep reasoning light. Explain only when helpful.".into()),
            native_thinking: None,
        },
        ThinkingLevel::Medium => ThinkingParams {
            temperature_adjustment: 0.0,
            max_tokens_adjustment: 0,
            system_prompt_prefix: None,
            native_thinking: None,
        },
        ThinkingLevel::High => ThinkingParams {
            temperature_adjustment: 0.05,
            max_tokens_adjustment: 1000,
            system_prompt_prefix: Some(
                "Think step by step. Provide thorough analysis and \
                 consider edge cases before answering."
                    .into(),
            ),
            native_thinking: None,
        },
        ThinkingLevel::Max => ThinkingParams {
            temperature_adjustment: 0.1,
            max_tokens_adjustment: 2000,
            system_prompt_prefix: Some(
                "Think very carefully and exhaustively. Break down the problem \
                 into sub-problems, consider all angles, verify your reasoning, \
                 and provide the most thorough analysis possible."
                    .into(),
            ),
            native_thinking: None,
        },
    }
}

/// Convert a `ThinkingLevel` into parameters, resolving native extended
/// thinking from the provided config.
pub fn apply_thinking_level_with_config(
    level: ThinkingLevel,
    config: &ThinkingConfig,
) -> ThinkingParams {
    use zeroclaw_config::scattered_types::MAX_BUDGET_TOKENS;
    let mut params = apply_thinking_level(level);
    if config.native_thinking {
        if let Some(budget) = config.budget_tokens_for(level) {
            let clamped = budget.min(MAX_BUDGET_TOKENS);
            if clamped < budget {
                tracing::warn!(
                    requested = budget,
                    clamped = clamped,
                    "budget_tokens exceeds maximum; clamping to {MAX_BUDGET_TOKENS}"
                );
            }
            params.native_thinking = Some(zeroclaw_config::scattered_types::NativeThinkingParams {
                budget_tokens: clamped,
            });
        }
    }
    params
}

/// Resolve the effective thinking level using the priority hierarchy:
/// 1. Inline directive (if present)
/// 2. Session override (reserved, currently always `None`)
/// 3. Agent config default
/// 4. Global default (`Medium`)
pub fn resolve_thinking_level(
    inline_directive: Option<ThinkingLevel>,
    session_override: Option<ThinkingLevel>,
    config: &ThinkingConfig,
) -> ThinkingLevel {
    inline_directive
        .or(session_override)
        .unwrap_or(config.default_level)
}

/// Clamp a temperature value to the valid range `[0.0, 2.0]`.
pub fn clamp_temperature(temp: f64) -> f64 {
    temp.clamp(0.0, 2.0)
}

pub struct ResolvedThinking {
    pub effective_message: String,
    pub params: ThinkingParams,
    pub effective_temperature: f64,
}

pub fn resolve_thinking_from_message(
    message: &str,
    config: &ThinkingConfig,
    base_temperature: f64,
) -> ResolvedThinking {
    use std::sync::Once;
    static VALIDATE_ONCE: Once = Once::new();
    VALIDATE_ONCE.call_once(|| config.warn_unknown_budget_keys());

    let (directive, effective_message) = match parse_thinking_directive(message) {
        Some((level, remaining)) => {
            tracing::info!(thinking_level = ?level, "Thinking directive parsed from message");
            (Some(level), remaining)
        }
        None => (None, message.to_string()),
    };
    let level = resolve_thinking_level(directive, None, config);
    let params = apply_thinking_level_with_config(level, config);
    let effective_temperature = clamp_temperature(base_temperature + params.temperature_adjustment);
    ResolvedThinking {
        effective_message,
        params,
        effective_temperature,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ThinkingLevel parsing ────────────────────────────────────

    #[test]
    fn thinking_level_from_str_canonical_names() {
        assert_eq!(
            ThinkingLevel::from_str_insensitive("off"),
            Some(ThinkingLevel::Off)
        );
        assert_eq!(
            ThinkingLevel::from_str_insensitive("minimal"),
            Some(ThinkingLevel::Minimal)
        );
        assert_eq!(
            ThinkingLevel::from_str_insensitive("low"),
            Some(ThinkingLevel::Low)
        );
        assert_eq!(
            ThinkingLevel::from_str_insensitive("medium"),
            Some(ThinkingLevel::Medium)
        );
        assert_eq!(
            ThinkingLevel::from_str_insensitive("high"),
            Some(ThinkingLevel::High)
        );
        assert_eq!(
            ThinkingLevel::from_str_insensitive("max"),
            Some(ThinkingLevel::Max)
        );
    }

    #[test]
    fn thinking_level_from_str_aliases() {
        assert_eq!(
            ThinkingLevel::from_str_insensitive("none"),
            Some(ThinkingLevel::Off)
        );
        assert_eq!(
            ThinkingLevel::from_str_insensitive("min"),
            Some(ThinkingLevel::Minimal)
        );
        assert_eq!(
            ThinkingLevel::from_str_insensitive("med"),
            Some(ThinkingLevel::Medium)
        );
        assert_eq!(
            ThinkingLevel::from_str_insensitive("default"),
            Some(ThinkingLevel::Medium)
        );
        assert_eq!(
            ThinkingLevel::from_str_insensitive("maximum"),
            Some(ThinkingLevel::Max)
        );
    }

    #[test]
    fn thinking_level_from_str_case_insensitive() {
        assert_eq!(
            ThinkingLevel::from_str_insensitive("HIGH"),
            Some(ThinkingLevel::High)
        );
        assert_eq!(
            ThinkingLevel::from_str_insensitive("Max"),
            Some(ThinkingLevel::Max)
        );
        assert_eq!(
            ThinkingLevel::from_str_insensitive("OFF"),
            Some(ThinkingLevel::Off)
        );
    }

    #[test]
    fn thinking_level_from_str_invalid_returns_none() {
        assert_eq!(ThinkingLevel::from_str_insensitive("turbo"), None);
        assert_eq!(ThinkingLevel::from_str_insensitive(""), None);
        assert_eq!(ThinkingLevel::from_str_insensitive("super-high"), None);
    }

    // ── Directive parsing ────────────────────────────────────────

    #[test]
    fn parse_directive_extracts_level_and_remaining_message() {
        let result = parse_thinking_directive("/think:high What is Rust?");
        assert!(result.is_some());
        let (level, remaining) = result.unwrap();
        assert_eq!(level, ThinkingLevel::High);
        assert_eq!(remaining, "What is Rust?");
    }

    #[test]
    fn parse_directive_handles_directive_only() {
        let result = parse_thinking_directive("/think:off");
        assert!(result.is_some());
        let (level, remaining) = result.unwrap();
        assert_eq!(level, ThinkingLevel::Off);
        assert_eq!(remaining, "");
    }

    #[test]
    fn parse_directive_strips_leading_whitespace() {
        let result = parse_thinking_directive("  /think:low  Tell me about Rust");
        assert!(result.is_some());
        let (level, remaining) = result.unwrap();
        assert_eq!(level, ThinkingLevel::Low);
        assert_eq!(remaining, "Tell me about Rust");
    }

    #[test]
    fn parse_directive_returns_none_for_no_directive() {
        assert!(parse_thinking_directive("Hello world").is_none());
        assert!(parse_thinking_directive("").is_none());
        assert!(parse_thinking_directive("/think").is_none());
    }

    #[test]
    fn parse_directive_returns_none_for_invalid_level() {
        assert!(parse_thinking_directive("/think:turbo What?").is_none());
    }

    #[test]
    fn parse_directive_not_triggered_mid_message() {
        assert!(parse_thinking_directive("Hello /think:high world").is_none());
    }

    // ── Level application ────────────────────────────────────────

    #[test]
    fn apply_thinking_level_off_is_concise() {
        let params = apply_thinking_level(ThinkingLevel::Off);
        assert!(params.temperature_adjustment < 0.0);
        assert!(params.max_tokens_adjustment < 0);
        assert!(params.system_prompt_prefix.is_some());
        assert!(
            params
                .system_prompt_prefix
                .unwrap()
                .to_lowercase()
                .contains("concise")
        );
    }

    #[test]
    fn apply_thinking_level_medium_is_neutral() {
        let params = apply_thinking_level(ThinkingLevel::Medium);
        assert!((params.temperature_adjustment - 0.0).abs() < f64::EPSILON);
        assert_eq!(params.max_tokens_adjustment, 0);
        assert!(params.system_prompt_prefix.is_none());
    }

    #[test]
    fn apply_thinking_level_high_adds_step_by_step() {
        let params = apply_thinking_level(ThinkingLevel::High);
        assert!(params.temperature_adjustment > 0.0);
        assert!(params.max_tokens_adjustment > 0);
        let prefix = params.system_prompt_prefix.unwrap();
        assert!(prefix.to_lowercase().contains("step by step"));
    }

    #[test]
    fn apply_thinking_level_max_is_most_thorough() {
        let params = apply_thinking_level(ThinkingLevel::Max);
        assert!(params.temperature_adjustment > 0.0);
        assert!(params.max_tokens_adjustment > 0);
        let prefix = params.system_prompt_prefix.unwrap();
        assert!(prefix.to_lowercase().contains("exhaustively"));
    }

    // ── Resolution hierarchy ─────────────────────────────────────

    #[test]
    fn resolve_inline_directive_takes_priority() {
        let config = ThinkingConfig {
            default_level: ThinkingLevel::Low,
            ..ThinkingConfig::default()
        };
        let result =
            resolve_thinking_level(Some(ThinkingLevel::Max), Some(ThinkingLevel::High), &config);
        assert_eq!(result, ThinkingLevel::Max);
    }

    #[test]
    fn resolve_session_override_takes_priority_over_config() {
        let config = ThinkingConfig {
            default_level: ThinkingLevel::Low,
            ..ThinkingConfig::default()
        };
        let result = resolve_thinking_level(None, Some(ThinkingLevel::High), &config);
        assert_eq!(result, ThinkingLevel::High);
    }

    #[test]
    fn resolve_falls_back_to_config_default() {
        let config = ThinkingConfig {
            default_level: ThinkingLevel::Minimal,
            ..ThinkingConfig::default()
        };
        let result = resolve_thinking_level(None, None, &config);
        assert_eq!(result, ThinkingLevel::Minimal);
    }

    #[test]
    fn resolve_default_config_uses_medium() {
        let config = ThinkingConfig::default();
        let result = resolve_thinking_level(None, None, &config);
        assert_eq!(result, ThinkingLevel::Medium);
    }

    // ── Temperature clamping ─────────────────────────────────────

    #[test]
    fn clamp_temperature_within_range() {
        assert!((clamp_temperature(0.7) - 0.7).abs() < f64::EPSILON);
        assert!((clamp_temperature(0.0) - 0.0).abs() < f64::EPSILON);
        assert!((clamp_temperature(2.0) - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn clamp_temperature_below_minimum() {
        assert!((clamp_temperature(-0.5) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn clamp_temperature_above_maximum() {
        assert!((clamp_temperature(3.0) - 2.0).abs() < f64::EPSILON);
    }

    // ── Serde round-trip ─────────────────────────────────────────

    #[test]
    fn thinking_config_deserializes_from_toml() {
        let toml_str = r#"default_level = "high""#;
        let config: ThinkingConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.default_level, ThinkingLevel::High);
    }

    #[test]
    fn thinking_config_default_level_deserializes() {
        let toml_str = "";
        let config: ThinkingConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.default_level, ThinkingLevel::Medium);
    }

    #[test]
    fn thinking_level_serializes_lowercase() {
        let level = ThinkingLevel::High;
        let json = serde_json::to_string(&level).unwrap();
        assert_eq!(json, "\"high\"");
    }
}
