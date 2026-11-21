//! Provider-neutral decision protocol types.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

// ── Wire protocol (TypeSafe `/v1/systemone`, also served by `laya-serve`) ──

/// One typed question. Only the two shapes dispatch needs are modeled.
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

/// Provider-neutral decision call; SOP policy remains a consumer.
#[async_trait::async_trait]
pub trait DecisionModel: Send + Sync {
    /// Short identifier recorded with each decision (`jev-latest`, `laya`, ...).
    fn id(&self) -> &str;
    async fn ask(&self, state: Value, questions: BTreeMap<String, Question>) -> Result<Answers>;
}
