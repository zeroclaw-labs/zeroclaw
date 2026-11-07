//! OpenAI Decisions request wire format.

use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use zeroclaw_api::decision::Question;

pub(crate) fn request(
    model: &str,
    state: Value,
    questions: &BTreeMap<String, Question>,
) -> Result<Value> {
    let questions: Vec<Value> = questions
        .iter()
        .map(|(name, question)| match question {
            Question::Noul { instructions } => {
                json!({"type": "predicate", "name": name, "instructions": instructions})
            }
            Question::Choice {
                instructions,
                criteria,
            } => {
                let choices: Vec<Value> = criteria
                    .iter()
                    .map(|(value, description)| json!({"value": value, "description": description}))
                    .collect();
                json!({
                    "type": "choice",
                    "name": name,
                    "instructions": instructions,
                    "choices": choices
                })
            }
        })
        .collect();
    let input = match state {
        Value::String(text) => text,
        value => serde_json::to_string(&value).context("serializing decision state")?,
    };
    Ok(json!({"model": model, "input": input, "questions": questions}))
}
