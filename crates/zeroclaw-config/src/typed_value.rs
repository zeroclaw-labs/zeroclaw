//! Strictly-typed JSON-to-`Config::set_prop` value coercion.
//!
//! Both the gateway HTTP CRUD layer and the CLI (`zeroclaw config patch`)
//! receive incoming values as `serde_json::Value` and need to hand them to
//! `Config::set_prop`, which takes a `&str`. The naive coercion (just JSON
//! stringify everything) loses type safety: a JSON array passed where a
//! scalar is expected silently round-trips through string parsing instead
//! of being rejected. Worse: the two surfaces had divergent coercion logic
//! — the HTTP layer enforced types, the CLI accepted whatever.
//!
//! This module is the single source of truth. Both surfaces consult the
//! field's declared `PropKind` and reject shape mismatches with a
//! `value_type_mismatch` error before the value reaches `set_prop`.
//!
//! Issue #6175.

use crate::api_error::{ConfigApiCode, ConfigApiError};
use crate::traits::PropKind;

/// Coerce a JSON value to the string representation `Config::set_prop`
/// expects, validating against the target field's declared `PropKind`.
///
/// Type rules:
/// - `StringArray`: JSON array of strings; rejects non-array, rejects
///   non-string elements (with offending index in the message). Empty
///   array `[]` is valid and distinct from `null`.
/// - `Bool`: JSON boolean (or string `"true"` / `"false"` for legacy
///   callers).
/// - `Integer`: JSON number with integer value (or numeric string).
/// - `Float`: JSON number (or numeric string).
/// - `String` / `Enum`: any scalar coerces to its display form.
/// - `null`: always valid; means "reset to default".
///
/// `kind` may be `None` for paths whose declared kind isn't known to the
/// caller (e.g. enum-shaped fields the introspection layer surfaces as
/// `Enum`); in that case we fall through to the existing best-effort
/// coercion that mirrors `set_prop`'s own string parser.
pub fn coerce_for_set_prop(
    value: &serde_json::Value,
    kind: Option<PropKind>,
) -> Result<String, ConfigApiError> {
    match (kind, value) {
        // Null is always valid — it means "reset to default".
        (_, serde_json::Value::Null) => Ok(String::new()),

        // Array fields: must receive a JSON array of strings.
        (Some(PropKind::StringArray), serde_json::Value::Array(items)) => {
            for (i, item) in items.iter().enumerate() {
                if !item.is_string() {
                    return Err(ConfigApiError::new(
                        ConfigApiCode::ValueTypeMismatch,
                        format!(
                            "array element [{i}] is {} — `Vec<String>` requires string elements",
                            json_type_name(item),
                        ),
                    ));
                }
            }
            serde_json::to_string(value).map_err(|e| {
                ConfigApiError::new(
                    ConfigApiCode::ValueTypeMismatch,
                    format!("could not serialize JSON value: {e}"),
                )
            })
        }
        (Some(PropKind::StringArray), other) => Err(ConfigApiError::new(
            ConfigApiCode::ValueTypeMismatch,
            format!(
                "`Vec<String>` field requires a JSON array; got {}",
                json_type_name(other),
            ),
        )),

        // `Vec<T>` of objects: any JSON array is acceptable; element shape
        // is validated by serde when `set_prop` deserializes back into the
        // target type. We just pass the JSON through verbatim.
        (Some(PropKind::ObjectArray), serde_json::Value::Array(_)) => serde_json::to_string(value)
            .map_err(|e| {
                ConfigApiError::new(
                    ConfigApiCode::ValueTypeMismatch,
                    format!("could not serialize JSON value: {e}"),
                )
            }),
        (Some(PropKind::ObjectArray), other) => Err(ConfigApiError::new(
            ConfigApiCode::ValueTypeMismatch,
            format!(
                "object-array field requires a JSON array of objects; got {}",
                json_type_name(other),
            ),
        )),

        // Bool fields.
        (Some(PropKind::Bool), serde_json::Value::Bool(b)) => Ok(b.to_string()),
        (Some(PropKind::Bool), serde_json::Value::String(s))
            if s.eq_ignore_ascii_case("true") || s.eq_ignore_ascii_case("false") =>
        {
            Ok(s.to_lowercase())
        }
        (Some(PropKind::Bool), other) => Err(ConfigApiError::new(
            ConfigApiCode::ValueTypeMismatch,
            format!(
                "bool field requires `true`/`false`; got {}",
                json_type_name(other)
            ),
        )),

        // Integer fields.
        (Some(PropKind::Integer), serde_json::Value::Number(n)) if n.is_i64() || n.is_u64() => {
            Ok(n.to_string())
        }
        (Some(PropKind::Integer), serde_json::Value::String(s)) if s.parse::<i64>().is_ok() => {
            Ok(s.clone())
        }
        (Some(PropKind::Integer), other) => Err(ConfigApiError::new(
            ConfigApiCode::ValueTypeMismatch,
            format!(
                "integer field requires a whole number; got {}",
                json_type_name(other)
            ),
        )),

        // Float fields.
        (Some(PropKind::Float), serde_json::Value::Number(n)) => Ok(n.to_string()),
        (Some(PropKind::Float), serde_json::Value::String(s)) if s.parse::<f64>().is_ok() => {
            Ok(s.clone())
        }
        (Some(PropKind::Float), other) => Err(ConfigApiError::new(
            ConfigApiCode::ValueTypeMismatch,
            format!(
                "float field requires a number; got {}",
                json_type_name(other)
            ),
        )),

        // Scalar / enum fields and unknown-kind paths: best-effort coerce.
        (_, serde_json::Value::String(s)) => Ok(s.clone()),
        (_, serde_json::Value::Bool(b)) => Ok(b.to_string()),
        (_, serde_json::Value::Number(n)) => Ok(n.to_string()),
        (_, serde_json::Value::Array(_)) | (_, serde_json::Value::Object(_)) => {
            serde_json::to_string(value).map_err(|e| {
                ConfigApiError::new(
                    ConfigApiCode::ValueTypeMismatch,
                    format!("could not serialize JSON value: {e}"),
                )
            })
        }
    }
}

fn json_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_resets_to_default_regardless_of_kind() {
        for k in [
            None,
            Some(PropKind::String),
            Some(PropKind::Bool),
            Some(PropKind::Integer),
            Some(PropKind::Float),
            Some(PropKind::Enum),
            Some(PropKind::StringArray),
        ] {
            assert_eq!(
                coerce_for_set_prop(&serde_json::Value::Null, k).unwrap(),
                ""
            );
        }
    }

    #[test]
    fn string_array_rejects_non_array() {
        let err = coerce_for_set_prop(
            &serde_json::Value::String("a,b".into()),
            Some(PropKind::StringArray),
        )
        .unwrap_err();
        assert_eq!(err.code, ConfigApiCode::ValueTypeMismatch);
        assert!(err.message.contains("Vec<String>"));
        assert!(err.message.contains("string"));
    }

    #[test]
    fn string_array_rejects_non_string_element_with_index() {
        let err = coerce_for_set_prop(
            &serde_json::json!(["a", 42, "c"]),
            Some(PropKind::StringArray),
        )
        .unwrap_err();
        assert_eq!(err.code, ConfigApiCode::ValueTypeMismatch);
        // Surfaces the offending index so the user can find the bad element.
        assert!(err.message.contains("[1]"));
    }

    #[test]
    fn empty_array_valid_for_string_array() {
        let s = coerce_for_set_prop(&serde_json::json!([]), Some(PropKind::StringArray)).unwrap();
        assert_eq!(s, "[]");
    }

    #[test]
    fn bool_field_rejects_non_bool_string() {
        let err = coerce_for_set_prop(
            &serde_json::Value::String("yes".into()),
            Some(PropKind::Bool),
        )
        .unwrap_err();
        assert_eq!(err.code, ConfigApiCode::ValueTypeMismatch);
    }

    #[test]
    fn bool_field_accepts_legacy_string() {
        // Legacy clients that pass "True" / "false" as a string still work.
        assert_eq!(
            coerce_for_set_prop(
                &serde_json::Value::String("True".into()),
                Some(PropKind::Bool)
            )
            .unwrap(),
            "true"
        );
    }

    #[test]
    fn integer_field_rejects_float() {
        // Use a non-pi float so clippy's approx_constant lint doesn't flag.
        let err =
            coerce_for_set_prop(&serde_json::json!(2.5_f64), Some(PropKind::Integer)).unwrap_err();
        assert_eq!(err.code, ConfigApiCode::ValueTypeMismatch);
    }

    #[test]
    fn unknown_kind_falls_back_to_string_form() {
        // Backward-compat for callers without PropKind context.
        assert_eq!(
            coerce_for_set_prop(&serde_json::json!(true), None).unwrap(),
            "true"
        );
        assert_eq!(
            coerce_for_set_prop(&serde_json::json!(["a", "b"]), None).unwrap(),
            "[\"a\",\"b\"]"
        );
    }
}
