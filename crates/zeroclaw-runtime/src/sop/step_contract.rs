use serde::{Deserialize, Serialize};

/// Conditional routing metadata for a single SOP step.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepRouting {
    /// Guard evaluated against accumulated run data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    /// Explicit successor step number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<u32>,
    /// Step numbers that must have completed before this step can run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<u32>,
}

impl StepRouting {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

/// Failure handling policy for a single SOP step.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
