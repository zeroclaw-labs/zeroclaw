//! Property helpers used by the `Configurable` derive macro and the `zeroclaw config` CLI.

use crate::traits::{ConfigTab, CredentialSurfaceClass, PropFieldInfo, PropKind};

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

/// True when `e` is the "Unknown property" marker produced by the
/// `Configurable` derive's `set_prop`/`get_prop` fallback (and by
/// `prop_name_to_serde_field` in this module). Namespace-sharing nested
/// delegation sites (serde-flatten, `Option<T>`, dotted-key candidate
/// loops) use it to tell "not one of mine — keep trying siblings" apart
/// from a real value error on a confirmed path (#9285). The same prefix
/// convention is what downstream consumers branch on
/// (`config_patch_map_prop_error` in `src/main.rs`, `map_prop_error` in
/// `zeroclaw-gateway`).
///
/// Both constructors build the error via `anyhow::Error::msg(String)`, so
/// the downcast is the common zero-alloc path; `to_string()` is only a
/// fallback for wrapped/contextualized errors.
pub fn is_unknown_property_error(e: &anyhow::Error) -> bool {
    if let Some(msg) = e.downcast_ref::<String>() {
        return msg.starts_with("Unknown property");
    }
    e.to_string().starts_with("Unknown property")
}

pub fn route_vec_path<'a, 'k, I>(
    name: &'a str,
    my_prefix: &str,
    field_name: &str,
    inner_prefix: &str,
    natural_keys: I,
) -> VecRoute<'a>
where
    I: IntoIterator<Item = (usize, &'k str)>,
{
    let key_prefix = if my_prefix.is_empty() {
        field_name.to_string()
    } else {
        format!("{my_prefix}.{field_name}")
    };
    let Some(rest) = name
        .strip_prefix(&key_prefix)
        .and_then(|s| s.strip_prefix('.'))
    else {
        return VecRoute::Miss;
    };

    // Longest-match against live natural keys, collecting every index that
    // hits the same maximal key so duplicates surface as Ambiguous.
    let mut best_len: Option<usize> = None;
    let mut best_hits: Vec<usize> = Vec::new();
    for (idx, key) in natural_keys {
        if key.is_empty() {
            // Empty natural-key slots can't be addressed by name; skip so
            // they don't soak up every otherwise-unmatched path via the
            // empty prefix.
            continue;
        }
        if rest
            .strip_prefix(key)
            .and_then(|s| s.strip_prefix('.'))
            .is_some()
            || rest == key
        {
            match best_len {
                Some(len) if key.len() < len => continue,
                Some(len) if key.len() == len => best_hits.push(idx),
                _ => {
                    best_len = Some(key.len());
                    best_hits.clear();
                    best_hits.push(idx);
                }
            }
        }
    }
    let Some(key_len) = best_len else {
        return VecRoute::Miss;
    };
    let key_slice = &rest[..key_len];
    if best_hits.len() > 1 {
        return VecRoute::Ambiguous {
            key: key_slice,
            count: best_hits.len(),
        };
    }
    let idx = best_hits[0];
    // `rest == key` means the inner suffix is empty (i.e. the path
    // addresses the element itself, not a property on it). Treat as a
    // miss for routing-into-properties; callers asking about the whole
    // element use map_keys / map_key_create instead.
    if rest.len() == key_len {
        return VecRoute::Miss;
    }
    let inner_suffix = &rest[key_len + 1..];
    let inner_name = if inner_prefix.is_empty() {
        inner_suffix.to_string()
    } else {
        format!("{inner_prefix}.{inner_suffix}")
    };
    VecRoute::Hit {
        index: idx,
        inner_name,
    }
}

/// Outcome of routing a `<prefix>.<field>.<natural_key>.<suffix>` path
/// into a `#[nested] Vec<T>` field. See [`route_vec_path`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VecRoute<'a> {
    /// Path is not addressed at this field at all (wrong prefix, or
    /// addresses the element itself rather than a sub-property). Caller
    /// should fall through to the next nested field.
    Miss,
    /// Path resolved to a unique element. `index` is the element's
    /// position in the underlying `Vec`; `inner_name` is the property
    /// path to pass to `T::get_prop` / `T::set_prop`.
    Hit {
        index: usize,
        inner_name: String,
    },
    Ambiguous {
        key: &'a str,
        count: usize,
    },
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
    credential_class: Option<CredentialSurfaceClass>,
    tab: ConfigTab,
    display_secret_terminals: &[&str],
    alias_source: Option<crate::traits::AliasSource>,
    multiline: bool,
) -> PropFieldInfo {
    let display_value = if is_secret || derived_from_secret {
        match table.and_then(|t| t.get(serde_name)) {
            Some(toml::Value::String(s)) if !s.is_empty() => "****".to_string(),
            Some(toml::Value::Array(arr)) if !arr.is_empty() => {
                format!("[{}]", vec!["****"; arr.len()].join(", "))
            }
            _ => crate::traits::UNSET_DISPLAY.to_string(),
        }
    } else {
        toml_value_to_display_for_kind(
            table.and_then(|t| t.get(serde_name)),
            kind,
            display_secret_terminals,
        )
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
        credential_class,
        tab,
        alias_source,
        multiline,
    }
}

/// Get a property value via serde serialization.
pub fn serde_get_prop<T: serde::Serialize>(
    target: &T,
    prefix: &str,
    name: &str,
    is_secret: bool,
    kind: PropKind,
    display_secret_terminals: &[&str],
) -> anyhow::Result<String> {
    if is_secret {
        return Ok("**** (encrypted)".to_string());
    }
    let serde_name = prop_name_to_serde_field(prefix, name)?;
    let table = toml::Value::try_from(target)?;
    Ok(toml_value_to_display_for_kind(
        table.as_table().and_then(|t| t.get(&serde_name)),
        kind,
        display_secret_terminals,
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
    if (value_str.is_empty() || value_str == crate::traits::UNSET_DISPLAY || value_str == "****")
        && is_option
    {
        table.remove(&serde_name);
    } else {
        table.insert(serde_name, parse_prop_value(value_str, kind)?);
    }
    *target = toml::from_str(&toml::to_string(&table)?)?;
    Ok(())
}

fn toml_value_to_display(value: Option<&toml::Value>) -> String {
    match value {
        None => crate::traits::UNSET_DISPLAY.to_string(),
        Some(toml::Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
    }
}

fn toml_value_to_display_for_kind(
    value: Option<&toml::Value>,
    kind: PropKind,
    display_secret_terminals: &[&str],
) -> String {
    match kind {
        PropKind::Object | PropKind::ObjectArray => match value {
            None => crate::traits::UNSET_DISPLAY.to_string(),
            Some(toml::Value::String(s)) => s.clone(),
            Some(v) => {
                let mut redacted = v.clone();
                redact_toml_display_secrets(&mut redacted, display_secret_terminals);
                redacted.to_string()
            }
        },
        _ => toml_value_to_display(value),
    }
}

pub fn object_array_json_display_value(
    value: &impl serde::Serialize,
    display_secret_terminals: &[&str],
) -> String {
    match serde_json::to_value(value) {
        Ok(mut value) => {
            redact_json_display_secrets(&mut value, display_secret_terminals);
            serde_json::to_string(&value).unwrap_or_else(|_| "[]".to_string())
        }
        Err(_) => "[]".to_string(),
    }
}

fn redact_json_display_secrets(value: &mut serde_json::Value, display_secret_terminals: &[&str]) {
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                redact_json_display_secrets(item, display_secret_terminals);
            }
        }
        serde_json::Value::Object(map) => {
            for (key, nested) in map.iter_mut() {
                if display_key_is_secret_terminal(key, display_secret_terminals) {
                    mask_json_value(nested);
                } else {
                    redact_json_display_secrets(nested, display_secret_terminals);
                }
            }
        }
        _ => {}
    }
}

fn redact_toml_display_secrets(value: &mut toml::Value, display_secret_terminals: &[&str]) {
    match value {
        toml::Value::Array(items) => {
            for item in items {
                redact_toml_display_secrets(item, display_secret_terminals);
            }
        }
        toml::Value::Table(table) => {
            for (key, nested) in table.iter_mut() {
                if display_key_is_secret_terminal(key, display_secret_terminals) {
                    mask_toml_value(nested);
                } else {
                    redact_toml_display_secrets(nested, display_secret_terminals);
                }
            }
        }
        _ => {}
    }
}

fn mask_json_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                mask_json_value(item);
            }
        }
        serde_json::Value::Object(map) => {
            for nested in map.values_mut() {
                mask_json_value(nested);
            }
        }
        serde_json::Value::Null => {}
        _ => *value = serde_json::Value::String("****".to_string()),
    }
}

fn mask_toml_value(value: &mut toml::Value) {
    match value {
        toml::Value::Array(items) => {
            for item in items {
                mask_toml_value(item);
            }
        }
        toml::Value::Table(table) => {
            for (_, nested) in table.iter_mut() {
                mask_toml_value(nested);
            }
        }
        _ => *value = toml::Value::String("****".to_string()),
    }
}

fn display_key_is_secret_terminal(key: &str, display_secret_terminals: &[&str]) -> bool {
    let normalized = normalize_display_key(key);
    display_secret_terminals
        .iter()
        .any(|terminal| normalize_display_key(terminal) == normalized)
}

fn normalize_display_key(key: &str) -> String {
    key.replace('-', "_").to_ascii_lowercase()
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
        PropKind::String | PropKind::Enum | PropKind::AliasRef => {
            Ok(toml::Value::String(value_str.to_string()))
        }
        PropKind::StringArray => {
            let trimmed = value_str.trim();
            // Accept JSON/TOML array syntax: ["a", "b", "c"]
            if trimmed.starts_with('[')
                && let Ok(arr) = serde_json::from_str::<Vec<String>>(trimmed)
            {
                return Ok(toml::Value::Array(
                    arr.into_iter()
                        .filter(|s| !s.is_empty() && s != crate::traits::UNSET_DISPLAY)
                        .map(toml::Value::String)
                        .collect(),
                ));
            }
            // Fall back to comma-separated input.
            let items = value_str
                .split(',')
                .map(|s| toml::Value::String(s.trim().to_string()))
                .filter(|v| {
                    v.as_str()
                        .is_some_and(|s| !s.is_empty() && s != crate::traits::UNSET_DISPLAY)
                })
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

#[must_use]
pub fn resolve_field_path(known_paths: &[String], raw: &str) -> String {
    let raw_segs: Vec<&str> = raw.split('.').collect();
    for known in known_paths {
        let known_segs: Vec<&str> = known.split('.').collect();
        if known_segs.len() != raw_segs.len() {
            continue;
        }
        let final_index = known_segs.len().saturating_sub(1);
        let all_match = known_segs
            .iter()
            .zip(raw_segs.iter())
            .enumerate()
            .all(|(idx, (k, r))| {
                k == r
                    || (k.contains('-') && k.replace('-', "_") == **r)
                    || (idx == final_index && k.contains('_') && k.replace('_', "-") == **r)
            });
        if all_match {
            return known.clone();
        }
    }
    raw.to_string()
}

pub fn kebab_to_snake(key: &str) -> String {
    key.replace('-', "_")
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

    // ── route_vec_path ────────────────────────────────────────────────────

    #[test]
    fn route_vec_path_hits_single_element_with_inner_suffix() {
        let keys = [(0usize, "fs"), (1, "github")];
        let got = route_vec_path(
            "mcp.servers.fs.url",
            "mcp",
            "servers",
            "mcp.servers",
            keys.iter().copied(),
        );
        assert_eq!(
            got,
            VecRoute::Hit {
                index: 0,
                inner_name: "mcp.servers.url".to_string(),
            }
        );
    }

    #[test]
    fn route_vec_path_routes_deep_inner_path() {
        // McpServerConfig has `headers` which is a HashMap; full path is
        // `mcp.servers.<name>.headers.<header-key>`. The router must
        // forward `headers.<header-key>` as the inner name.
        let keys = [(0usize, "primary")];
        let got = route_vec_path(
            "mcp.servers.primary.headers.authorization",
            "mcp",
            "servers",
            "mcp.servers",
            keys.iter().copied(),
        );
        assert_eq!(
            got,
            VecRoute::Hit {
                index: 0,
                inner_name: "mcp.servers.headers.authorization".to_string(),
            }
        );
    }

    #[test]
    fn route_vec_path_misses_on_wrong_prefix() {
        let keys = [(0usize, "fs")];
        let got = route_vec_path(
            "providers.models.anthropic.default.api-key",
            "mcp",
            "servers",
            "mcp.servers",
            keys.iter().copied(),
        );
        assert_eq!(got, VecRoute::Miss);
    }

    #[test]
    fn route_vec_path_misses_on_unknown_natural_key() {
        let keys = [(0usize, "fs"), (1, "github")];
        let got = route_vec_path(
            "mcp.servers.unknown.url",
            "mcp",
            "servers",
            "mcp.servers",
            keys.iter().copied(),
        );
        assert_eq!(got, VecRoute::Miss);
    }

    #[test]
    fn route_vec_path_picks_longest_dotted_natural_key() {
        // An operator may name an MCP server `acme.tools` (TOML allows
        // any string for `name`). The router must prefer the longer
        // key when one is a prefix of another.
        let keys = [(0usize, "acme"), (1, "acme.tools")];
        let got = route_vec_path(
            "mcp.servers.acme.tools.url",
            "mcp",
            "servers",
            "mcp.servers",
            keys.iter().copied(),
        );
        assert_eq!(
            got,
            VecRoute::Hit {
                index: 1,
                inner_name: "mcp.servers.url".to_string(),
            }
        );
    }

    #[test]
    fn route_vec_path_reports_ambiguous_duplicates() {
        // Two entries share the same `name`. validate_mcp_config catches
        // this at save time, but until the user repairs the config the
        // router must refuse to silently edit one of them.
        let keys = [(0usize, "fs"), (1, "fs")];
        let got = route_vec_path(
            "mcp.servers.fs.url",
            "mcp",
            "servers",
            "mcp.servers",
            keys.iter().copied(),
        );
        assert_eq!(
            got,
            VecRoute::Ambiguous {
                key: "fs",
                count: 2,
            }
        );
    }

    #[test]
    fn route_vec_path_skips_empty_natural_keys() {
        // A newly-inserted entry that hasn't had `name` set yet must
        // not match every otherwise-unmatched path via the empty key.
        let keys = [(0usize, ""), (1, "fs")];
        let got = route_vec_path(
            "mcp.servers.fs.url",
            "mcp",
            "servers",
            "mcp.servers",
            keys.iter().copied(),
        );
        assert_eq!(
            got,
            VecRoute::Hit {
                index: 1,
                inner_name: "mcp.servers.url".to_string(),
            }
        );
    }

    #[test]
    fn route_vec_path_treats_bare_element_path_as_miss() {
        // `mcp.servers.fs` with no trailing field addresses the element
        // itself, not a property; routing into properties must miss so
        // callers fall through to map_keys-shaped APIs.
        let keys = [(0usize, "fs")];
        let got = route_vec_path(
            "mcp.servers.fs",
            "mcp",
            "servers",
            "mcp.servers",
            keys.iter().copied(),
        );
        assert_eq!(got, VecRoute::Miss);
    }

    #[test]
    fn route_vec_path_honours_my_prefix() {
        // Same field name nested under a non-empty outer prefix
        // (the way the derive will actually call it for the top-level
        // Mcp section: my_prefix="mcp", field_name="servers").
        let keys = [(0usize, "fs")];
        let got = route_vec_path(
            "mcp.servers.fs.transport",
            "mcp",
            "servers",
            "mcp.servers",
            keys.iter().copied(),
        );
        assert_eq!(
            got,
            VecRoute::Hit {
                index: 0,
                inner_name: "mcp.servers.transport".to_string(),
            }
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
    fn parse_string_array_drops_unset_sentinel() {
        let bare = parse_prop_value(crate::traits::UNSET_DISPLAY, PropKind::StringArray).unwrap();
        assert_eq!(bare.as_array().unwrap().len(), 0);
        let json = parse_prop_value(r#"["<unset>", "/real"]"#, PropKind::StringArray).unwrap();
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].as_str(), Some("/real"));
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

    #[test]
    fn resolve_field_path_canonicalizes_snake_field_segments() {
        let known = vec![
            "providers.models.anthropic.my_bot.api-key".to_string(),
            "providers.models.anthropic.my_bot.model".to_string(),
        ];
        // User typed snake `api_key`; alias `my_bot` stays untouched
        // because the canonical segment has no `-`.
        assert_eq!(
            resolve_field_path(&known, "providers.models.anthropic.my_bot.api_key"),
            "providers.models.anthropic.my_bot.api-key",
        );
    }

    #[test]
    fn resolve_field_path_passes_through_canonical_input() {
        let known = vec!["providers.models.anthropic.my_bot.api-key".to_string()];
        assert_eq!(
            resolve_field_path(&known, "providers.models.anthropic.my_bot.api-key"),
            "providers.models.anthropic.my_bot.api-key",
        );
    }

    #[test]
    fn resolve_field_path_canonicalizes_kebab_final_field_segments() {
        let known = vec!["providers.models.deepseek.default.api_key".to_string()];
        assert_eq!(
            resolve_field_path(&known, "providers.models.deepseek.default.api-key"),
            "providers.models.deepseek.default.api_key",
        );
    }

    #[test]
    fn resolve_field_path_returns_raw_when_no_match() {
        let known: Vec<String> = vec![];
        assert_eq!(resolve_field_path(&known, "no.such.path"), "no.such.path");
    }

    #[test]
    fn resolve_field_path_does_not_corrupt_snake_alias() {
        // `my_bot` is an alias; user typed it correctly; we must not
        // turn it into `my-bot` while resolving an api_key snake input.
        let known = vec!["providers.models.anthropic.my_bot.api-key".to_string()];
        let resolved = resolve_field_path(&known, "providers.models.anthropic.my_bot.api_key");
        assert!(resolved.contains("my_bot"));
        assert!(!resolved.contains("my-bot"));
    }

    #[test]
    fn kebab_to_snake_converts_hyphens() {
        assert_eq!(kebab_to_snake("api-key"), "api_key");
        assert_eq!(kebab_to_snake("bot-token"), "bot_token");
        assert_eq!(kebab_to_snake("allowed-users"), "allowed_users");
        assert_eq!(kebab_to_snake("external-peers"), "external_peers");
    }

    #[test]
    fn kebab_to_snake_noop_for_plain_keys() {
        assert_eq!(kebab_to_snake("uri"), "uri");
        assert_eq!(kebab_to_snake("model"), "model");
        assert_eq!(kebab_to_snake(""), "");
    }
}
