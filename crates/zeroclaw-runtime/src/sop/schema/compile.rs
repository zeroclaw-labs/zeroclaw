use std::collections::BTreeMap;

use serde_json::Value;

use super::types::{SchemaError, SchemaNode};

const TYPE_OBJECT: &str = "object";
const TYPE_ARRAY: &str = "array";
const TYPE_STRING: &str = "string";
const TYPE_NUMBER: &str = "number";
const TYPE_INTEGER: &str = "integer";
const TYPE_BOOLEAN: &str = "boolean";
const TYPE_NULL: &str = "null";

pub(crate) fn compile_schema(schema: &Value) -> Result<SchemaNode, SchemaError> {
    let object = schema
        .as_object()
        .ok_or_else(|| SchemaError::Malformed("schema fragment must be an object".into()))?;

    let required = compile_required(object.get("required"))?;
    let properties = compile_properties(object.get("properties"))?;
    let items = object
        .get("items")
        .map(compile_schema)
        .transpose()?
        .map(Box::new);

    let Some(schema_type) = object.get("type") else {
        if !properties.is_empty() || !required.is_empty() {
            return Ok(SchemaNode::Object {
                required,
                properties,
            });
        }
        if items.is_some() {
            return Ok(SchemaNode::Array { items });
        }
        return Ok(SchemaNode::Any);
    };

    let schema_type = schema_type
        .as_str()
        .ok_or_else(|| SchemaError::Malformed("type must be a string".into()))?;

    match schema_type {
        TYPE_OBJECT => Ok(SchemaNode::Object {
            required,
            properties,
        }),
        TYPE_ARRAY => Ok(SchemaNode::Array { items }),
        TYPE_STRING => Ok(SchemaNode::String),
        TYPE_NUMBER => Ok(SchemaNode::Number),
        TYPE_INTEGER => Ok(SchemaNode::Integer),
        TYPE_BOOLEAN => Ok(SchemaNode::Boolean),
        TYPE_NULL => Ok(SchemaNode::Null),
        other => Err(SchemaError::Malformed(format!(
            "unsupported type `{other}`"
        ))),
    }
}

fn compile_required(value: Option<&Value>) -> Result<Vec<String>, SchemaError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| SchemaError::Malformed("required must be an array".into()))?;
    values
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| SchemaError::Malformed("required entries must be strings".into()))
        })
        .collect()
}

fn compile_properties(value: Option<&Value>) -> Result<BTreeMap<String, SchemaNode>, SchemaError> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let properties = value
        .as_object()
        .ok_or_else(|| SchemaError::Malformed("properties must be an object".into()))?;
    properties
        .iter()
        .map(|(name, schema)| Ok((name.clone(), compile_schema(schema)?)))
        .collect()
}
