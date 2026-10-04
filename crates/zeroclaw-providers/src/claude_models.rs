//! Claude model identity helpers shared by the Anthropic and Bedrock adapters.

/// Which thinking request shape a Claude model accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeThinkingShape {
    /// Extended thinking is requested with `type: "enabled"` and a token
    /// budget, and the request must pin the sampling temperature to 1.0.
    FixedBudget,
    /// Thinking is adaptive: the request may say `type: "adaptive"` and steer
    /// depth with `output_config.effort`; a fixed budget is rejected, and so
    /// is a temperature other than 1 while thinking is active.
    Adaptive,
}

/// Classify a model id by the Claude generation it names.
///
/// Generations before 4.6 keep the fixed budget. Generation 4.6 and later,
/// and any Claude id whose version cannot be read, are adaptive, so a new
/// release needs no code change here. Ids that are not Claude models at all
/// keep the fixed budget, which is the shape Anthropic-compatible proxies
/// accepted before this classification existed.
#[must_use]
pub fn claude_thinking_shape(model: &str) -> ClaudeThinkingShape {
    match claude_id(model) {
        ClaudeId::NotClaude => ClaudeThinkingShape::FixedBudget,
        ClaudeId::Generation { version, .. } if version < (4, 6) => {
            ClaudeThinkingShape::FixedBudget
        }
        ClaudeId::Generation { .. } | ClaudeId::Unversioned => ClaudeThinkingShape::Adaptive,
    }
}

/// Existing chat-completions gateway serialization contract. This intentionally
/// preserves its case-sensitive substring eligibility instead of applying the
/// broader native-adapter capability policy to an opted-in gateway route.
#[must_use]
pub(crate) fn compatible_claude_thinking_shape(model: &str) -> ClaudeThinkingShape {
    if model.contains("claude-opus-4-7") || model.contains("claude-fable-5") {
        ClaudeThinkingShape::Adaptive
    } else {
        ClaudeThinkingShape::FixedBudget
    }
}

/// Whether a Claude model accepts `updates` as its thinking display value.
///
/// Generation 5.1 narrowed the display values to `summarized` and `omitted`.
/// Earlier generations keep accepting `updates`, and so do ids that are not
/// Claude models, whose wire contract this module does not know. A Claude id
/// whose version cannot be read follows the newest generation, the same rule
/// `claude_thinking_shape` applies.
#[must_use]
pub fn claude_accepts_display_updates(model: &str) -> bool {
    match claude_id(model) {
        ClaudeId::NotClaude => true,
        ClaudeId::Generation { version, .. } => version < (5, 1),
        ClaudeId::Unversioned => false,
    }
}

/// Whether signed thinking from completed turns remains part of the prompt.
/// Unknown models retain their records: request-shape support alone does not
/// establish that discarding reasoning is safe.
#[must_use]
pub fn claude_keeps_prior_reasoning(model: &str) -> bool {
    match claude_id(model) {
        ClaudeId::Generation {
            family: ClaudeFamily::Opus,
            version,
        } => version >= (4, 5),
        ClaudeId::Generation {
            family: ClaudeFamily::Sonnet,
            version,
        } => version >= (4, 6),
        ClaudeId::Generation {
            family: ClaudeFamily::Haiku,
            version,
        } if version <= (4, 5) => false,
        _ => true,
    }
}

/// Whether a known model binds thinking to its preceding prompt and permits
/// removing invalidated blocks after a real prefix rewrite. This is independent
/// of adaptive requests and prior-turn retention; in particular 4.6 models
/// require their in-flight blocks. Unknown/future contracts preserve records.
#[must_use]
pub fn claude_invalidates_reasoning_on_prefix_rewrite(model: &str) -> bool {
    matches!(
        claude_id(model),
        ClaudeId::Generation {
            family: ClaudeFamily::Fable,
            version: (5, 1)
        } | ClaudeId::Generation {
            family: ClaudeFamily::Opus | ClaudeFamily::Sonnet,
            version: (5, 5)
        }
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClaudeFamily {
    Opus,
    Sonnet,
    Haiku,
    Fable,
    Mythos,
    Unknown,
}

/// What a model id says about the Claude generation it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClaudeId {
    /// The id does not name a Claude model.
    NotClaude,
    /// A Claude id whose generation cannot be read.
    Unversioned,
    /// A Claude id naming this `(major, minor)` generation.
    Generation {
        family: ClaudeFamily,
        version: (u32, u32),
    },
}

/// Anchors on the `claude-` substring so Bedrock ids carrying region and
/// vendor prefixes resolve the same way as bare API ids.
fn claude_id(model: &str) -> ClaudeId {
    let lower = model.to_ascii_lowercase();
    let Some(start) = lower.find("claude-") else {
        return ClaudeId::NotClaude;
    };
    let rest = &lower[start + "claude-".len()..];
    let family = rest
        .split('-')
        .find_map(|token| match token {
            "opus" => Some(ClaudeFamily::Opus),
            "sonnet" => Some(ClaudeFamily::Sonnet),
            "haiku" => Some(ClaudeFamily::Haiku),
            "fable" => Some(ClaudeFamily::Fable),
            "mythos" => Some(ClaudeFamily::Mythos),
            _ => None,
        })
        .unwrap_or(ClaudeFamily::Unknown);
    claude_generation(rest).map_or(ClaudeId::Unversioned, |version| ClaudeId::Generation {
        family,
        version,
    })
}

/// Read the `(major, minor)` generation from the id tokens after `claude-`.
///
/// The first short all-digit token is the major version and the token right
/// after it, when it is also a short all-digit token, is the minor version.
/// Date stamps and revision suffixes are longer or contain letters, so they
/// never read as a version. Legacy ids spell the generation as `major.minor`
/// in a single token.
fn claude_generation(rest: &str) -> Option<(u32, u32)> {
    let mut tokens = rest.split('-').filter(|token| !token.is_empty());
    while let Some(token) = tokens.next() {
        if let Some((major, minor)) = token.split_once('.')
            && let (Some(major), Some(minor)) = (short_number(major), short_number(minor))
        {
            return Some((major, minor));
        }
        if let Some(major) = short_number(token) {
            let minor = tokens.next().and_then(short_number).unwrap_or(0);
            return Some((major, minor));
        }
    }
    None
}

fn short_number(token: &str) -> Option<u32> {
    (!token.is_empty() && token.len() < 4 && token.bytes().all(|b| b.is_ascii_digit()))
        .then(|| token.parse().ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_policy_is_independent_of_request_shape() {
        for model in [
            "claude-opus-4-5",
            "claude-sonnet-4-6",
            "claude-opus-4-6",
            "claude-fable-5-1",
            "claude-mythos-preview",
            "claude-next",
            "other",
        ] {
            assert!(claude_keeps_prior_reasoning(model), "{model}");
        }
        for model in [
            "claude-sonnet-4-5",
            "claude-haiku-4-5",
            "claude-opus-4-1",
            "anthropic.claude-3-7-sonnet-20250219-v1:0",
        ] {
            assert!(!claude_keeps_prior_reasoning(model), "{model}");
        }
        for model in [
            "claude-fable-5-1",
            "global.anthropic.claude-opus-5-5-v1",
            "claude-sonnet-5-5",
        ] {
            assert!(
                claude_invalidates_reasoning_on_prefix_rewrite(model),
                "{model}"
            );
        }
        for model in [
            "claude-sonnet-4-6",
            "claude-opus-4-6",
            "claude-fable-5",
            "claude-mythos-5-1",
            "claude-fable-6",
            "claude-next",
            "other",
        ] {
            assert!(
                !claude_invalidates_reasoning_on_prefix_rewrite(model),
                "{model}"
            );
        }
    }

    #[test]
    fn adaptive_generations_classify_as_adaptive() {
        for model in [
            "claude-fable-5-1",
            "claude-fable-5",
            "claude-mythos-5-1",
            "claude-opus-5",
            "claude-opus-4-8",
            "claude-opus-4-7-20260101",
            "claude-sonnet-5",
            "claude-opus-4-6",
            "claude-sonnet-4-6",
            "Claude-Sonnet-4-6",
            "anthropic.claude-fable-5-1",
            "us.anthropic.claude-opus-4-8-v1",
            "global.anthropic.claude-sonnet-4-6-v1",
        ] {
            assert_eq!(
                claude_thinking_shape(model),
                ClaudeThinkingShape::Adaptive,
                "{model} should be adaptive"
            );
        }
    }

    #[test]
    fn fixed_budget_generations_classify_as_fixed_budget() {
        for model in [
            "claude-haiku-4-5",
            "claude-sonnet-4-5-20250929",
            "claude-opus-4-5",
            "claude-opus-4-1-20250805",
            "claude-sonnet-4-20250514",
            "claude-opus-4-20250514",
            "claude-3-7-sonnet-20250219",
            "claude-3-5-haiku-20241022",
            "anthropic.claude-3-5-haiku-20241022-v1:0",
            "us.anthropic.claude-haiku-4-5-v1",
            "claude-2.1",
            "claude-instant-1.2",
        ] {
            assert_eq!(
                claude_thinking_shape(model),
                ClaudeThinkingShape::FixedBudget,
                "{model} should use a fixed budget"
            );
        }
    }

    #[test]
    fn unversioned_claude_ids_are_adaptive() {
        assert_eq!(
            claude_thinking_shape("claude-next"),
            ClaudeThinkingShape::Adaptive
        );
    }

    #[test]
    fn non_claude_ids_keep_the_fixed_budget_shape() {
        for model in ["gpt-4o", "minimax-m2", "glm-4.7", ""] {
            assert_eq!(
                claude_thinking_shape(model),
                ClaudeThinkingShape::FixedBudget,
                "{model} is not a Claude id"
            );
        }
    }

    #[test]
    fn display_updates_is_refused_from_generation_5_1() {
        for model in [
            "claude-fable-5-1",
            "claude-fable-5-1-20260815",
            "claude-mythos-5-1",
            "claude-opus-5-2",
            "claude-sonnet-6",
            "anthropic.claude-fable-5-1",
            "us.anthropic.claude-mythos-5-1-v1",
        ] {
            assert!(
                !claude_accepts_display_updates(model),
                "{model} should not take the updates display"
            );
        }
    }

    #[test]
    fn display_updates_is_accepted_before_generation_5_1() {
        for model in [
            "claude-fable-5",
            "claude-opus-5",
            "claude-sonnet-5",
            "claude-opus-4-8",
            "claude-opus-4-7-20260101",
            "claude-sonnet-4-6",
            "claude-sonnet-4-5",
            "us.anthropic.claude-opus-4-8-v1",
        ] {
            assert!(
                claude_accepts_display_updates(model),
                "{model} should take the updates display"
            );
        }
    }

    #[test]
    fn unversioned_claude_ids_refuse_display_updates() {
        assert!(!claude_accepts_display_updates("claude-next"));
    }

    #[test]
    fn non_claude_ids_keep_display_updates() {
        for model in ["gpt-4o", "minimax-m2", ""] {
            assert!(
                claude_accepts_display_updates(model),
                "{model} is not a Claude id"
            );
        }
    }

    #[test]
    fn date_and_revision_suffixes_never_read_as_a_version() {
        assert_eq!(claude_generation("sonnet-4-20250514"), Some((4, 0)));
        assert_eq!(claude_generation("opus-4-8-v1"), Some((4, 8)));
        assert_eq!(claude_generation("3-5-haiku-20241022-v1:0"), Some((3, 5)));
        assert_eq!(claude_generation("next"), None);
    }
}
