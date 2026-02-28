use crate::config::{
    AckReactionChatType, AckReactionConfig, AckReactionRuleAction, AckReactionRuleConfig,
    AckReactionStrategy,
};
use regex::RegexBuilder;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckReactionContextChatType {
    Direct,
    Group,
}

#[derive(Debug, Clone, Copy)]
pub struct AckReactionContext<'a> {
    pub text: &'a str,
    pub sender_id: Option<&'a str>,
    pub chat_id: Option<&'a str>,
    pub chat_type: AckReactionContextChatType,
    pub locale_hint: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AckReactionSelectionSource {
    Rule(usize),
    ChannelPool,
    DefaultPool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AckReactionSelection {
    pub emoji: Option<String>,
    pub matched_rule_index: Option<usize>,
    pub suppressed: bool,
    pub source: Option<AckReactionSelectionSource>,
}

#[allow(clippy::cast_possible_truncation)]
fn pick_uniform_index(len: usize) -> usize {
    debug_assert!(len > 0);
    let upper = len as u64;
    let reject_threshold = (u64::MAX / upper) * upper;

    loop {
        let value = rand::random::<u64>();
        if value < reject_threshold {
            return (value % upper) as usize;
        }
    }
}

fn normalize_entries(entries: &[String]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| entry.trim())
        .filter(|entry| !entry.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn matches_chat_type(rule: &AckReactionRuleConfig, chat_type: AckReactionContextChatType) -> bool {
    if rule.chat_types.is_empty() {
        return true;
    }

    let wanted = match chat_type {
        AckReactionContextChatType::Direct => AckReactionChatType::Direct,
        AckReactionContextChatType::Group => AckReactionChatType::Group,
    };
    rule.chat_types.iter().any(|candidate| *candidate == wanted)
}

fn matches_sender(rule: &AckReactionRuleConfig, sender_id: Option<&str>) -> bool {
    if rule.sender_ids.is_empty() {
        return true;
    }

    let normalized_sender = sender_id.map(str::trim).filter(|value| !value.is_empty());
    rule.sender_ids.iter().any(|candidate| {
        let candidate = candidate.trim();
        if candidate == "*" {
            return true;
        }
        normalized_sender.is_some_and(|sender| sender == candidate)
    })
}

fn matches_chat_id(rule: &AckReactionRuleConfig, chat_id: Option<&str>) -> bool {
    if rule.chat_ids.is_empty() {
        return true;
    }

    let normalized_chat = chat_id.map(str::trim).filter(|value| !value.is_empty());
    rule.chat_ids.iter().any(|candidate| {
        let candidate = candidate.trim();
        if candidate == "*" {
            return true;
        }
        normalized_chat.is_some_and(|chat| chat == candidate)
    })
}

fn normalize_locale(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('-', "_")
}

fn locale_matches(rule_locale: &str, actual_locale: &str) -> bool {
    let rule_locale = normalize_locale(rule_locale);
    if rule_locale.is_empty() {
        return false;
    }
    if rule_locale == "*" {
        return true;
    }

    let actual_locale = normalize_locale(actual_locale);
    actual_locale == rule_locale || actual_locale.starts_with(&(rule_locale + "_"))
}

fn matches_locale(rule: &AckReactionRuleConfig, locale_hint: Option<&str>) -> bool {
    if rule.locale_any.is_empty() {
        return true;
    }

    let Some(actual_locale) = locale_hint.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    rule.locale_any
        .iter()
        .any(|candidate| locale_matches(candidate, actual_locale))
}

fn contains_keyword(text: &str, keyword: &str) -> bool {
    text.contains(&keyword.to_ascii_lowercase())
}

fn regex_is_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return false;
    }

    match RegexBuilder::new(pattern).case_insensitive(true).build() {
        Ok(regex) => regex.is_match(text),
        Err(error) => {
            tracing::warn!(
                pattern = pattern,
                "Invalid ACK reaction regex pattern: {error}"
            );
            false
        }
    }
}

fn matches_text(rule: &AckReactionRuleConfig, text: &str) -> bool {
    let normalized = text.to_ascii_lowercase();

    if !rule.contains_any.is_empty()
        && !rule
            .contains_any
            .iter()
            .map(String::as_str)
            .map(str::trim)
            .filter(|keyword| !keyword.is_empty())
            .any(|keyword| contains_keyword(&normalized, keyword))
    {
        return false;
    }

    if !rule
        .contains_all
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .filter(|keyword| !keyword.is_empty())
        .all(|keyword| contains_keyword(&normalized, keyword))
    {
        return false;
    }

    if rule
        .contains_none
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .filter(|keyword| !keyword.is_empty())
        .any(|keyword| contains_keyword(&normalized, keyword))
    {
        return false;
    }

    if !rule.regex_any.is_empty()
        && !rule
            .regex_any
            .iter()
            .map(String::as_str)
            .map(str::trim)
            .filter(|pattern| !pattern.is_empty())
            .any(|pattern| regex_is_match(pattern, text))
    {
        return false;
    }

    if !rule
        .regex_all
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty())
        .all(|pattern| regex_is_match(pattern, text))
    {
        return false;
    }

    if rule
        .regex_none
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty())
        .any(|pattern| regex_is_match(pattern, text))
    {
        return false;
    }

    true
}

fn rule_matches(rule: &AckReactionRuleConfig, ctx: &AckReactionContext<'_>) -> bool {
    rule.enabled
        && matches_chat_type(rule, ctx.chat_type)
        && matches_sender(rule, ctx.sender_id)
        && matches_chat_id(rule, ctx.chat_id)
        && matches_locale(rule, ctx.locale_hint)
        && matches_text(rule, ctx.text)
}

fn pick_from_pool(pool: &[String], strategy: AckReactionStrategy) -> Option<String> {
    if pool.is_empty() {
        return None;
    }
    match strategy {
        AckReactionStrategy::Random => Some(pool[pick_uniform_index(pool.len())].clone()),
        AckReactionStrategy::First => pool.first().cloned(),
    }
}

fn default_pool(defaults: &[&str]) -> Vec<String> {
    defaults
        .iter()
        .map(|emoji| emoji.trim())
        .filter(|emoji| !emoji.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn normalize_sample_rate(rate: f64) -> f64 {
    if rate.is_finite() {
        rate.clamp(0.0, 1.0)
    } else {
        1.0
    }
}

fn passes_sample_rate(rate: f64) -> bool {
    let rate = normalize_sample_rate(rate);
    if rate <= 0.0 {
        return false;
    }
    if rate >= 1.0 {
        return true;
    }
    rand::random::<f64>() < rate
}

pub fn select_ack_reaction(
    policy: Option<&AckReactionConfig>,
    defaults: &[&str],
    ctx: &AckReactionContext<'_>,
) -> Option<String> {
    select_ack_reaction_with_trace(policy, defaults, ctx).emoji
}

pub fn select_ack_reaction_with_trace(
    policy: Option<&AckReactionConfig>,
    defaults: &[&str],
    ctx: &AckReactionContext<'_>,
) -> AckReactionSelection {
    let enabled = policy.is_none_or(|cfg| cfg.enabled);
    if !enabled {
        return AckReactionSelection {
            emoji: None,
            matched_rule_index: None,
            suppressed: false,
            source: None,
        };
    }

    let default_strategy = policy.map_or(AckReactionStrategy::Random, |cfg| cfg.strategy);
    let default_sample_rate = policy.map_or(1.0, |cfg| cfg.sample_rate);

    if let Some(cfg) = policy {
        for (index, rule) in cfg.rules.iter().enumerate() {
            if !rule_matches(rule, ctx) {
                continue;
            }

            let effective_sample_rate = rule.sample_rate.unwrap_or(default_sample_rate);
            if !passes_sample_rate(effective_sample_rate) {
                continue;
            }

            if rule.action == AckReactionRuleAction::Suppress {
                return AckReactionSelection {
                    emoji: None,
                    matched_rule_index: Some(index),
                    suppressed: true,
                    source: Some(AckReactionSelectionSource::Rule(index)),
                };
            }

            let rule_pool = normalize_entries(&rule.emojis);
            if rule_pool.is_empty() {
                continue;
            }

            let strategy = rule.strategy.unwrap_or(default_strategy);
            if let Some(picked) = pick_from_pool(&rule_pool, strategy) {
                return AckReactionSelection {
                    emoji: Some(picked),
                    matched_rule_index: Some(index),
                    suppressed: false,
                    source: Some(AckReactionSelectionSource::Rule(index)),
                };
            }
        }
    }

    if !passes_sample_rate(default_sample_rate) {
        return AckReactionSelection {
            emoji: None,
            matched_rule_index: None,
            suppressed: false,
            source: None,
        };
    }

    let maybe_channel_pool = policy
        .map(|cfg| normalize_entries(&cfg.emojis))
        .filter(|pool| !pool.is_empty());
    let (fallback_pool, source) = if let Some(channel_pool) = maybe_channel_pool {
        (channel_pool, AckReactionSelectionSource::ChannelPool)
    } else {
        (
            default_pool(defaults),
            AckReactionSelectionSource::DefaultPool,
        )
    };

    AckReactionSelection {
        emoji: pick_from_pool(&fallback_pool, default_strategy),
        matched_rule_index: None,
        suppressed: false,
        source: Some(source),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> AckReactionContext<'static> {
        AckReactionContext {
            text: "Deploy succeeded in group chat",
            sender_id: Some("u123"),
            chat_id: Some("-100200300"),
            chat_type: AckReactionContextChatType::Group,
            locale_hint: Some("en_us"),
        }
    }

    #[test]
    fn disabled_policy_returns_none() {
        let cfg = AckReactionConfig {
            enabled: false,
            emojis: vec!["✅".into()],
            ..AckReactionConfig::default()
        };
        assert_eq!(select_ack_reaction(Some(&cfg), &["👍"], &ctx()), None);
    }

    #[test]
    fn falls_back_to_defaults_when_no_override() {
        let picked = select_ack_reaction(None, &["👍"], &ctx());
        assert_eq!(picked.as_deref(), Some("👍"));
    }

    #[test]
    fn first_strategy_uses_first_emoji() {
        let cfg = AckReactionConfig {
            strategy: AckReactionStrategy::First,
            emojis: vec!["🔥".into(), "✅".into()],
            ..AckReactionConfig::default()
        };
        assert_eq!(
            select_ack_reaction(Some(&cfg), &["👍"], &ctx()).as_deref(),
            Some("🔥")
        );
    }

    #[test]
    fn rule_matches_chat_type_and_keyword() {
        let rule = AckReactionRuleConfig {
            contains_any: vec!["deploy".into()],
            chat_types: vec![AckReactionChatType::Group],
            strategy: Some(AckReactionStrategy::First),
            emojis: vec!["🚀".into()],
            ..AckReactionRuleConfig::default()
        };
        let cfg = AckReactionConfig {
            emojis: vec!["👍".into()],
            rules: vec![rule],
            ..AckReactionConfig::default()
        };
        assert_eq!(
            select_ack_reaction(Some(&cfg), &["👍"], &ctx()).as_deref(),
            Some("🚀")
        );
    }

    #[test]
    fn rule_respects_sender_and_locale_filters() {
        let rule = AckReactionRuleConfig {
            sender_ids: vec!["u999".into()],
            locale_any: vec!["zh".into()],
            strategy: Some(AckReactionStrategy::First),
            emojis: vec!["🇨🇳".into()],
            ..AckReactionRuleConfig::default()
        };
        let cfg = AckReactionConfig {
            emojis: vec!["👍".into()],
            rules: vec![rule],
            ..AckReactionConfig::default()
        };
        assert_eq!(
            select_ack_reaction(Some(&cfg), &["👍"], &ctx()).as_deref(),
            Some("👍")
        );
    }

    #[test]
    fn rule_respects_chat_id_filter() {
        let rule = AckReactionRuleConfig {
            contains_any: vec!["deploy".into()],
            chat_ids: vec!["chat-other".into()],
            strategy: Some(AckReactionStrategy::First),
            emojis: vec!["🔒".into()],
            ..AckReactionRuleConfig::default()
        };
        let cfg = AckReactionConfig {
            emojis: vec!["👍".into()],
            rules: vec![rule],
            ..AckReactionConfig::default()
        };
        assert_eq!(
            select_ack_reaction(Some(&cfg), &["👍"], &ctx()).as_deref(),
            Some("👍")
        );
    }

    #[test]
    fn rule_can_suppress_reaction() {
        let rule = AckReactionRuleConfig {
            contains_any: vec!["deploy".into()],
            action: AckReactionRuleAction::Suppress,
            ..AckReactionRuleConfig::default()
        };
        let cfg = AckReactionConfig {
            emojis: vec!["👍".into()],
            rules: vec![rule],
            ..AckReactionConfig::default()
        };
        let selected = select_ack_reaction_with_trace(Some(&cfg), &["✅"], &ctx());
        assert_eq!(selected.emoji, None);
        assert!(selected.suppressed);
        assert_eq!(selected.matched_rule_index, Some(0));
    }

    #[test]
    fn contains_none_blocks_keyword_match() {
        let rule = AckReactionRuleConfig {
            contains_any: vec!["deploy".into()],
            contains_none: vec!["succeeded".into()],
            emojis: vec!["🚀".into()],
            ..AckReactionRuleConfig::default()
        };
        let cfg = AckReactionConfig {
            emojis: vec!["👍".into()],
            rules: vec![rule],
            ..AckReactionConfig::default()
        };
        assert_eq!(
            select_ack_reaction(Some(&cfg), &["✅"], &ctx()).as_deref(),
            Some("👍")
        );
    }

    #[test]
    fn regex_filters_are_supported() {
        let rule = AckReactionRuleConfig {
            regex_any: vec![r"deploy\s+succeeded".into()],
            regex_none: vec![r"panic|fatal".into()],
            strategy: Some(AckReactionStrategy::First),
            emojis: vec!["🧪".into(), "🚀".into()],
            ..AckReactionRuleConfig::default()
        };
        let cfg = AckReactionConfig {
            rules: vec![rule],
            ..AckReactionConfig::default()
        };
        assert_eq!(
            select_ack_reaction(Some(&cfg), &["✅"], &ctx()).as_deref(),
            Some("🧪")
        );
    }

    #[test]
    fn sample_rate_zero_disables_fallback_reaction() {
        let cfg = AckReactionConfig {
            sample_rate: 0.0,
            emojis: vec!["✅".into()],
            ..AckReactionConfig::default()
        };
        assert_eq!(select_ack_reaction(Some(&cfg), &["👍"], &ctx()), None);
    }
}
