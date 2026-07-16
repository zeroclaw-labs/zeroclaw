mod compile;
mod types;

use serde_json::Value;

pub use types::SchemaError;
use types::{SchemaNode, value_kind};

/// Validate a JSON value against ZeroClaw's minimal SOP schema subset.
///
/// Supported keywords are `type`, `required`, `properties`, and `items`.
/// Malformed or unsupported type declarations fail closed.
pub fn validate_value(schema: &Value, data: &Value) -> Result<(), SchemaError> {
    let node = compile::compile_schema(schema)?;
    validate_node(&node, data, "$")
}

fn validate_node(node: &SchemaNode, data: &Value, path: &str) -> Result<(), SchemaError> {
    match node {
        SchemaNode::Any => Ok(()),
        SchemaNode::Object {
            required,
            properties,
        } => {
            let object = data
                .as_object()
                .ok_or_else(|| type_error(path, "object", data))?;
            for key in required {
                if !object.contains_key(key) {
                    return Err(SchemaError::MissingRequired {
                        path: path.into(),
                        key: key.clone(),
                    });
                }
            }
            for (key, child) in properties {
                if let Some(value) = object.get(key) {
                    validate_node(child, value, &child_path(path, key))?;
                }
            }
            Ok(())
        }
        SchemaNode::Array { items } => {
            let array = data
                .as_array()
                .ok_or_else(|| type_error(path, "array", data))?;
            if let Some(items) = items {
                for (idx, value) in array.iter().enumerate() {
                    validate_node(items, value, &child_path(path, &idx.to_string()))?;
                }
            }
            Ok(())
        }
        SchemaNode::String => {
            if data.is_string() {
                Ok(())
            } else {
                Err(type_error(path, "string", data))
            }
        }
        SchemaNode::Number => {
            if data.is_number() {
                Ok(())
            } else {
                Err(type_error(path, "number", data))
            }
        }
        SchemaNode::Integer => {
            if data.as_i64().is_some() || data.as_u64().is_some() {
                Ok(())
            } else {
                Err(type_error(path, "integer", data))
            }
        }
        SchemaNode::Boolean => {
            if data.is_boolean() {
                Ok(())
            } else {
                Err(type_error(path, "boolean", data))
            }
        }
        SchemaNode::Null => {
            if data.is_null() {
                Ok(())
            } else {
                Err(type_error(path, "null", data))
            }
        }
    }
}

fn type_error(path: &str, expected: &'static str, got: &Value) -> SchemaError {
    SchemaError::Type {
        path: path.into(),
        expected,
        got: value_kind(got),
    }
}

fn child_path(parent: &str, child: &str) -> String {
    if parent == "$" {
        format!("$.{child}")
    } else {
        format!("{parent}.{child}")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn validates_nested_object_schema() {
        let schema = json!({
            "type": "object",
            "required": ["status"],
            "properties": {
                "status": { "type": "string" },
                "count": { "type": "integer" }
            }
        });
        let data = json!({ "status": "ok", "count": 2 });

        validate_value(&schema, &data).expect("valid data should pass");
    }

    #[test]
    fn rejects_missing_required_property() {
        let schema = json!({
            "type": "object",
            "required": ["status"]
        });

        assert!(matches!(
            validate_value(&schema, &json!({})),
            Err(SchemaError::MissingRequired { .. })
        ));
    }

    #[test]
    fn validates_array_items() {
        let schema = json!({
            "type": "array",
            "items": { "type": "boolean" }
        });

        validate_value(&schema, &json!([true, false])).expect("valid array should pass");
        assert!(matches!(
            validate_value(&schema, &json!([true, "no"])),
            Err(SchemaError::Type { .. })
        ));
    }

    #[test]
    fn malformed_schema_fails_closed() {
        let schema = json!({ "type": 7 });

        assert!(matches!(
            validate_value(&schema, &json!({})),
            Err(SchemaError::Malformed(_))
        ));
    }
}
