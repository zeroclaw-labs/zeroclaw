//! Property helpers used by the `Configurable` derive macro and the `zeroclaw config` CLI.

use crate::traits::{PropFieldInfo, PropKind};

/// For a `#[nested] HashMap<String, T>` field, parse a `get_prop`/`set_prop`
/// path of the form `<my_prefix>.<field_name>.<hm_key>.<inner_suffix>` and
/// return the HashMap key + the fully-qualified inner name that the value
/// type's own `get_prop` / `set_prop` expects.
///
/// HashMap keys are user-controlled and may contain dots, URLs, or hostnames
/// (for example `providers.models.custom:https://example.invalid/v1.api-key`).
/// Current map-keyed config sections expose leaf fields, so split from the
/// right and preserve any dots inside the runtime key.
///
/// Returns `None` when the path doesn't match, letting the derive's
/// generated code fall through to the next nested field.
pub fn route_hashmap_path<'a>(
    name: &'a str,
    my_prefix: &str,
    field_name: &str,
    inner_prefix: &str,
) -> Option<(&'a str, String)> {
    let key_prefix = if my_prefix.is_empty() {
        field_name.to_string()
    } else {
        format!("{my_prefix}.{field_name}")
    };
    let rest = name.strip_prefix(&key_prefix)?.strip_prefix('.')?;
    let (hm_key, inner_suffix) = rest.rsplit_once('.')?;
    let inner_name = if inner_prefix.is_empty() {
        inner_suffix.to_string()
    } else {
        format!("{inner_prefix}.{inner_suffix}")
    };
    Some((hm_key, inner_name))
}

/// For a `#[nested] HashMap<String, HashMap<String, T>>` field, parse a path
/// `<my_prefix>.<field_name>.<outer_key>.<inner_key>.<inner_suffix>` and
/// return (outer_key, inner_key, fully-qualified inner name for T::get_prop).
///
/// Returns `None` when the path doesn't match (wrong prefix or too few segments).
pub fn route_double_hashmap_path<'a>(
    name: &'a str,
    my_prefix: &str,
    field_name: &str,
    inner_prefix: &str,
) -> Option<(&'a str, &'a str, String)> {
    let key_prefix = if my_prefix.is_empty() {
        field_name.to_string()
    } else {
        format!("{my_prefix}.{field_name}")
    };
    let rest = name.strip_prefix(&key_prefix)?.strip_prefix('.')?;
    let (outer_key, rest2) = rest.split_once('.')?;
    let (inner_key, inner_suffix) = rest2.split_once('.')?;
    let inner_name = if inner_prefix.is_empty() {
        inner_suffix.to_string()
    } else {
        format!("{inner_prefix}.{inner_suffix}")
    };
    Some((outer_key, inner_key, inner_name))
}

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
#[allow(clippy::too_many_arguments)]
pub fn make_prop_field(
    table: Option<&toml::Table>,
    name: &str,
    serde_name: &str,
    category: &'static str,
    type_hint: &'static str,
    kind: PropKind,
    is_secret: bool,
    enum_variants: Option<fn() -> Vec<String>>,
    description: &'static str,
    derived_from_secret: bool,
) -> PropFieldInfo {
    let display_value = if is_secret || derived_from_secret {
        match table.and_then(|t| t.get(serde_name)) {
            Some(toml::Value::String(s)) if !s.is_empty() => "****".to_string(),
            Some(toml::Value::Array(arr)) if !arr.is_empty() => {
                format!("[{}]", vec!["****"; arr.len()].join(", "))
            }
            _ => "<unset>".to_string(),
        }
    } else {
        toml_value_to_display(table.and_then(|t| t.get(serde_name)))
    };
    PropFieldInfo {
        name: name.to_string(),
        category,
        display_value,
        type_hint,
        kind,
        is_secret,
        enum_variants,
        description,
        derived_from_secret,
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
        PropKind::StringArray => {
            let trimmed = value_str.trim();
            // Accept JSON/TOML array syntax: ["a", "b", "c"]
            if trimmed.starts_with('[')
                && let Ok(arr) = serde_json::from_str::<Vec<String>>(trimmed)
            {
                return Ok(toml::Value::Array(
                    arr.into_iter().map(toml::Value::String).collect(),
                ));
            }
            // Fall back to comma-separated input.
            let items = value_str
                .split(',')
                .map(|s| toml::Value::String(s.trim().to_string()))
                .filter(|v| v.as_str().is_some_and(|s| !s.is_empty()))
                .collect();
            Ok(toml::Value::Array(items))
        }
        // `Vec<T>` of structs: round-trip a JSON array of objects to a
        // TOML array. JSON `null` (used by serde for `Option::None`) is
        // dropped because TOML has no null — the absent key conveys the
        // same meaning when the field deserializes back into `Option<T>`.
        PropKind::ObjectArray => {
            let v: serde_json::Value = serde_json::from_str(value_str)
                .map_err(|e| anyhow::anyhow!("invalid JSON array of objects: {e}"))?;
            json_to_toml(v).ok_or_else(|| {
                anyhow::anyhow!("JSON value contained only nulls — nothing to write")
            })
        }
    }
}

/// Walk a `serde_json::Value` into a `toml::Value`, dropping any `null`s
/// (TOML has no null; absence of a key conveys `Option::None`).
fn json_to_toml(v: serde_json::Value) -> Option<toml::Value> {
    match v {
        serde_json::Value::Null => None,
        serde_json::Value::Bool(b) => Some(toml::Value::Boolean(b)),
        serde_json::Value::String(s) => Some(toml::Value::String(s)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(toml::Value::Integer(i))
            } else if let Some(u) = n.as_u64() {
                // TOML integers are i64; clamp pathological u64 values.
                Some(toml::Value::Integer(i64::try_from(u).unwrap_or(i64::MAX)))
            } else {
                n.as_f64().map(toml::Value::Float)
            }
        }
        serde_json::Value::Array(items) => Some(toml::Value::Array(
            items.into_iter().filter_map(json_to_toml).collect(),
        )),
        serde_json::Value::Object(map) => {
            let mut table = toml::map::Map::new();
            for (k, val) in map {
                if let Some(tv) = json_to_toml(val) {
                    table.insert(k, tv);
                }
            }
            Some(toml::Value::Table(table))
        }
    }
}

/// Validate that an alias key is safe for use in TOML dotted paths, URLs, and
/// filesystem paths on Windows, macOS, and Linux.
///
/// Allowed: `[a-zA-Z0-9][a-zA-Z0-9_-]{0,62}` — alphanumeric start, then
/// alphanumeric, underscore, or hyphen. Maximum 63 characters.
///
/// Dots are forbidden because they are TOML key separators. Spaces and all
/// other punctuation are forbidden to stay safe across OS path APIs and URL
/// path segments.
pub fn validate_alias_key(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("alias must not be empty".to_string());
    }
    if key.len() > 63 {
        return Err(format!(
            "alias '{}' is too long ({} chars); maximum is 63",
            key,
            key.len()
        ));
    }
    let mut chars = key.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphanumeric() {
        return Err(format!("alias '{}' must start with a letter or digit", key));
    }
    for ch in chars {
        if !matches!(ch, 'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_') {
            return Err(format!(
                "alias '{}' contains invalid character {:?}; \
                 only letters, digits, hyphens, and underscores are allowed",
                key, ch
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_string_array_splits_on_comma() {
        let result = parse_prop_value("alice, bob, charlie", PropKind::StringArray).unwrap();
        let arr = result.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0].as_str(), Some("alice"));
        assert_eq!(arr[1].as_str(), Some("bob"));
        assert_eq!(arr[2].as_str(), Some("charlie"));
    }

    #[test]
    fn parse_string_array_empty_input_gives_empty_array() {
        let result = parse_prop_value("", PropKind::StringArray).unwrap();
        assert_eq!(result.as_array().unwrap().len(), 0);
    }

    #[test]
    fn parse_string_array_single_value() {
        let result = parse_prop_value("alice", PropKind::StringArray).unwrap();
        let arr = result.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].as_str(), Some("alice"));
    }

    #[test]
    fn parse_string_array_quote_in_value_is_literal() {
        let result = parse_prop_value(r#"tok1, p@ss"word"#, PropKind::StringArray).unwrap();
        let arr = result.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0].as_str(), Some("tok1"));
        assert_eq!(arr[1].as_str(), Some(r#"p@ss"word"#));
    }

    // ── validate_alias_key ────────────────────────────────────────────────

    #[test]
    fn validate_alias_key_accepts_simple_names() {
        assert!(validate_alias_key("default").is_ok());
        assert!(validate_alias_key("work").is_ok());
        assert!(validate_alias_key("my-alias").is_ok());
        assert!(validate_alias_key("my_alias").is_ok());
        assert!(validate_alias_key("alias123").is_ok());
        assert!(validate_alias_key("A").is_ok());
        assert!(validate_alias_key("z9-Z").is_ok());
    }

    #[test]
    fn validate_alias_key_rejects_empty() {
        assert!(validate_alias_key("").is_err());
    }

    #[test]
    fn validate_alias_key_rejects_dot() {
        // dots break TOML dotted-key path parsing
        let err = validate_alias_key("my.alias").unwrap_err();
        assert!(err.contains("invalid character"), "{err}");
    }

    #[test]
    fn validate_alias_key_rejects_slash() {
        let err = validate_alias_key("my/alias").unwrap_err();
        assert!(err.contains("invalid character"), "{err}");
    }

    #[test]
    fn validate_alias_key_rejects_space() {
        let err = validate_alias_key("my alias").unwrap_err();
        assert!(err.contains("invalid character"), "{err}");
    }

    #[test]
    fn validate_alias_key_rejects_leading_hyphen() {
        let err = validate_alias_key("-bad").unwrap_err();
        assert!(err.contains("must start with"), "{err}");
    }

    #[test]
    fn validate_alias_key_rejects_leading_underscore() {
        let err = validate_alias_key("_bad").unwrap_err();
        assert!(err.contains("must start with"), "{err}");
    }

    #[test]
    fn validate_alias_key_rejects_over_63_chars() {
        let long = "a".repeat(64);
        let err = validate_alias_key(&long).unwrap_err();
        assert!(err.contains("too long"), "{err}");
    }

    #[test]
    fn validate_alias_key_accepts_exactly_63_chars() {
        let at_limit = "a".repeat(63);
        assert!(validate_alias_key(&at_limit).is_ok());
    }

    #[test]
    fn validate_alias_key_rejects_windows_reserved_chars() {
        for ch in [':', '*', '?', '"', '<', '>', '|', '\\'] {
            let key = format!("alias{ch}name");
            assert!(
                validate_alias_key(&key).is_err(),
                "expected rejection of char {ch:?} in alias key"
            );
        }
    }
}
