use crate::config::{
    AckReactionChatType, AckReactionConfig, AckReactionRuleConfig, AckReactionStrategy,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckReactionContextChatType {
    Direct,
    Group,
}

#[derive(Debug, Clone, Copy)]
pub struct AckReactionContext<'a> {
    pub text: &'a str,
    pub sender_id: Option<&'a str>,
    pub chat_type: AckReactionContextChatType,
    pub locale_hint: Option<&'a str>,
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

    true
}

fn rule_matches(rule: &AckReactionRuleConfig, ctx: &AckReactionContext<'_>) -> bool {
    rule.enabled
        && matches_chat_type(rule, ctx.chat_type)
        && matches_sender(rule, ctx.sender_id)
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

pub fn select_ack_reaction(
    policy: Option<&AckReactionConfig>,
    defaults: &[&str],
    ctx: &AckReactionContext<'_>,
) -> Option<String> {
    let enabled = policy.is_none_or(|cfg| cfg.enabled);
    if !enabled {
        return None;
    }

    let default_strategy = policy.map_or(AckReactionStrategy::Random, |cfg| cfg.strategy);

    if let Some(cfg) = policy {
        for rule in &cfg.rules {
            if !rule_matches(rule, ctx) {
                continue;
            }

            let rule_pool = normalize_entries(&rule.emojis);
            if rule_pool.is_empty() {
                continue;
            }

            let strategy = rule.strategy.unwrap_or(default_strategy);
            if let Some(picked) = pick_from_pool(&rule_pool, strategy) {
                return Some(picked);
            }
        }
    }

    let fallback_pool = policy
        .map(|cfg| normalize_entries(&cfg.emojis))
        .filter(|pool| !pool.is_empty())
        .unwrap_or_else(|| default_pool(defaults));

    pick_from_pool(&fallback_pool, default_strategy)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> AckReactionContext<'static> {
        AckReactionContext {
            text: "Deploy succeeded in group chat",
            sender_id: Some("u123"),
            chat_type: AckReactionContextChatType::Group,
            locale_hint: Some("en_us"),
        }
    }

    #[test]
    fn disabled_policy_returns_none() {
        let cfg = AckReactionConfig {
            enabled: false,
            strategy: AckReactionStrategy::Random,
            emojis: vec!["✅".into()],
            rules: Vec::new(),
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
            enabled: true,
            strategy: AckReactionStrategy::First,
            emojis: vec!["🔥".into(), "✅".into()],
            rules: Vec::new(),
        };
        assert_eq!(
            select_ack_reaction(Some(&cfg), &["👍"], &ctx()).as_deref(),
            Some("🔥")
        );
    }

    #[test]
    fn rule_matches_chat_type_and_keyword() {
        let rule = AckReactionRuleConfig {
            enabled: true,
            contains_any: vec!["deploy".into()],
            contains_all: Vec::new(),
            sender_ids: Vec::new(),
            chat_types: vec![AckReactionChatType::Group],
            locale_any: Vec::new(),
            strategy: Some(AckReactionStrategy::First),
            emojis: vec!["🚀".into()],
        };
        let cfg = AckReactionConfig {
            enabled: true,
            strategy: AckReactionStrategy::Random,
            emojis: vec!["👍".into()],
            rules: vec![rule],
        };
        assert_eq!(
            select_ack_reaction(Some(&cfg), &["👍"], &ctx()).as_deref(),
            Some("🚀")
        );
    }

    #[test]
    fn rule_respects_sender_and_locale_filters() {
        let rule = AckReactionRuleConfig {
            enabled: true,
            contains_any: Vec::new(),
            contains_all: Vec::new(),
            sender_ids: vec!["u999".into()],
            chat_types: Vec::new(),
            locale_any: vec!["zh".into()],
            strategy: Some(AckReactionStrategy::First),
            emojis: vec!["🇨🇳".into()],
        };
        let cfg = AckReactionConfig {
            enabled: true,
            strategy: AckReactionStrategy::Random,
            emojis: vec!["👍".into()],
            rules: vec![rule],
        };
        assert_eq!(
            select_ack_reaction(Some(&cfg), &["👍"], &ctx()).as_deref(),
            Some("👍")
        );
    }
}
