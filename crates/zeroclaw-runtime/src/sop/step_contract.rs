use serde::{Deserialize, Serialize};

/// A single named output port on a switch step. Rules are evaluated top to
/// bottom; the first whose `when` guard passes routes the run to `goto`. A
/// rule with `when` unset is the catch-all (n8n's "unknown"/default port) and
/// should be ordered last.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SwitchRule {
    /// Port label shown on the node's output pin (e.g. `pull_request`).
    pub name: String,
    /// Guard evaluated against accumulated run data. `None` = catch-all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    /// Target step this port routes to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goto: Option<u32>,
}

/// Conditional routing metadata for a single SOP step.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct StepRouting {
    /// Guard evaluated against accumulated run data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    /// Explicit successor step number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<u32>,
    /// When true, this step ends its branch: no implicit fallthrough to the
    /// following step is derived. Lets an authoring surface delete the default
    /// sequence edge and leave a node free-floating between saves.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub terminal: bool,
    /// Step numbers that must have completed before this step can run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<u32>,
    /// Ordered switch ports. Non-empty makes this a multi-branch switch node:
    /// each port is a named conditional out-edge, matched top to bottom.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub switch: Vec<SwitchRule>,
}

impl StepRouting {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

/// Failure handling policy for a single SOP step.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum StepFailure {
    #[default]
    Fail,
    Retry {
        max: u32,
    },
    Goto {
        step: u32,
    },
}

impl StepFailure {
    pub fn is_fail(&self) -> bool {
        matches!(self, Self::Fail)
    }
}
