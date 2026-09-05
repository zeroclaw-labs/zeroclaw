//! What a session can adjust about the model's reasoning, and how the choice
//! for one turn is resolved.
//!
//! RPC turns apply only native parameters: the depth and the display travel
//! on the request, while the prompt prefixes and sampling nudges the CLI and
//! channels add for a level are not applied. Rewriting the system prompt per
//! level would restart the prompt cache and break signed-thinking replay
//! within a tool round. A session is therefore offered a level only where the
//! model reads it natively.

use serde::{Deserialize, Serialize};
use zeroclaw_api::model_provider::{NativeThinkingParams, ThinkingDisplay, ThinkingEffort};
use zeroclaw_config::scattered_types::{ThinkingConfig, ThinkingLevel};
use zeroclaw_providers::claude_models::{
    ClaudeProviderSlot, ClaudeThinkingShape, ThinkingCapabilities, thinking_capabilities,
};

/// Levels a session may name on a model that takes a depth setting, before
/// filtering by what the generation accepts. `off` and `minimal` are left
/// out: on these models they send the same request as `low`, and the prompt
/// prefixes that tell them apart elsewhere do not apply here.
const SESSION_DEPTH_LEVELS: &[ThinkingLevel] = &[
    ThinkingLevel::Low,
    ThinkingLevel::Medium,
    ThinkingLevel::High,
    ThinkingLevel::XHigh,
    ThinkingLevel::Max,
];

/// Levels a session may name on a model that takes a token budget, once the
/// profile enables native thinking: the levels that carry a budget, plus the
/// default that sends none.
const SESSION_BUDGET_LEVELS: &[ThinkingLevel] = &[
    ThinkingLevel::Medium,
    ThinkingLevel::High,
    ThinkingLevel::Max,
];

/// The controls the model behind a `<type>.<alias>` reference accepts.
/// Provider types whose adapters carry no Claude controls accept nothing.
#[must_use]
pub fn capabilities_for(model_provider: &str, model: &str) -> ThinkingCapabilities {
    ClaudeProviderSlot::from_provider_ref(model_provider)
        .map_or(ThinkingCapabilities::NONE, |slot| {
            thinking_capabilities(slot, model)
        })
}

/// The levels a session may choose, given what the model accepts and whether
/// the profile opted into token budgets.
#[must_use]
pub fn accepted_levels(
    capabilities: &ThinkingCapabilities,
    profile: &ThinkingConfig,
) -> Vec<ThinkingLevel> {
    match capabilities.shape {
        ClaudeThinkingShape::Adaptive => SESSION_DEPTH_LEVELS
            .iter()
            .copied()
            .filter(|level| {
                level
                    .native_effort()
                    .is_none_or(|effort| capabilities.supports_effort(effort))
            })
            .collect(),
        ClaudeThinkingShape::FixedBudget
            if capabilities.accepts_budget && profile.native_thinking =>
        {
            SESSION_BUDGET_LEVELS.to_vec()
        }
        ClaudeThinkingShape::FixedBudget => Vec::new(),
    }
}

/// Where the level a turn will use comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LevelSource {
    /// A `session/configure` override.
    Session,
    /// The runtime profile's `default_level`.
    Profile,
    /// The profile names no depth, so the model applies its own default.
    ModelDefault,
}

/// Where the display a turn will use comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplaySource {
    /// A `session/configure` override.
    Session,
    /// The runtime profile's `agent.thinking.display`.
    Profile,
    /// The provider alias's `thinking_display`.
    Alias,
    /// Nothing chose a display, so the API applies its own default.
    ModelDefault,
}

/// What a session can adjust, and what it currently has.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkingOptions {
    pub model_provider: String,
    pub model: String,
    pub levels: Vec<ThinkingLevel>,
    pub displays: Vec<ThinkingDisplay>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_level: Option<ThinkingLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level_source: Option<LevelSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_display: Option<ThinkingDisplay>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_source: Option<DisplaySource>,
}

/// The facts the options are computed from, gathered by the caller under one
/// config read so they describe the same moment.
#[derive(Clone, Copy, Debug)]
pub struct ThinkingContext<'a> {
    pub model_provider: &'a str,
    pub model: &'a str,
    pub profile: &'a ThinkingConfig,
    /// The display the provider alias configured, when the slot has one.
    pub alias_display: Option<ThinkingDisplay>,
    pub session_level: Option<ThinkingLevel>,
    pub session_display: Option<ThinkingDisplay>,
}

/// The options for a session. The current values are present exactly when
/// the matching list is non-empty; a session override is reported as it
/// stands even when a later model change left it outside the list, so the
/// caller can still offer to clear it.
#[must_use]
pub fn thinking_options(context: &ThinkingContext<'_>) -> ThinkingOptions {
    let capabilities = capabilities_for(context.model_provider, context.model);
    let levels = accepted_levels(&capabilities, context.profile);
    let displays = capabilities.displays.to_vec();

    let (current_level, level_source) = if levels.is_empty() {
        (None, None)
    } else if let Some(level) = context.session_level {
        (Some(level), Some(LevelSource::Session))
    } else {
        let level = fit_level(context.profile.default_level, &capabilities);
        // The default level asks the provider for nothing.
        let source = if level == ThinkingLevel::Medium {
            LevelSource::ModelDefault
        } else {
            LevelSource::Profile
        };
        (Some(level), Some(source))
    };

    let (current_display, display_source) = if displays.is_empty() {
        (None, None)
    } else if let Some(display) = context.session_display {
        (Some(display), Some(DisplaySource::Session))
    } else if let Some(display) = context
        .profile
        .display
        .to_display()
        .filter(|display| capabilities.supports_display(*display))
    {
        (Some(display), Some(DisplaySource::Profile))
    } else if let Some(display) = context
        .alias_display
        .filter(|display| capabilities.supports_display(*display))
    {
        (Some(display), Some(DisplaySource::Alias))
    } else {
        (
            Some(ThinkingDisplay::Omitted),
            Some(DisplaySource::ModelDefault),
        )
    };

    ThinkingOptions {
        model_provider: context.model_provider.to_string(),
        model: context.model.to_string(),
        levels,
        displays,
        current_level,
        level_source,
        current_display,
        display_source,
    }
}

/// The level the profile default resolves to on this model: a depth the
/// generation lacks becomes the depth just below, which is what the adapter
/// sends.
fn fit_level(level: ThinkingLevel, capabilities: &ThinkingCapabilities) -> ThinkingLevel {
    match level.native_effort() {
        Some(effort)
            if capabilities.shape == ClaudeThinkingShape::Adaptive
                && !capabilities.supports_effort(effort) =>
        {
            capabilities
                .fit_effort(effort)
                .map_or(level, level_for_effort)
        }
        _ => level,
    }
}

fn level_for_effort(effort: ThinkingEffort) -> ThinkingLevel {
    match effort {
        ThinkingEffort::Low => ThinkingLevel::Low,
        ThinkingEffort::High => ThinkingLevel::High,
        ThinkingEffort::XHigh => ThinkingLevel::XHigh,
        ThinkingEffort::Max => ThinkingLevel::Max,
    }
}

/// The native thinking parameters for one turn. The inline prefix beats the
/// session override, which beats the profile default; the session display
/// beats the profile's `display`, and the provider alias fills in behind
/// both. `None` when nothing asks the provider for anything, which leaves
/// the model to its own defaults.
#[must_use]
pub fn resolve_session_thinking(
    inline_level: Option<ThinkingLevel>,
    session_level: Option<ThinkingLevel>,
    session_display: Option<ThinkingDisplay>,
    profile: &ThinkingConfig,
) -> Option<NativeThinkingParams> {
    let level =
        crate::agent::thinking::resolve_thinking_level(inline_level, session_level, profile);
    let native =
        crate::agent::thinking::apply_thinking_level_with_config(level, profile).native_thinking;
    match (native, session_display) {
        (Some(params), display) => Some(NativeThinkingParams {
            display: display.or(params.display),
            ..params
        }),
        (None, Some(display)) => Some(NativeThinkingParams {
            budget_tokens: None,
            effort: None,
            display: Some(display),
        }),
        (None, None) => None,
    }
}

/// `low, medium, high` for an error message.
#[must_use]
pub fn join_levels(levels: &[ThinkingLevel]) -> String {
    levels
        .iter()
        .map(|level| level.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// `omitted, summarized` for an error message.
#[must_use]
pub fn join_displays(displays: &[ThinkingDisplay]) -> String {
    displays
        .iter()
        .map(|display| display.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ThinkingDisplay::{Omitted, Summarized, Updates};
    use ThinkingLevel::{High, Low, Max, Medium, XHigh};

    fn profile(default_level: ThinkingLevel, native_thinking: bool) -> ThinkingConfig {
        ThinkingConfig {
            default_level,
            native_thinking,
            ..ThinkingConfig::default()
        }
    }

    fn context<'a>(
        model_provider: &'a str,
        model: &'a str,
        profile: &'a ThinkingConfig,
    ) -> ThinkingContext<'a> {
        ThinkingContext {
            model_provider,
            model,
            profile,
            alias_display: None,
            session_level: None,
            session_display: None,
        }
    }

    #[test]
    fn levels_follow_the_generation_and_the_budget_opt_in() {
        let default = profile(Medium, false);
        let native = profile(Medium, true);
        let cases: &[(&str, &str, &ThinkingConfig, &[ThinkingLevel])] = &[
            (
                "anthropic.default",
                "claude-fable-5-1",
                &default,
                &[Low, Medium, High, XHigh, Max],
            ),
            (
                "anthropic.default",
                "claude-opus-4-6",
                &default,
                &[Low, Medium, High, Max],
            ),
            ("anthropic.default", "claude-haiku-4-5", &default, &[]),
            (
                "anthropic.default",
                "claude-haiku-4-5",
                &native,
                &[Medium, High, Max],
            ),
            (
                "bedrock.eu",
                "us.anthropic.claude-opus-4-8-v1",
                &default,
                &[Low, Medium, High, XHigh, Max],
            ),
            ("openai.default", "gpt-4o", &native, &[]),
            (
                "openrouter.default",
                "anthropic/claude-opus-4-8",
                &native,
                &[],
            ),
        ];
        for (model_provider, model, profile, expected) in cases {
            let capabilities = capabilities_for(model_provider, model);
            assert_eq!(
                accepted_levels(&capabilities, profile),
                *expected,
                "{model} on {model_provider}"
            );
        }
    }

    #[test]
    fn options_report_sources_for_level_and_display() {
        let high = profile(High, false);
        let mut ctx = context("anthropic.default", "claude-fable-5-1", &high);
        let options = thinking_options(&ctx);
        assert_eq!(options.levels, vec![Low, Medium, High, XHigh, Max]);
        assert_eq!(options.displays, vec![Omitted, Summarized, Updates]);
        assert_eq!(options.current_level, Some(High));
        assert_eq!(options.level_source, Some(LevelSource::Profile));
        assert_eq!(options.current_display, Some(Omitted));
        assert_eq!(options.display_source, Some(DisplaySource::ModelDefault));

        ctx.alias_display = Some(Summarized);
        let options = thinking_options(&ctx);
        assert_eq!(options.current_display, Some(Summarized));
        assert_eq!(options.display_source, Some(DisplaySource::Alias));

        ctx.session_level = Some(Max);
        ctx.session_display = Some(Updates);
        let options = thinking_options(&ctx);
        assert_eq!(options.current_level, Some(Max));
        assert_eq!(options.level_source, Some(LevelSource::Session));
        assert_eq!(options.current_display, Some(Updates));
        assert_eq!(options.display_source, Some(DisplaySource::Session));

        let medium = profile(Medium, false);
        let options = thinking_options(&context("anthropic.default", "claude-opus-5", &medium));
        assert_eq!(options.current_level, Some(Medium));
        assert_eq!(
            options.level_source,
            Some(LevelSource::ModelDefault),
            "the default level leaves the depth to the model"
        );
        assert_eq!(options.displays, vec![Omitted, Summarized]);
    }

    #[test]
    fn options_report_the_profile_display_between_session_and_alias() {
        use zeroclaw_config::scattered_types::ThinkingDisplayMode;
        let mut profile = profile(High, false);
        profile.display = ThinkingDisplayMode::Summarized;
        let mut ctx = context("anthropic.default", "claude-opus-4-8", &profile);
        ctx.alias_display = Some(Updates);
        let options = thinking_options(&ctx);
        assert_eq!(options.current_display, Some(Summarized));
        assert_eq!(options.display_source, Some(DisplaySource::Profile));

        ctx.session_display = Some(Omitted);
        let options = thinking_options(&ctx);
        assert_eq!(options.current_display, Some(Omitted));
        assert_eq!(options.display_source, Some(DisplaySource::Session));

        // A profile display the generation does not take falls through.
        let mut ctx = context("anthropic.default", "claude-opus-4-6", &profile);
        ctx.alias_display = Some(Updates);
        let options = thinking_options(&ctx);
        assert_eq!(options.current_display, None);
        assert_eq!(options.display_source, None);

        let params = resolve_session_thinking(None, None, None, &profile).unwrap();
        assert_eq!(
            params.display,
            Some(Summarized),
            "the profile display rides on the turn when the session chose none"
        );
        let params = resolve_session_thinking(None, None, Some(Updates), &profile).unwrap();
        assert_eq!(params.display, Some(Updates), "the session display wins");
    }

    #[test]
    fn options_are_empty_where_nothing_is_adjustable() {
        let high = profile(High, false);
        let options = thinking_options(&context("openai.default", "gpt-4o", &high));
        assert!(options.levels.is_empty());
        assert!(options.displays.is_empty());
        assert_eq!(options.current_level, None);
        assert_eq!(options.level_source, None);
        assert_eq!(options.current_display, None);
        assert_eq!(options.display_source, None);
        assert_eq!(options.model_provider, "openai.default");
        assert_eq!(options.model, "gpt-4o");
    }

    #[test]
    fn options_drop_displays_and_fit_the_profile_level_on_the_4_6_generation() {
        let xhigh = profile(XHigh, false);
        let mut ctx = context("anthropic.default", "claude-opus-4-6", &xhigh);
        ctx.alias_display = Some(Summarized);
        let options = thinking_options(&ctx);
        assert_eq!(options.levels, vec![Low, Medium, High, Max]);
        assert!(options.displays.is_empty());
        assert_eq!(
            options.current_level,
            Some(High),
            "the profile's xhigh is sent as high on this generation"
        );
        assert_eq!(options.level_source, Some(LevelSource::Profile));
        assert_eq!(options.current_display, None);
        assert_eq!(options.display_source, None);
    }

    #[test]
    fn options_on_bedrock_offer_depths_but_no_display() {
        let high = profile(High, false);
        let options = thinking_options(&context(
            "bedrock.eu",
            "us.anthropic.claude-fable-5-1",
            &high,
        ));
        assert_eq!(options.levels, vec![Low, Medium, High, XHigh, Max]);
        assert!(options.displays.is_empty());
        assert_eq!(options.current_display, None);
    }

    #[test]
    fn options_keep_a_stale_session_level_so_it_can_be_cleared() {
        let high = profile(High, false);
        let mut ctx = context("anthropic.default", "claude-opus-4-6", &high);
        ctx.session_level = Some(XHigh);
        let options = thinking_options(&ctx);
        assert_eq!(options.current_level, Some(XHigh));
        assert_eq!(options.level_source, Some(LevelSource::Session));
    }

    #[test]
    fn serialized_options_match_the_wire_contract() {
        let high = profile(High, false);
        let mut ctx = context("anthropic.default", "claude-fable-5-1", &high);
        ctx.session_display = Some(Summarized);
        let json = serde_json::to_value(thinking_options(&ctx)).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "model_provider": "anthropic.default",
                "model": "claude-fable-5-1",
                "levels": ["low", "medium", "high", "xhigh", "max"],
                "displays": ["omitted", "summarized", "updates"],
                "current_level": "high",
                "level_source": "profile",
                "current_display": "summarized",
                "display_source": "session"
            })
        );
        let empty = serde_json::to_value(thinking_options(&context(
            "openai.default",
            "gpt-4o",
            &high,
        )))
        .unwrap();
        assert_eq!(
            empty,
            serde_json::json!({
                "model_provider": "openai.default",
                "model": "gpt-4o",
                "levels": [],
                "displays": []
            })
        );
    }

    #[test]
    fn turn_params_follow_inline_then_session_then_profile() {
        let high = profile(High, false);
        let params = resolve_session_thinking(Some(Max), Some(Low), None, &high)
            .expect("a chosen depth carries params");
        assert_eq!(params.effort, Some(ThinkingEffort::Max));
        assert_eq!(params.display, None);

        let params = resolve_session_thinking(None, Some(Low), Some(Updates), &high).unwrap();
        assert_eq!(params.effort, Some(ThinkingEffort::Low));
        assert_eq!(params.display, Some(Updates));

        let params = resolve_session_thinking(None, None, None, &high).unwrap();
        assert_eq!(params.effort, Some(ThinkingEffort::High));
        assert_eq!(params.budget_tokens, None, "budgets stay opt-in");
    }

    #[test]
    fn turn_params_carry_a_display_alone_and_nothing_when_nothing_was_chosen() {
        let medium = profile(Medium, false);
        assert_eq!(resolve_session_thinking(None, None, None, &medium), None);
        let params = resolve_session_thinking(None, None, Some(Summarized), &medium).unwrap();
        assert_eq!(params.effort, None);
        assert_eq!(params.budget_tokens, None);
        assert_eq!(params.display, Some(Summarized));
        // An explicit medium behaves like the default level.
        assert_eq!(
            resolve_session_thinking(None, Some(Medium), None, &profile(High, false)),
            None
        );
    }

    #[test]
    fn turn_params_carry_the_budget_where_the_profile_opted_in() {
        let native = profile(High, true);
        let params = resolve_session_thinking(None, Some(Max), None, &native).unwrap();
        assert_eq!(params.budget_tokens, Some(50_000));
        assert_eq!(params.effort, Some(ThinkingEffort::Max));
    }

    #[test]
    fn lists_join_for_error_messages() {
        assert_eq!(join_levels(&[Low, Medium, High]), "low, medium, high");
        assert_eq!(join_displays(&[Omitted, Updates]), "omitted, updates");
        assert_eq!(join_levels(&[]), "");
    }
}
