use std::collections::BTreeMap;

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SchemaNode {
    Any,
    Object {
        required: Vec<String>,
        properties: BTreeMap<String, SchemaNode>,
    },
    Array {
        items: Option<Box<SchemaNode>>,
    },
    String,
    Number,
    Integer,
    Boolean,
    Null,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaError {
    Type {
        path: String,
        expected: &'static str,
        got: &'static str,
    },
    MissingRequired {
        path: String,
        key: String,
    },
    Malformed(String),
}

impl std::fmt::Display for SchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Type {
                path,
                expected,
                got,
            } => write!(
                f,
                "schema type mismatch at {path}: expected {expected}, got {got}"
            ),
            Self::MissingRequired { path, key } => {
                write!(f, "schema required key missing at {path}: {key}")
            }
            Self::Malformed(message) => write!(f, "malformed schema: {message}"),
        }
    }
}

impl std::error::Error for SchemaError {}

pub(crate) fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(number) if number.is_i64() || number.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
