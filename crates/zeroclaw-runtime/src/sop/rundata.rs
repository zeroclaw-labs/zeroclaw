use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::types::{SopStepResult, SopStepStatus};

/// Accumulated step outputs, shared by schema validation and routing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunData {
    pub outputs: BTreeMap<u32, Value>,
}

impl RunData {
    pub fn from_step_results(results: &[SopStepResult]) -> Self {
        let mut data = Self::default();
        for result in results {
            if result.status == SopStepStatus::Completed {
                data.insert_output_str(result.step_number, &result.output);
            }
        }
        data
    }

    pub fn insert_output(&mut self, step_number: u32, output: Value) {
        self.outputs.insert(step_number, output);
    }

    pub fn insert_output_str(&mut self, step_number: u32, output: &str) {
        let value = serde_json::from_str(output).unwrap_or_else(|_| Value::String(output.into()));
        self.insert_output(step_number, value);
    }

    pub fn merge(&mut self, other: RunData) {
        self.outputs.extend(other.outputs);
    }

    pub fn to_payload(&self) -> Value {
        let steps = self
            .outputs
            .iter()
            .map(|(step, value)| (step.to_string(), value.clone()))
            .collect();
        json!({ "steps": Value::Object(steps) })
    }

    pub fn get_path(&self, path: &str) -> Option<Value> {
        if path.is_empty() {
            return None;
        }
        let payload = self.to_payload();
        let pointer = path
            .strip_prefix("$.")
            .map(|rest| format!("/{}", rest.replace('.', "/")))
            .unwrap_or_else(|| path.to_string());
        payload.pointer(&pointer).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_data_parses_json_outputs() {
        let mut data = RunData::default();
        data.insert_output_str(1, r#"{"ok":true}"#);

        assert_eq!(data.outputs[&1]["ok"], true);
    }

    #[test]
    fn run_data_keeps_plain_outputs_as_strings() {
        let mut data = RunData::default();
        data.insert_output_str(2, "plain text");

        assert_eq!(data.outputs[&2], Value::String("plain text".into()));
    }

    #[test]
    fn run_data_ignores_failed_and_skipped_outputs() {
        let results = vec![
            SopStepResult {
                effective_agent: None,
                step_number: 1,
                status: SopStepStatus::Failed,
                output: r#"{"failed":true}"#.into(),
                started_at: "now".into(),
                completed_at: Some("now".into()),
                tool_calls: Vec::new(),
            },
            SopStepResult {
                effective_agent: None,
                step_number: 2,
                status: SopStepStatus::Skipped,
                output: r#"{"skipped":true}"#.into(),
                started_at: "now".into(),
                completed_at: Some("now".into()),
                tool_calls: Vec::new(),
            },
            SopStepResult {
                effective_agent: None,
                step_number: 3,
                status: SopStepStatus::Completed,
                output: r#"{"ok":true}"#.into(),
                started_at: "now".into(),
                completed_at: Some("now".into()),
                tool_calls: Vec::new(),
            },
        ];

        let data = RunData::from_step_results(&results);

        assert!(!data.outputs.contains_key(&1));
        assert!(!data.outputs.contains_key(&2));
        assert_eq!(data.outputs[&3]["ok"], true);
    }
}
