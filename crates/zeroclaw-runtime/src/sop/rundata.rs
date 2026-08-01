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

/// Parse a model step's raw output text into a JSON value.
///
/// Exact JSON remains the first and schema-independent path. Fuzzy recovery is
/// deliberately limited to outputs with a declared schema: trigger payloads and
/// other raw strings must not lose their surrounding instructions just because
/// they contain a JSON example. A recovered candidate must be the only complete
/// object/array in the text and satisfy the declared output schema.
pub(crate) fn parse_step_output_value(raw: &str, output_schema: Option<&Value>) -> Value {
    let trimmed = raw.trim();

    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return value;
    }

    let Some(schema) = output_schema else {
        return Value::String(raw.into());
    };
    let Some(candidate) = unique_embedded_container(trimmed) else {
        return Value::String(raw.into());
    };
    if super::schema::validate_value(schema, &candidate).is_ok() {
        candidate
    } else {
        Value::String(raw.into())
    }
}

/// Return one complete embedded JSON object or array. Advancing past a parsed
/// container prevents nested objects (for example, an object inside an array)
/// from becoming competing candidates. A second top-level candidate makes the
/// output ambiguous and therefore unrecoverable.
fn unique_embedded_container(text: &str) -> Option<Value> {
    let mut candidate = None;
    let mut cursor = 0;

    while cursor < text.len() {
        let Some((relative_start, opening)) = text[cursor..]
            .char_indices()
            .find(|(_, ch)| matches!(ch, '{' | '['))
        else {
            break;
        };
        let start = cursor + relative_start;
        let mut stream = serde_json::Deserializer::from_str(&text[start..]).into_iter::<Value>();
        match stream.next() {
            Some(Ok(value)) if value.is_object() || value.is_array() => {
                if candidate.is_some() {
                    return None;
                }
                candidate = Some(value);
                cursor = start + stream.byte_offset();
            }
            _ => {
                cursor = start + opening.len_utf8();
            }
        }
    }

    candidate
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
    fn parses_fenced_json_wrapped_in_prose() {
        // The shape a chat-tuned / "Claude Code"-identity model commonly emits.
        let raw = "The run has ended — final status is **failed**.\n\n\
            ```json\n{\n  \"status\": \"ok\",\n  \"word_count\": 738\n}\n```\n\n\
            Summary: the run stopped safely at Step 1.";
        let schema = json!({"type": "object", "required": ["status", "word_count"]});
        let value = parse_step_output_value(raw, Some(&schema));
        assert_eq!(value["status"], "ok");
        assert_eq!(value["word_count"], 738);
    }

    #[test]
    fn parses_bare_fenced_json_without_lang_tag() {
        let raw = "```\n{\"outcome\": \"stopped\"}\n```";
        let schema = json!({"type": "object", "required": ["outcome"]});
        assert_eq!(
            parse_step_output_value(raw, Some(&schema))["outcome"],
            "stopped"
        );
    }

    #[test]
    fn parses_object_embedded_in_prose_without_fences() {
        let raw = "Here is the result: {\"preferences_loaded\": true} — done.";
        let schema = json!({"type": "object", "required": ["preferences_loaded"]});
        assert_eq!(
            parse_step_output_value(raw, Some(&schema))["preferences_loaded"],
            Value::Bool(true)
        );
    }

    #[test]
    fn braces_inside_strings_do_not_unbalance_extraction() {
        let raw = "note: {\"summary\": \"contains a } brace and { another\"} tail";
        let schema = json!({"type": "object", "required": ["summary"]});
        let value = parse_step_output_value(raw, Some(&schema));
        assert_eq!(value["summary"], "contains a } brace and { another");
    }

    #[test]
    fn non_json_prose_still_falls_back_to_string() {
        let raw = "I could not complete the extraction step.";
        let schema = json!({"type": "object"});
        assert_eq!(
            parse_step_output_value(raw, Some(&schema)),
            Value::String(raw.into())
        );
    }

    #[test]
    fn preserves_complete_array_instead_of_extracting_nested_object() {
        let raw = "Result: [{\"approved\":true}]";
        let schema = json!({
            "type": "array",
            "items": {"type": "object", "required": ["approved"]}
        });

        assert_eq!(
            parse_step_output_value(raw, Some(&schema)),
            json!([{"approved": true}])
        );
    }

    #[test]
    fn competing_json_values_fail_closed_to_original_string() {
        let raw = "first {\"approved\":true}; second {\"approved\":false}";
        let schema = json!({"type": "object", "required": ["approved"]});

        assert_eq!(
            parse_step_output_value(raw, Some(&schema)),
            Value::String(raw.into())
        );
    }

    #[test]
    fn recovery_must_match_declared_output_shape() {
        let raw = "Use this example: {\"approved\":true}";
        let schema = json!({"type": "string"});

        assert_eq!(
            parse_step_output_value(raw, Some(&schema)),
            Value::String(raw.into())
        );
    }

    #[test]
    fn wrapped_json_without_output_schema_stays_raw() {
        let raw = "Use this example: {\"approved\":true}";

        assert_eq!(
            parse_step_output_value(raw, None),
            Value::String(raw.into())
        );
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
