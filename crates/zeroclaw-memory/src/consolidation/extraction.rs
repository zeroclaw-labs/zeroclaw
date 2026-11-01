//! The extraction contract ends here. Storage only receives validated results.

use std::fmt;
use std::sync::OnceLock;

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use zeroclaw_api::model_provider::{
    ChatMessage, ChatRequest, ChatResponse, ModelProvider, ToolRoundPolicy,
    build_tool_instructions_text,
};
use zeroclaw_api::tool::ToolSpec;
use zeroclaw_providers::ProviderDispatch;

use super::ConsolidationResult;

const TOOL_NAME: &str = "extract_consolidation";
// Includes JSON's largest Unicode escape spelling at all schema string limits.
const MAX_RESPONSE_BYTES: usize = 262_144;
const MAX_DIAGNOSTICS: usize = 8;
const MAX_FEEDBACK_BYTES: usize = 256;

struct Contract {
    schema: Value,
    validator: jsonschema::Validator,
}

fn contract() -> Result<&'static Contract, ()> {
    static CONTRACT: OnceLock<Result<Contract, ()>> = OnceLock::new();
    CONTRACT
        .get_or_init(|| {
            let schema: Value = serde_json::from_str(include_str!("../consolidation.schema.json"))
                .map_err(|_| ())?;
            let validator = jsonschema::draft202012::options()
                .with_pattern_options(jsonschema::PatternOptions::regex())
                .build(&schema)
                .map_err(|_| ())?;
            Ok(Contract { schema, validator })
        })
        .as_ref()
        .map_err(|_| ())
}

fn tool_schema(schema: &Value) -> Value {
    let mut projection = schema.clone();
    if let Some(root) = projection.as_object_mut() {
        root.remove("$schema");
        root.remove("if");
        root.remove("then");
        if let Some(properties) = root.get_mut("properties").and_then(Value::as_object_mut) {
            for property in properties.values_mut() {
                if property["type"]
                    .as_array()
                    .is_some_and(|types| types.iter().any(|kind| kind == "null"))
                    && let Some(property) = property.as_object_mut()
                {
                    // The shared provider cleaner removes null from type unions.
                    // Widen this projection instead; the canonical validator
                    // still owns nullability and every other acceptance rule.
                    property.remove("type");
                }
            }
        }
    }
    projection
}

/// Only schema-owned property names and fixed keywords can enter feedback.
fn feedback(contract: &Contract, candidate: &Value) -> String {
    let mut diagnostics = Vec::new();
    let mut bytes = 0;
    for error in contract
        .validator
        .iter_errors(candidate)
        .take(MAX_DIAGNOSTICS)
    {
        let schema_path = error.schema_path().to_string();
        let keyword = match schema_path.rsplit('/').next().unwrap_or("") {
            keyword @ ("type"
            | "required"
            | "additionalProperties"
            | "pattern"
            | "maxLength"
            | "maxItems"
            | "enum") => keyword,
            _ => "type",
        };
        let missing = match error.kind() {
            jsonschema::error::ValidationErrorKind::Required { property } => property.as_str(),
            _ => None,
        };
        let mut segments = schema_path.split('/');
        let field = missing.or_else(|| {
            segments.find(|segment| *segment == "properties")?;
            segments.next()
        });
        let path = field
            .filter(|field| contract.schema["properties"].get(*field).is_some())
            .map_or_else(|| "/".to_string(), |field| format!("/{field}"));
        let diagnostic = format!("{keyword} {path}");
        if !diagnostics.contains(&diagnostic) && bytes + diagnostic.len() < MAX_FEEDBACK_BYTES {
            bytes += diagnostic.len() + 1;
            diagnostics.push(diagnostic);
        }
    }
    diagnostics.join("\n")
}

fn validate(candidate: Value, contract: &Contract) -> Result<ConsolidationResult, String> {
    let diagnostics = feedback(contract, &candidate);
    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }
    // Deserialization must never become a second, more permissive validator.
    serde_json::from_value(candidate).map_err(|_| "type /".into())
}

// Keep serde's parser, recursion limit and streaming offsets, but reject
// duplicate keys before Value would discard an earlier candidate value.
struct UniqueValue(Value);

impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct UniqueVisitor;
        impl<'de> Visitor<'de> for UniqueVisitor {
            type Value = UniqueValue;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("JSON with unique object keys")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut object = serde_json::Map::new();
                while let Some((key, value)) = map.next_entry::<String, UniqueValue>()? {
                    if object.insert(key, value.0).is_some() {
                        return Err(de::Error::custom("duplicate object key"));
                    }
                }
                Ok(UniqueValue(Value::Object(object)))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = seq.next_element::<UniqueValue>()? {
                    values.push(value.0);
                }
                Ok(UniqueValue(Value::Array(values)))
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::String(value.into())))
            }
            fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::Bool(value)))
            }
            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
                Ok(UniqueValue(value.into()))
            }
            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(UniqueValue(value.into()))
            }
            fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
                serde_json::Number::from_f64(value)
                    .map(|number| UniqueValue(Value::Number(number)))
                    .ok_or_else(|| de::Error::custom("invalid JSON number"))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::Null))
            }
        }
        deserializer.deserialize_any(UniqueVisitor)
    }
}

fn prompt_candidate(raw: &str) -> Result<Value, String> {
    if raw.len() > MAX_RESPONSE_BYTES {
        return Err("maxLength /".into());
    }
    let mut remaining = raw;
    let mut candidate = None;
    while let Some(start) = remaining.find(['{', '[']) {
        if candidate.is_some() {
            return Err("maxItems /".into());
        }
        let mut stream =
            serde_json::Deserializer::from_str(&remaining[start..]).into_iter::<UniqueValue>();
        let value = stream.next().and_then(Result::ok).ok_or("type /")?.0;
        if !value.is_object() {
            return Err("type /".into());
        }
        remaining = &remaining[start + stream.byte_offset()..];
        candidate = Some(value);
    }
    let mut candidate = candidate.ok_or("required /")?;
    if candidate.get("name").is_some() || candidate.get("arguments").is_some() {
        let object = candidate.as_object_mut().ok_or("type /")?;
        if object.len() != 2 || object.get("name").and_then(Value::as_str) != Some(TOOL_NAME) {
            return Err("additionalProperties /".into());
        }
        candidate = object.remove("arguments").ok_or("required /")?;
    }
    Ok(candidate)
}

fn native_candidate(response: ChatResponse) -> Result<Value, String> {
    if response.tool_calls.len() != 1 {
        return Err("maxItems /".into());
    }
    let call = &response.tool_calls[0];
    if call.name != TOOL_NAME || call.arguments.len() > MAX_RESPONSE_BYTES {
        return Err("type /".into());
    }
    // Native arguments must be one complete JSON value, without prose/fences.
    serde_json::from_str::<UniqueValue>(&call.arguments)
        .map(|value| value.0)
        .map_err(|_| "type /".into())
}

pub(super) fn parse_prompt(raw: &str) -> Result<ConsolidationResult, String> {
    let contract = contract().map_err(|_| "type /")?;
    validate(prompt_candidate(raw)?, contract)
}

fn log_failure(reason: &'static str, attempt: usize) {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(::serde_json::json!({
                "error_key": "memory.consolidation_extraction_failed",
                "reason": reason,
                "attempt": attempt,
            })),
        "Memory consolidation extraction failed, using history fallback"
    );
}

pub(super) async fn extract(
    provider: &dyn ModelProvider,
    model: &str,
    temperature: Option<f64>,
    system_prompt: &str,
    turn: &str,
) -> Option<ConsolidationResult> {
    let contract = contract()
        .map_err(|()| log_failure("contract_initialization_failed", 0))
        .ok()?;
    let native = provider.supports_forced_tool_choice(model);
    let tool = ToolSpec::new(
        TOOL_NAME,
        "Extract one complete memory consolidation result. This tool is never executed.",
        if native {
            tool_schema(&contract.schema)
        } else {
            contract.schema.clone()
        },
    );
    let tools = [tool];
    let instructions = if native {
        format!("{system_prompt}\nUse the extraction tool exactly once.")
    } else {
        format!(
            "{system_prompt}\n{}\nFor this extraction, return exactly one extract_consolidation tool call, or its flat JSON arguments alone. Do not emit multiple calls or continue after the result. This tool is an output envelope and will never be executed.",
            build_tool_instructions_text(&tools)
        )
    };
    let mut messages = vec![ChatMessage::system(instructions), ChatMessage::user(turn)];
    // This consumer owns semantic repair only. Existing provider transport
    // retries/failover retain their configured policy beneath each invocation.
    for attempt in 0..2 {
        let response = zeroclaw_api::TOOL_CHOICE_OVERRIDE
            .scope(
                native.then(|| "any".to_string()),
                ProviderDispatch::from_ref(provider).chat_with_tool_round_policy(
                    ChatRequest {
                        messages: &messages,
                        tools: native.then_some(tools.as_slice()),
                        thinking: None,
                    },
                    model,
                    temperature,
                    if native {
                        ToolRoundPolicy::Single
                    } else {
                        ToolRoundPolicy::Batch
                    },
                ),
            )
            .await
            .map_err(|_| log_failure("provider_error", attempt + 1))
            .ok()?;
        let result = if native {
            native_candidate(response).and_then(|candidate| validate(candidate, contract))
        } else {
            if !response.tool_calls.is_empty() {
                log_failure("unexpected_tool_calls", attempt + 1);
                return None;
            }
            let Some(text) = response
                .text
                .as_deref()
                .filter(|text| !text.trim().is_empty())
            else {
                log_failure("missing_output", attempt + 1);
                return None;
            };
            parse_prompt(text)
        };
        match result {
            Ok(result) => return Some(result),
            Err(diagnostics) if attempt == 0 => {
                // Some prompt adapters forward only the last user message.
                // Keep the source and repair together instead of depending
                // on multi-message history support.
                messages[1] = ChatMessage::user(format!(
                    "{turn}\n\nRepair the extraction. Schema keywords and field paths:\n{diagnostics}"
                ));
            }
            Err(_) => {
                log_failure("repair_exhausted", attempt + 1);
                return None;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests;
