//! Property helpers used by the `Configurable` derive macro and the `zeroclaw config` CLI.

use crate::traits::{PropFieldInfo, PropKind};

/// For a `#[nested] HashMap<String, T>` field, parse a `get_prop`/`set_prop`
/// path of the form `<my_prefix>.<field_name>.<hm_key>.<inner_suffix>` and
/// return the HashMap key + the fully-qualified inner name that the value
/// type's own `get_prop` / `set_prop` expects.
///
/// HashMap keys are user-controlled and may contain dots, URLs, or hostnames
/// (for example `model_providers.custom:https://example.invalid/v1.api-key`).
/// Inner values may themselves be deeply nested (`AliasedAgentConfig` has
/// `agent.thinking.<...>` subpaths), so neither left-splitting nor
/// right-splitting works in isolation. Match against the actual present
/// keys and pick the longest prefix that is followed by `.` — this
/// correctly handles dotted keys *and* deep inner paths in one parse.
///
/// `keys` is an iterator over the live HashMap's keys (typically
/// `self.<field>.keys().map(String::as_str)` from the derive). Returns
/// `None` when the path doesn't match, letting the derive's generated
/// code fall through to the next nested field.
pub fn route_hashmap_path<'a, 'k, I>(
    name: &'a str,
    my_prefix: &str,
    field_name: &str,
    inner_prefix: &str,
    keys: I,
) -> Option<(&'a str, String)>
where
    I: IntoIterator<Item = &'k str>,
{
    let key_prefix = if my_prefix.is_empty() {
        field_name.to_string()
    } else {
        format!("{my_prefix}.{field_name}")
    };
    let rest = name.strip_prefix(&key_prefix)?.strip_prefix('.')?;
    // Longest-match against present map keys. Dotted keys (URL-shaped
    // custom provider entries) sort longer than their unprefixed siblings,
    // so this also disambiguates `custom:https://x` vs. `custom`.
    let mut best: Option<(usize, &'a str)> = None;
    for k in keys {
        if let Some(_suffix) = rest.strip_prefix(k).and_then(|s| s.strip_prefix('.'))
            && best.is_none_or(|(len, _)| k.len() > len)
        {
            // Slice the original `rest` so we can keep the lifetime tied
            // to `name` rather than to a transient `&str` from the keys
            // iterator.
            let hm_key = &rest[..k.len()];
            best = Some((k.len(), hm_key));
        }
    }
    let (key_len, hm_key) = best?;
    let inner_suffix = &rest[key_len + 1..];
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
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"prefix": prefix, "name": name})),
                    "prop_name_to_serde_field: property name does not share the configured prefix"
                );
                anyhow::Error::msg(format!("Unknown property '{name}'"))
            })?
    };
    let field_part = suffix.split('.').next().unwrap_or(suffix);
    Ok(field_part.replace('-', "_"))
}

fn parse_prop_value(value_str: &str, kind: PropKind) -> anyhow::Result<toml::Value> {
    let reject = |reason: &'static str, attrs: serde_json::Value| {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(attrs),
            "parse_prop_value rejected input"
        );
        let _ = reason;
    };
    match kind {
        PropKind::Bool => Ok(toml::Value::Boolean(value_str.parse().map_err(|_| {
            reject(
                "bool",
                ::serde_json::json!({"kind": "bool", "got_len": value_str.len()}),
            );
            anyhow::Error::msg(format!(
                "Invalid bool value '{value_str}', expected 'true' or 'false'"
            ))
        })?)),
        PropKind::Integer => Ok(toml::Value::Integer(value_str.parse().map_err(|_| {
            reject(
                "integer",
                ::serde_json::json!({"kind": "integer", "got_len": value_str.len()}),
            );
            anyhow::Error::msg(format!("Invalid integer value '{value_str}'"))
        })?)),
        PropKind::Float => Ok(toml::Value::Float(value_str.parse().map_err(|_| {
            reject(
                "float",
                ::serde_json::json!({"kind": "float", "got_len": value_str.len()}),
            );
            anyhow::Error::msg(format!("Invalid float value '{value_str}'"))
        })?)),
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
        // dropped because TOML has no null - the absent key conveys the
        // same meaning when the field deserializes back into `Option<T>`.
        PropKind::ObjectArray => {
            let v: serde_json::Value = serde_json::from_str(value_str).map_err(|e| {
                reject(
                    "object_array",
                    ::serde_json::json!({"kind": "object_array", "error": format!("{}", e)}),
                );
                anyhow::Error::msg(format!("invalid JSON array of objects: {e}"))
            })?;
            json_to_toml(v).ok_or_else(|| {
                reject(
                    "object_array_nulls",
                    ::serde_json::json!({"kind": "object_array", "reason": "all-null"}),
                );
                anyhow::Error::msg("JSON value contained only nulls, nothing to write")
            })
        }
        // Struct-shaped scalar: parse the JSON object into a TOML table so
        // the parent serde round-trip deserializes into the typed struct
        // (e.g. `Option<ModelPricing>`). Inserting a raw String here would
        // fail serde because the field is typed, not free-form text.
        PropKind::Object => {
            let v: serde_json::Value = serde_json::from_str(value_str).map_err(|e| {
                reject(
                    "object",
                    ::serde_json::json!({"kind": "object", "error": format!("{}", e)}),
                );
                anyhow::Error::msg(format!("invalid JSON object: {e}"))
            })?;
            if !matches!(v, serde_json::Value::Object(_)) {
                reject(
                    "object_shape",
                    ::serde_json::json!({"kind": "object", "got_shape": "non-object"}),
                );
                anyhow::bail!("Object field requires a JSON object; got {v}");
            }
            json_to_toml(v).ok_or_else(|| {
                reject(
                    "object_nulls",
                    ::serde_json::json!({"kind": "object", "reason": "all-null"}),
                );
                anyhow::Error::msg("JSON object contained only nulls, nothing to write")
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

/// Validate that an alias key is safe for use in TOML dotted paths, URLs,
/// filesystem paths on Windows/macOS/Linux, and `ZEROCLAW_*` env-var grammar.
///
/// Allowed: lowercase ASCII alphanumeric plus single underscore, 1-63 chars.
/// Must start AND end with alphanumeric. Adjacent underscores (`__`) are
/// forbidden because they collide with the env-var grammar's path separator.
///
/// The env-var grammar uses `__` as path separator, which lets aliases keep
/// single `_` literally (`prod_v2`, `staging_api`). Hyphens are forbidden
/// because they are illegal in POSIX env-var identifiers; uppercase is
/// forbidden so the bootstrap env-vars (`ZEROCLAW_WORKSPACE`,
/// `ZEROCLAW_CONFIG_DIR`) stay disambiguated by case.
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
    let first = key.chars().next().unwrap();
    let last = key.chars().next_back().unwrap();
    if !matches!(first, 'a'..='z' | '0'..='9') {
        return Err(format!(
            "alias '{key}' must start with a lowercase letter or digit"
        ));
    }
    if !matches!(last, 'a'..='z' | '0'..='9') {
        return Err(format!(
            "alias '{key}' must end with a lowercase letter or digit"
        ));
    }
    if key.contains("__") {
        return Err(format!(
            "alias '{key}' must not contain `__`; it is reserved as the env-var grammar's path separator"
        ));
    }
    for ch in key.chars() {
        if !matches!(ch, 'a'..='z' | '0'..='9' | '_') {
            return Err(format!(
                "alias '{}' contains invalid character {:?}; \
                 only lowercase letters, digits, and single underscores are allowed (no hyphen, no uppercase)",
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
    fn route_hashmap_path_handles_deep_inner_paths() {
        // Regression: AliasedAgentConfig has nested fields like
        // `agent.thinking.<...>` (3+ segments under the alias key). The
        // earlier rsplit-once parser would mis-route, yielding hm_key =
        // "fake123.agent.thinking" instead of "fake123".
        let keys = ["fake123"];
        let got = route_hashmap_path(
            "agents.fake123.agent.thinking.default-level",
            "",
            "agents",
            "",
            keys.iter().copied(),
        );
        assert_eq!(
            got,
            Some(("fake123", "agent.thinking.default-level".to_string()))
        );
    }

    #[test]
    fn route_hashmap_path_picks_longest_dotted_key() {
        // Custom-URL keys may contain dots; the longest matching key
        // wins so `custom:https://example/v1` is preferred over `custom`.
        let keys = ["custom", "custom:https://example/v1"];
        let got = route_hashmap_path(
            "providers.models.custom:https://example/v1.api-key",
            "",
            "providers.models",
            "",
            keys.iter().copied(),
        );
        assert_eq!(
            got,
            Some(("custom:https://example/v1", "api-key".to_string()))
        );
    }

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
    fn validate_alias_key_accepts_lowercase_alphanumeric_with_underscore() {
        assert!(validate_alias_key("default").is_ok());
        assert!(validate_alias_key("work").is_ok());
        assert!(validate_alias_key("alias123").is_ok());
        assert!(validate_alias_key("a").is_ok());
        assert!(validate_alias_key("prod2024").is_ok());
        // V0.8.0: env-var grammar uses `__` as separator, so single `_`
        // inside an alias is unambiguous.
        assert!(validate_alias_key("prod_v2").is_ok());
        assert!(validate_alias_key("staging_api").is_ok());
    }

    #[test]
    fn validate_alias_key_rejects_empty() {
        assert!(validate_alias_key("").is_err());
    }

    #[test]
    fn validate_alias_key_rejects_uppercase() {
        // Leading uppercase trips the start-char rule.
        let err = validate_alias_key("MyAlias").unwrap_err();
        assert!(err.contains("must start with"), "{err}");
        let err = validate_alias_key("A").unwrap_err();
        assert!(err.contains("must start with"), "{err}");
        // Embedded uppercase trips the per-char rule.
        let err = validate_alias_key("myAlias").unwrap_err();
        assert!(err.contains("invalid character"), "{err}");
    }

    #[test]
    fn validate_alias_key_rejects_leading_underscore() {
        let err = validate_alias_key("_bad").unwrap_err();
        assert!(err.contains("must start with"), "{err}");
    }

    #[test]
    fn validate_alias_key_rejects_trailing_underscore() {
        let err = validate_alias_key("bad_").unwrap_err();
        assert!(err.contains("must end with"), "{err}");
    }

    #[test]
    fn validate_alias_key_rejects_double_underscore() {
        let err = validate_alias_key("foo__bar").unwrap_err();
        assert!(err.contains("must not contain `__`"), "{err}");
    }

    #[test]
    fn validate_alias_key_rejects_hyphen() {
        // V0.8.0: hyphens are illegal in env-var identifiers.
        let err = validate_alias_key("my-alias").unwrap_err();
        assert!(err.contains("invalid character"), "{err}");
    }

    #[test]
    fn validate_alias_key_rejects_dot() {
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
