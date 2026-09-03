//! Claude model identity helpers shared by the Anthropic and Bedrock adapters.

/// Which thinking request shape a Claude model accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeThinkingShape {
    /// Extended thinking is requested with `type: "enabled"` and a token
    /// budget, and the request must pin the sampling temperature to 1.0.
    FixedBudget,
    /// Thinking is adaptive: the request may say `type: "adaptive"` and steer
    /// depth with `output_config.effort`; a fixed budget and any sampling
    /// parameter are rejected.
    Adaptive,
}

/// Classify a model id by the Claude generation it names.
///
/// Anchors on the `claude-` substring so Bedrock ids carrying region and
/// vendor prefixes resolve the same way as bare API ids. Generations before
/// 4.6 keep the fixed budget. Generation 4.6 and later, and any Claude id
/// whose version cannot be read, are adaptive, so a new release needs no code
/// change here. Ids that are not Claude models at all keep the fixed budget,
/// which is the shape Anthropic-compatible proxies accepted before this
/// classification existed.
#[must_use]
pub fn claude_thinking_shape(model: &str) -> ClaudeThinkingShape {
    let lower = model.to_ascii_lowercase();
    let Some(start) = lower.find("claude-") else {
        return ClaudeThinkingShape::FixedBudget;
    };
    let rest = &lower[start + "claude-".len()..];
    match claude_generation(rest) {
        Some(generation) if generation < (4, 6) => ClaudeThinkingShape::FixedBudget,
        _ => ClaudeThinkingShape::Adaptive,
    }
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
    fn date_and_revision_suffixes_never_read_as_a_version() {
        assert_eq!(claude_generation("sonnet-4-20250514"), Some((4, 0)));
        assert_eq!(claude_generation("opus-4-8-v1"), Some((4, 8)));
        assert_eq!(claude_generation("3-5-haiku-20241022-v1:0"), Some((3, 5)));
        assert_eq!(claude_generation("next"), None);
    }
}
