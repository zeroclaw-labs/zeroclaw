//! Provider-neutral decision-model wire contract.

use std::collections::BTreeMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Typed question sent to a decision model.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    Noul {
        instructions: String,
    },
    Choice {
        instructions: String,
        criteria: BTreeMap<String, String>,
    },
}

/// Typed answer returned by a decision model.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Answer {
    Noul {
        noul: f64,
    },
    Choice {
        choice: String,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
}

#[derive(Debug, Clone, Deserialize)]
pub struct Answers {
    #[serde(default)]
    pub model: Option<String>,
    pub answers: BTreeMap<String, Answer>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
}

/// A typed decision provider used by SOP policy evaluation.
#[async_trait::async_trait]
pub trait DecisionModel: Send + Sync {
    /// Stable model identifier recorded in decision audit data.
    fn id(&self) -> &str;

    /// Ask typed questions about a JSON state.
    async fn ask(&self, state: Value, questions: BTreeMap<String, Question>) -> Result<Answers>;
}
