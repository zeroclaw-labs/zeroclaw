//! Shared runtime-status wire types.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeConfigKind {
    Default,
    Custom,
    Temporary,
}
