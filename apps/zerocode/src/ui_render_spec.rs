use crate::config::UiProfile;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UiRenderSpec {
    pub(crate) transcript: TranscriptRenderSpec,
    pub(crate) tools: ToolRenderSpec,
    pub(crate) status: StatusRenderSpec,
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
pub(crate) struct StatusRenderSpec {
    pub(crate) session_row: SessionStatusMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionStatusMode {
    Hidden,
    Workspace,
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
    pub(crate) const fn for_profile(profile: UiProfile) -> Self {
        match profile {
            UiProfile::Minimal => Self::minimal(),
            UiProfile::Rich => Self::rich(),
        }
    }

    pub(crate) const fn minimal() -> Self {
        Self {
            transcript: TranscriptRenderSpec {
                rails: RailMode::None,
                thoughts: ThoughtMode::Visible,
            },
            tools: ToolRenderSpec {
                semantic_icons: IconMode::None,
            },
            status: StatusRenderSpec {
                session_row: SessionStatusMode::Hidden,
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
            status: StatusRenderSpec {
                session_row: SessionStatusMode::Workspace,
            },
        }
    }
}
