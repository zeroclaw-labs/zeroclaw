#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UiRenderSpec {
    pub(crate) transcript: TranscriptRenderSpec,
    pub(crate) tools: ToolRenderSpec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TranscriptRenderSpec {
    pub(crate) rails: RailMode,
    pub(crate) thoughts: ThoughtMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ToolRenderSpec {
    pub(crate) semantic_icons: IconMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RailMode {
    None,
    Typed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ThoughtMode {
    Hidden,
    Visible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IconMode {
    None,
    Semantic,
}

impl UiRenderSpec {
    pub(crate) const fn minimal() -> Self {
        Self {
            transcript: TranscriptRenderSpec {
                rails: RailMode::None,
                thoughts: ThoughtMode::Visible,
            },
            tools: ToolRenderSpec {
                semantic_icons: IconMode::None,
            },
        }
    }

    pub(crate) const fn rich() -> Self {
        Self {
            transcript: TranscriptRenderSpec {
                rails: RailMode::Typed,
                thoughts: ThoughtMode::Visible,
            },
            tools: ToolRenderSpec {
                semantic_icons: IconMode::Semantic,
            },
        }
    }
}
