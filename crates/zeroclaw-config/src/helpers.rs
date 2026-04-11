//! Property helpers used by the `Configurable` derive macro and the `zeroclaw props` CLI.

use crate::traits::{PropFieldInfo, PropKind};

/// Return a comma-separated string of valid enum variant names for display in error messages.
#[cfg(feature = "schema-export")]
pub fn enum_variants<T: schemars::JsonSchema>() -> String {
    #[cfg(feature = "schema-export")]
    let schema = schemars::schema_for!(T);
    let json = match serde_json::to_value(&schema) {
        Ok(v) => v,
        Err(_) => return "(unknown variants)".to_string(),
    };

    if let Some(variants) = json.get("enum").and_then(|v| v.as_array()) {
        let names: Vec<&str> = variants.iter().filter_map(|v| v.as_str()).collect();
        if !names.is_empty() {
            return names.join(", ");
        }
    }

    if let Some(one_of) = json.get("oneOf").and_then(|v| v.as_array()) {
        let names: Vec<&str> = one_of
            .iter()
            .filter_map(|s| {
                s.get("const").and_then(|v| v.as_str()).or_else(|| {
                    s.get("enum")
                        .and_then(|v| v.as_array())
                        .and_then(|arr| arr.first())
                        .and_then(|v| v.as_str())
                })
            })
            .collect();
        if !names.is_empty() {
            return names.join(", ");
        }
    }

    "(unknown variants)".to_string()
}

/// Build a `PropFieldInfo` by reading the display value from a serialized TOML table.
pub fn make_prop_field(
    table: Option<&toml::Table>,
    name: &'static str,
    serde_name: &str,
    category: &'static str,
    type_hint: &'static str,
    kind: PropKind,
    is_secret: bool,
    enum_variants: Option<fn() -> Vec<String>>,
) -> PropFieldInfo {
    let display_value = if is_secret {
        match table.and_then(|t| t.get(serde_name)) {
            Some(toml::Value::String(s)) if !s.is_empty() => "****".to_string(),
            _ => "<unset>".to_string(),
        }
    } else {
        toml_value_to_display(table.and_then(|t| t.get(serde_name)))
    };
    PropFieldInfo {
        name,
        category,
        display_value,
        type_hint,
        kind,
        is_secret,
        enum_variants,
    }
}

/// Get a property value via serde serialization.
pub fn serde_get_prop<T: serde::Serialize>(
    target: &T,
    prefix: &str,
    name: &str,
    is_secret: bool,
) -> anyhow::Result<String> {
    if is_secret {
        return Ok("**** (encrypted)".to_string());
    }
    let serde_name = prop_name_to_serde_field(prefix, name)?;
    let table = toml::Value::try_from(target)?;
    Ok(toml_value_to_display(
        table.as_table().and_then(|t| t.get(&serde_name)),
    ))
}

/// Set a property value via serde roundtrip.
pub fn serde_set_prop<T: serde::Serialize + serde::de::DeserializeOwned>(
    target: &mut T,
    prefix: &str,
    name: &str,
    value_str: &str,
    kind: PropKind,
    is_option: bool,
) -> anyhow::Result<()> {
    let serde_name = prop_name_to_serde_field(prefix, name)?;
    let mut table: toml::Table = toml::from_str(&toml::to_string(target)?)?;
    if value_str.is_empty() && is_option {
        table.remove(&serde_name);
    } else {
        table.insert(serde_name, parse_prop_value(value_str, kind)?);
    }
    *target = toml::from_str(&toml::to_string(&table)?)?;
    Ok(())
}

fn toml_value_to_display(value: Option<&toml::Value>) -> String {
    match value {
        None => "<unset>".to_string(),
        Some(toml::Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
    }
}

fn prop_name_to_serde_field(prefix: &str, name: &str) -> anyhow::Result<String> {
    let suffix = if prefix.is_empty() {
        name
    } else {
        name.strip_prefix(prefix)
            .and_then(|s| s.strip_prefix('.'))
            .ok_or_else(|| anyhow::anyhow!("Unknown property '{name}'"))?
    };
    let field_part = suffix.split('.').next().unwrap_or(suffix);
    Ok(field_part.replace('-', "_"))
}

fn parse_prop_value(value_str: &str, kind: PropKind) -> anyhow::Result<toml::Value> {
    match kind {
        PropKind::Bool => Ok(toml::Value::Boolean(value_str.parse().map_err(|_| {
            anyhow::anyhow!("Invalid bool value '{value_str}' — expected 'true' or 'false'")
        })?)),
        PropKind::Integer => {
            Ok(toml::Value::Integer(value_str.parse().map_err(|_| {
                anyhow::anyhow!("Invalid integer value '{value_str}'")
            })?))
        }
        PropKind::Float => {
            Ok(toml::Value::Float(value_str.parse().map_err(|_| {
                anyhow::anyhow!("Invalid float value '{value_str}'")
            })?))
        }
        PropKind::String | PropKind::Enum => Ok(toml::Value::String(value_str.to_string())),
    }
}
