//! Claude model identity helpers shared by the Anthropic and Bedrock adapters.

use zeroclaw_api::model_provider::{ThinkingDisplay, ThinkingEffort};

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

/// Which provider slot serves the model. The adapters forward different
/// request fields, so one model id can take different controls on each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeProviderSlot {
    Anthropic,
    Bedrock,
}

impl ClaudeProviderSlot {
    /// Read the slot from a `<type>.<alias>` provider reference. `None` for
    /// provider types whose adapters carry no Claude thinking controls, such
    /// as an OpenAI-compatible gateway that happens to serve a Claude model.
    #[must_use]
    pub fn from_provider_ref(provider_ref: &str) -> Option<Self> {
        let provider_type = provider_ref
            .split_once('.')
            .map_or(provider_ref, |(provider_type, _)| provider_type);
        match provider_type.trim().to_ascii_lowercase().as_str() {
            "anthropic" => Some(Self::Anthropic),
            "bedrock" => Some(Self::Bedrock),
            _ => None,
        }
    }
}

/// The thinking controls a model accepts on a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThinkingCapabilities {
    pub shape: ClaudeThinkingShape,
    /// Whether a request may name a thinking token budget.
    pub accepts_budget: bool,
    /// The depth settings a request may name, in ascending order.
    pub efforts: &'static [ThinkingEffort],
    /// The displays a request may name.
    pub displays: &'static [ThinkingDisplay],
}

impl ThinkingCapabilities {
    /// No thinking controls at all: the id is not a Claude model, so nothing
    /// is known about what the endpoint behind it accepts.
    pub const NONE: Self = Self {
        shape: ClaudeThinkingShape::FixedBudget,
        accepts_budget: false,
        efforts: &[],
        displays: &[],
    };

    #[must_use]
    pub fn supports_effort(&self, effort: ThinkingEffort) -> bool {
        self.efforts.contains(&effort)
    }

    #[must_use]
    pub fn supports_display(&self, display: ThinkingDisplay) -> bool {
        self.displays.contains(&display)
    }

    /// The deepest accepted setting at or below `effort`, or `None` when the
    /// model takes no depth setting. Every generation that takes a depth
    /// takes the lowest one, so a request never loses its depth entirely.
    #[must_use]
    pub fn fit_effort(&self, effort: ThinkingEffort) -> Option<ThinkingEffort> {
        self.efforts
            .iter()
            .copied()
            .filter(|candidate| *candidate <= effort)
            .max()
    }

    /// The display to send for a requested one: the request itself when the
    /// model takes it, a readable summary in place of progress notes where
    /// the model takes summaries, and nothing where it takes no display.
    #[must_use]
    pub fn fit_display(&self, display: ThinkingDisplay) -> Option<ThinkingDisplay> {
        if self.supports_display(display) {
            Some(display)
        } else if display == ThinkingDisplay::Updates
            && self.supports_display(ThinkingDisplay::Summarized)
        {
            Some(ThinkingDisplay::Summarized)
        } else {
            None
        }
    }
}

/// Generations before 4.6 take a token budget and nothing else.
const FIXED_BUDGET: ThinkingCapabilities = ThinkingCapabilities {
    shape: ClaudeThinkingShape::FixedBudget,
    accepts_budget: true,
    efforts: &[],
    displays: &[],
};

/// Depth settings of the 4.6 generation.
const EFFORTS_4_6: &[ThinkingEffort] = &[
    ThinkingEffort::Low,
    ThinkingEffort::High,
    ThinkingEffort::Max,
];

/// Depth settings of generation 4.7 and later.
const EFFORTS_CURRENT: &[ThinkingEffort] = &[
    ThinkingEffort::Low,
    ThinkingEffort::High,
    ThinkingEffort::XHigh,
    ThinkingEffort::Max,
];

/// Displays generations 4.7 through 5.0 take on the Anthropic API, including
/// the short progress notes some of them write between tool calls.
const DISPLAYS_PRE_5_1: &[ThinkingDisplay] = &[
    ThinkingDisplay::Omitted,
    ThinkingDisplay::Summarized,
    ThinkingDisplay::Updates,
];

/// Displays generation 5.1 and later take on the Anthropic API. The
/// generation narrowed the field to summaries and the API default: Fable 5.1,
/// the one family documented to write progress notes, answered 400 to
/// `updates` in a live probe, so a request for them is sent as a summary
/// instead. A Claude id whose version cannot be read follows this row.
const DISPLAYS_CURRENT: &[ThinkingDisplay] =
    &[ThinkingDisplay::Omitted, ThinkingDisplay::Summarized];

/// Classify what `model` accepts when served through `slot`.
///
/// Anchors on the `claude-` substring so Bedrock ids carrying region and
/// vendor prefixes resolve the same way as bare API ids. Generations before
/// 4.6 keep the fixed budget. Generation 4.6 thinks adaptively but takes no
/// display. Generation 4.7 and later, and any Claude id whose version cannot
/// be read, take every depth, so a new release needs no code change here; on
/// the Anthropic API they also take a display, narrowed from generation 5.1
/// onward. The Bedrock adapter forwards no display. Ids that are not Claude
/// models at all get no controls.
#[must_use]
pub fn thinking_capabilities(slot: ClaudeProviderSlot, model: &str) -> ThinkingCapabilities {
    let lower = model.to_ascii_lowercase();
    let Some(rest) = claude_id_rest(&lower) else {
        return ThinkingCapabilities::NONE;
    };
    let generation = claude_generation(rest);
    match generation {
        Some(generation) if generation < (4, 6) => FIXED_BUDGET,
        Some((4, 6)) => ThinkingCapabilities {
            shape: ClaudeThinkingShape::Adaptive,
            accepts_budget: false,
            efforts: EFFORTS_4_6,
            displays: &[],
        },
        _ => ThinkingCapabilities {
            shape: ClaudeThinkingShape::Adaptive,
            accepts_budget: false,
            efforts: EFFORTS_CURRENT,
            displays: match slot {
                ClaudeProviderSlot::Bedrock => &[],
                ClaudeProviderSlot::Anthropic => match generation {
                    Some(generation) if generation < (5, 1) => DISPLAYS_PRE_5_1,
                    _ => DISPLAYS_CURRENT,
                },
            },
        },
    }
}

/// Classify a model id by the Claude generation it names.
///
/// The shape does not depend on the slot: generations before 4.6 keep the
/// fixed budget, and everything else thinks adaptively. Ids that are not
/// Claude models at all keep the fixed budget, which is the shape
/// Anthropic-compatible proxies accepted before this classification existed.
#[must_use]
pub fn claude_thinking_shape(model: &str) -> ClaudeThinkingShape {
    thinking_capabilities(ClaudeProviderSlot::Anthropic, model).shape
}

/// The part of a lowercase id after `claude-`, or `None` for an id that is
/// not a Claude model.
fn claude_id_rest(lower: &str) -> Option<&str> {
    lower
        .find("claude-")
        .map(|start| &lower[start + "claude-".len()..])
}

/// Read the `(major, minor)` generation from the id tokens after `claude-`.
///
/// The first short all-digit token is the major version and the token right
/// after it, when it is also a short all-digit token, is the minor version.
/// Date stamps and revision suffixes are longer or contain letters, so they
/// never read as a version. Legacy ids spell the generation as `major.minor`
/// in a single token.
#[must_use]
pub fn claude_generation(rest: &str) -> Option<(u32, u32)> {
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
    use ClaudeProviderSlot::{Anthropic, Bedrock};
    use ThinkingDisplay::{Omitted, Summarized, Updates};
    use ThinkingEffort::{High, Low, Max, XHigh};

    const ADAPTIVE_IDS: &[&str] = &[
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
    ];

    const FIXED_BUDGET_IDS: &[&str] = &[
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
    ];

    #[test]
    fn adaptive_generations_classify_as_adaptive() {
        for model in ADAPTIVE_IDS {
            assert_eq!(
                claude_thinking_shape(model),
                ClaudeThinkingShape::Adaptive,
                "{model} should be adaptive"
            );
        }
    }

    #[test]
    fn fixed_budget_generations_classify_as_fixed_budget() {
        for model in FIXED_BUDGET_IDS {
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
            // An id whose generation cannot be read follows the newest row.
            "claude-next",
        ] {
            let capabilities = thinking_capabilities(ClaudeProviderSlot::Anthropic, model);
            assert!(
                !capabilities.supports_display(ThinkingDisplay::Updates),
                "{model} should not take the updates display"
            );
            assert_eq!(
                capabilities.fit_display(ThinkingDisplay::Updates),
                Some(ThinkingDisplay::Summarized),
                "{model} should read progress notes as a summary"
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
            "us.anthropic.claude-opus-4-8-v1",
        ] {
            let capabilities = thinking_capabilities(ClaudeProviderSlot::Anthropic, model);
            assert!(
                capabilities.supports_display(ThinkingDisplay::Updates),
                "{model} should take the updates display"
            );
            assert_eq!(
                capabilities.fit_display(ThinkingDisplay::Updates),
                Some(ThinkingDisplay::Updates),
                "{model} should keep the requested progress notes"
            );
        }
    }

    #[test]
    fn generations_without_a_display_field_take_no_updates() {
        // The 4.6 generation reads no display at all, and the budget
        // generations before it send none either, so progress notes are not
        // fitted to a summary there; they are dropped.
        for model in ["claude-sonnet-4-6", "claude-sonnet-4-5"] {
            let capabilities = thinking_capabilities(ClaudeProviderSlot::Anthropic, model);
            assert!(
                !capabilities.supports_display(ThinkingDisplay::Updates),
                "{model} should not take the updates display"
            );
            assert_eq!(
                capabilities.fit_display(ThinkingDisplay::Updates),
                None,
                "{model} carries no display field"
            );
        }
    }

    #[test]
    fn non_claude_ids_take_no_display_at_all() {
        for model in ["gpt-4o", "minimax-m2", ""] {
            let capabilities = thinking_capabilities(ClaudeProviderSlot::Anthropic, model);
            assert!(
                capabilities.displays.is_empty(),
                "{model} is not a Claude id, so nothing is known about its displays"
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

    #[test]
    fn capabilities_follow_the_slot_and_the_generation() {
        type Row = (
            ClaudeProviderSlot,
            &'static str,
            ClaudeThinkingShape,
            bool,
            &'static [ThinkingEffort],
            &'static [ThinkingDisplay],
        );
        let none: &[ThinkingDisplay] = &[];
        let rows: &[Row] = &[
            (
                Anthropic,
                "gpt-4o",
                ClaudeThinkingShape::FixedBudget,
                false,
                &[],
                none,
            ),
            (
                Bedrock,
                "amazon.nova-pro-v1:0",
                ClaudeThinkingShape::FixedBudget,
                false,
                &[],
                none,
            ),
            (
                Anthropic,
                "",
                ClaudeThinkingShape::FixedBudget,
                false,
                &[],
                none,
            ),
            (
                Anthropic,
                "claude-haiku-4-5",
                ClaudeThinkingShape::FixedBudget,
                true,
                &[],
                none,
            ),
            (
                Bedrock,
                "anthropic.claude-3-5-haiku-20241022-v1:0",
                ClaudeThinkingShape::FixedBudget,
                true,
                &[],
                none,
            ),
            (
                Anthropic,
                "claude-opus-4-6",
                ClaudeThinkingShape::Adaptive,
                false,
                &[Low, High, Max],
                none,
            ),
            (
                Bedrock,
                "us.anthropic.claude-sonnet-4-6-v1",
                ClaudeThinkingShape::Adaptive,
                false,
                &[Low, High, Max],
                none,
            ),
            (
                Anthropic,
                "claude-opus-4-7",
                ClaudeThinkingShape::Adaptive,
                false,
                &[Low, High, XHigh, Max],
                &[Omitted, Summarized, Updates],
            ),
            (
                Anthropic,
                "claude-sonnet-5",
                ClaudeThinkingShape::Adaptive,
                false,
                &[Low, High, XHigh, Max],
                &[Omitted, Summarized, Updates],
            ),
            (
                Anthropic,
                "claude-next",
                ClaudeThinkingShape::Adaptive,
                false,
                &[Low, High, XHigh, Max],
                &[Omitted, Summarized],
            ),
            (
                Anthropic,
                "claude-fable-5-1",
                ClaudeThinkingShape::Adaptive,
                false,
                &[Low, High, XHigh, Max],
                &[Omitted, Summarized],
            ),
            (
                Anthropic,
                "claude-mythos-5-1",
                ClaudeThinkingShape::Adaptive,
                false,
                &[Low, High, XHigh, Max],
                &[Omitted, Summarized],
            ),
            (
                Bedrock,
                "us.anthropic.claude-opus-4-8-v1",
                ClaudeThinkingShape::Adaptive,
                false,
                &[Low, High, XHigh, Max],
                none,
            ),
            (
                Bedrock,
                "anthropic.claude-fable-5-1",
                ClaudeThinkingShape::Adaptive,
                false,
                &[Low, High, XHigh, Max],
                none,
            ),
        ];
        for (slot, model, shape, accepts_budget, efforts, displays) in rows {
            let capabilities = thinking_capabilities(*slot, model);
            assert_eq!(capabilities.shape, *shape, "shape of {model} on {slot:?}");
            assert_eq!(
                capabilities.accepts_budget, *accepts_budget,
                "budget of {model} on {slot:?}"
            );
            assert_eq!(
                capabilities.efforts, *efforts,
                "efforts of {model} on {slot:?}"
            );
            assert_eq!(
                capabilities.displays, *displays,
                "displays of {model} on {slot:?}"
            );
        }
    }

    #[test]
    fn fit_effort_takes_the_nearest_depth_at_or_below() {
        let current = thinking_capabilities(Anthropic, "claude-opus-4-8");
        assert_eq!(current.fit_effort(Max), Some(Max));
        assert_eq!(current.fit_effort(High), Some(High));
        assert_eq!(current.fit_effort(Low), Some(Low));
        assert!(current.supports_effort(Max));
        assert!(current.supports_display(Summarized));
        assert!(
            current.supports_display(Updates),
            "the 4.8 generation still reads the progress notes"
        );
        assert!(
            !thinking_capabilities(Anthropic, "claude-fable-5-1").supports_display(Updates),
            "generation 5.1 narrowed the field to summaries and the API default"
        );

        assert_eq!(current.fit_effort(XHigh), Some(XHigh));

        let previous = thinking_capabilities(Anthropic, "claude-opus-4-6");
        assert_eq!(
            previous.fit_effort(XHigh),
            Some(High),
            "the 4.6 generation has no xhigh, so it takes the depth just below"
        );
        assert!(!previous.supports_effort(XHigh));

        let older = thinking_capabilities(Anthropic, "claude-haiku-4-5");
        assert_eq!(older.fit_effort(High), None);
        assert!(!older.supports_effort(Low));

        assert_eq!(ThinkingCapabilities::NONE.fit_effort(Low), None);
        assert!(!ThinkingCapabilities::NONE.supports_display(Omitted));
    }

    #[test]
    fn fit_display_sends_summaries_in_place_of_progress_notes() {
        let fable = thinking_capabilities(Anthropic, "claude-fable-5-1");
        assert_eq!(fable.fit_display(Summarized), Some(Summarized));
        assert_eq!(fable.fit_display(Omitted), Some(Omitted));
        assert_eq!(
            fable.fit_display(Updates),
            Some(Summarized),
            "progress notes are not offered anywhere yet, so a summary stands in"
        );
        let previous = thinking_capabilities(Anthropic, "claude-opus-4-6");
        assert_eq!(previous.fit_display(Updates), None);
        assert_eq!(previous.fit_display(Summarized), None);
        let bedrock = thinking_capabilities(Bedrock, "anthropic.claude-fable-5-1");
        assert_eq!(bedrock.fit_display(Summarized), None);
    }

    #[test]
    fn provider_slot_reads_the_type_of_a_reference() {
        assert_eq!(
            ClaudeProviderSlot::from_provider_ref("anthropic.default"),
            Some(Anthropic)
        );
        assert_eq!(
            ClaudeProviderSlot::from_provider_ref("Bedrock.eu-west"),
            Some(Bedrock)
        );
        assert_eq!(
            ClaudeProviderSlot::from_provider_ref("anthropic"),
            Some(Anthropic)
        );
        assert_eq!(
            ClaudeProviderSlot::from_provider_ref("openrouter.default"),
            None
        );
        assert_eq!(ClaudeProviderSlot::from_provider_ref(""), None);
    }

    #[test]
    fn shape_agrees_with_capabilities_on_every_slot() {
        for model in ADAPTIVE_IDS.iter().chain(FIXED_BUDGET_IDS) {
            for slot in [Anthropic, Bedrock] {
                assert_eq!(
                    thinking_capabilities(slot, model).shape,
                    claude_thinking_shape(model),
                    "{model} on {slot:?}"
                );
            }
        }
    }
}
