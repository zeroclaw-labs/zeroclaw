//! JSON Patch over the config tree.
//!
//! One implementation for every caller that edits config: the gateway's
//! `PATCH /api/config`, the CLI's `zeroclaw config patch`, and the agent-facing
//! `config_patch` tool. They previously each had their own op loop, and a
//! component test existed only to assert that two of them still agreed on error
//! envelopes — a test that has to exist because the code was duplicated.
//!
//! Applying is deliberately separated from persisting. [`apply_patch_ops`] takes
//! a `&mut Config` and mutates it in memory; the caller decides whether that
//! config is saved, swapped into a running gateway, or thrown away. That is what
//! lets a caller apply the same patch to a *copy* to compute a preview — which
//! is how the agent-facing tool renders a permission change for approval without
//! committing anything.

use serde::{Deserialize, Serialize};

use crate::api_error::{ConfigApiCode, ConfigApiError};
use crate::schema::Config;
use crate::traits::{ConfigTab, CredentialSurfaceClass, PropFieldInfo, PropKind, UNSET_DISPLAY};
use crate::typed_value::coerce_for_set_prop;

/// One requested operation, parsed from a JSON Patch document.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PatchOp {
    pub op: String,
    pub path: String,
    #[serde(default)]
    pub value: Option<serde_json::Value>,
    #[serde(default)]
    pub comment: Option<String>,
}

/// One applied operation, for the caller to render.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PatchOpResult {
    pub op: String,
    pub path: String,
    /// The resulting value at the target path after the op applied.
    /// `None` for secret paths (per the secrets-handling boundary), and for
    /// `remove` ops where the field was reset to its default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub populated: Option<bool>,
    /// Comment that was applied alongside this op (if any). Echoed so
    /// clients can confirm the comment was actually written to disk
    /// without having to round-trip through `GET` and parse the TOML.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

/// `/a/b` (JSON Pointer) to `a.b` (the dotted form every prop API speaks).
/// A path that is already dotted passes through unchanged.
#[must_use]
pub fn json_pointer_to_dotted(path: &str) -> String {
    if path.starts_with('/') {
        path.trim_start_matches('/').replace('/', ".")
    } else {
        path.to_string()
    }
}

/// Classify a `set_prop`/`get_prop` failure. An unknown property is a 404-shaped
/// `path_not_found`; anything else came from validation.
#[must_use]
pub fn map_prop_error(err: anyhow::Error, path: &str) -> ConfigApiError {
    let msg = err.to_string();
    if msg.starts_with("Unknown property") {
        ConfigApiError::path_not_found(path)
    } else {
        ConfigApiError::from_validation(err).with_path(path)
    }
}

/// Prop metadata for a path, used to decide secrecy and coercion kind.
///
/// Falls back to synthesizing secret metadata for paths that `prop_fields`
/// does not enumerate but `prop_is_secret` recognises — dynamic per-alias
/// credential paths, which exist in the tree but not in the static field list.
#[must_use]
pub fn lookup_prop_field(config: &Config, path: &str) -> Option<PropFieldInfo> {
    config
        .prop_fields()
        .into_iter()
        .find(|info| info.name == path)
        .or_else(|| {
            Config::prop_is_secret(path).then(|| PropFieldInfo {
                name: path.to_string(),
                category: "Secrets",
                display_value: UNSET_DISPLAY.to_string(),
                type_hint: "String",
                kind: PropKind::String,
                is_secret: true,
                enum_variants: None,
                description: "",
                derived_from_secret: false,
                credential_class: Some(CredentialSurfaceClass::EncryptedSecret),
                tab: ConfigTab::None,
                alias_source: None,
                multiline: false,
            })
        })
}

/// Parse a JSON Patch document into ops, without touching config.
pub fn parse_patch_ops(value: serde_json::Value) -> Result<Vec<PatchOp>, ConfigApiError> {
    let ops = value.as_array().ok_or_else(|| {
        ConfigApiError::new(
            ConfigApiCode::ValueTypeMismatch,
            "JSON Patch body must be a JSON array of operations",
        )
    })?;

    let mut parsed = Vec::with_capacity(ops.len());
    for (idx, op) in ops.iter().enumerate() {
        let object = op.as_object().ok_or_else(|| {
            ConfigApiError::new(
                ConfigApiCode::ValueTypeMismatch,
                format!("JSON Patch op[{idx}] must be an object"),
            )
            .with_op_index(idx)
        })?;
        let op_name = object.get("op").and_then(|v| v.as_str()).ok_or_else(|| {
            ConfigApiError::new(
                ConfigApiCode::ValueTypeMismatch,
                format!("JSON Patch op[{idx}] requires string `op` field"),
            )
            .with_op_index(idx)
        })?;
        let path = object.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
            ConfigApiError::new(
                ConfigApiCode::ValueTypeMismatch,
                format!("JSON Patch op[{idx}] requires string `path` field"),
            )
            .with_op_index(idx)
        })?;
        let comment = match object.get("comment") {
            Some(value) => Some(
                value
                    .as_str()
                    .ok_or_else(|| {
                        ConfigApiError::new(
                            ConfigApiCode::ValueTypeMismatch,
                            format!("JSON Patch op[{idx}] `comment` field must be a string"),
                        )
                        .with_path(json_pointer_to_dotted(path))
                        .with_op_index(idx)
                    })?
                    .to_string(),
            ),
            None => None,
        };

        parsed.push(PatchOp {
            op: op_name.to_string(),
            path: path.to_string(),
            value: object.get("value").cloned(),
            comment,
        });
    }

    Ok(parsed)
}

/// Apply ops to `config` **in memory**. Nothing is written to disk.
///
/// All-or-nothing from the caller's perspective only if the caller discards
/// `config` on error: ops before a failing one have already mutated it. Every
/// caller either works on a clone (gateway, tool) or aborts before saving (CLI),
/// so a partial application is never persisted.
pub fn apply_patch_ops(
    config: &mut Config,
    ops: &[PatchOp],
) -> Result<Vec<PatchOpResult>, ConfigApiError> {
    let mut results = Vec::with_capacity(ops.len());

    for (idx, op) in ops.iter().enumerate() {
        let path = json_pointer_to_dotted(&op.path);
        if matches!(op.op.as_str(), "add" | "replace") && config.ensure_map_key_for_path(&path) {
            // Refused to vivify the reserved `default` agent: surface the same
            // reserved error the explicit create surfaces do, not a generic 404.
            return Err(ConfigApiError::new(
                ConfigApiCode::ValidationFailed,
                "alias `default` is reserved and cannot be created",
            )
            .with_path(&path)
            .with_op_index(idx));
        }
        let info = lookup_prop_field(config, &path);
        let is_sensitive = info
            .as_ref()
            .map(|i| i.is_secret || i.derived_from_secret)
            .unwrap_or(false);

        match op.op.as_str() {
            "test" => {
                // Secret values can't leave the server, so a differential
                // test response would be the only signal — ban the op.
                if is_sensitive {
                    return Err(ConfigApiError::secret_test_forbidden(&path).with_op_index(idx));
                }
                let want = op.value.as_ref().ok_or_else(|| {
                    ConfigApiError::new(
                        ConfigApiCode::ValueTypeMismatch,
                        "JSON Patch `test` op requires `value` field",
                    )
                    .with_path(&path)
                    .with_op_index(idx)
                })?;
                let actual_str = config
                    .get_prop(&path)
                    .map_err(|e| map_prop_error(e, &path).with_op_index(idx))?;
                let want_str = coerce_for_set_prop(want, info.as_ref().map(|i| i.kind))
                    .map_err(|e| e.with_path(&path).with_op_index(idx))?;
                if actual_str != want_str {
                    return Err(ConfigApiError::new(
                        ConfigApiCode::ValidationFailed,
                        format!("`test` op failed: expected {want_str:?}, got {actual_str:?}"),
                    )
                    .with_path(&path)
                    .with_op_index(idx));
                }
                results.push(PatchOpResult {
                    op: op.op.clone(),
                    path,
                    value: Some(serde_json::Value::String(actual_str)),
                    populated: None,
                    comment: None, // `test` ops don't write
                });
            }
            "add" | "replace" => {
                let value = op.value.as_ref().ok_or_else(|| {
                    ConfigApiError::new(
                        ConfigApiCode::ValueTypeMismatch,
                        format!("JSON Patch `{}` op requires `value` field", op.op),
                    )
                    .with_path(&path)
                    .with_op_index(idx)
                })?;
                let value_str = coerce_for_set_prop(value, info.as_ref().map(|i| i.kind))
                    .map_err(|e| e.with_path(&path).with_op_index(idx))?;
                config
                    .set_prop_persistent(&path, &value_str)
                    .map_err(|e| map_prop_error(e, &path).with_op_index(idx))?;
                if is_sensitive {
                    results.push(PatchOpResult {
                        op: op.op.clone(),
                        path,
                        value: None,
                        populated: Some(!value_str.is_empty()),
                        comment: op.comment.clone(),
                    });
                } else {
                    results.push(PatchOpResult {
                        op: op.op.clone(),
                        path,
                        value: Some(serde_json::Value::String(value_str)),
                        populated: None,
                        comment: op.comment.clone(),
                    });
                }
            }
            "remove" => {
                config
                    .set_prop_persistent(&path, "")
                    .map_err(|e| map_prop_error(e, &path).with_op_index(idx))?;
                if is_sensitive {
                    results.push(PatchOpResult {
                        op: op.op.clone(),
                        path,
                        value: None,
                        populated: Some(false),
                        comment: op.comment.clone(),
                    });
                } else {
                    results.push(PatchOpResult {
                        op: op.op.clone(),
                        path,
                        value: Some(serde_json::Value::Null),
                        populated: None,
                        comment: op.comment.clone(),
                    });
                }
            }
            "comment" => {
                // Comment-only update: record the (path, comment) pair
                // for `apply_comments` after the patch commits, but
                // skip `set_prop` entirely. Lets the operator annotate
                // a secret without rotating its ciphertext.
                if info.is_none() {
                    return Err(ConfigApiError::path_not_found(&path).with_op_index(idx));
                }
                let comment = op.comment.clone().ok_or_else(|| {
                    ConfigApiError::new(
                        ConfigApiCode::ValueTypeMismatch,
                        "JSON Patch `comment` op requires `comment` field",
                    )
                    .with_path(&path)
                    .with_op_index(idx)
                })?;
                results.push(PatchOpResult {
                    op: op.op.clone(),
                    path,
                    value: None,
                    populated: None,
                    comment: Some(comment),
                });
            }
            "move" | "copy" => {
                return Err(ConfigApiError::op_not_supported(&op.op)
                    .with_path(&path)
                    .with_op_index(idx));
            }
            other => {
                return Err(ConfigApiError::new(
                    ConfigApiCode::OpNotSupported,
                    format!("unknown JSON Patch operation `{other}`"),
                )
                .with_path(&path)
                .with_op_index(idx));
            }
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_pointer_to_dotted_handles_pointer_form() {
        assert_eq!(json_pointer_to_dotted("/gateway/host"), "gateway.host");
    }

    #[test]
    fn json_pointer_to_dotted_passes_dotted_form_through() {
        assert_eq!(json_pointer_to_dotted("gateway.host"), "gateway.host");
    }

    #[test]
    fn map_prop_error_classifies_unknown_property() {
        let err = map_prop_error(anyhow::Error::msg("Unknown property: nope"), "nope");
        assert_eq!(err.code, ConfigApiCode::PathNotFound);
    }

    #[test]
    fn parse_patch_ops_rejects_a_non_array_body() {
        let err = parse_patch_ops(serde_json::json!({"op": "replace"}))
            .expect_err("object body must be refused");
        assert_eq!(err.code, ConfigApiCode::ValueTypeMismatch);
    }

    #[test]
    fn parse_patch_ops_reports_the_offending_index() {
        let err = parse_patch_ops(serde_json::json!([
            {"op": "replace", "path": "/gateway/host", "value": "x"},
            {"path": "/gateway/port", "value": 1},
        ]))
        .expect_err("second op has no `op` field");
        assert_eq!(err.op_index, Some(1));
    }

    #[test]
    fn apply_patch_ops_refuses_an_unknown_operation() {
        let mut config = Config::default();
        let ops = parse_patch_ops(serde_json::json!([
            {"op": "frobnicate", "path": "/gateway/host", "value": "x"}
        ]))
        .expect("parses");

        let err = apply_patch_ops(&mut config, &ops).expect_err("unknown op must be refused");
        assert_eq!(err.code, ConfigApiCode::OpNotSupported);
        assert_eq!(err.op_index, Some(0));
    }

    #[test]
    fn apply_patch_ops_mutates_in_memory_without_persisting() {
        let mut config = Config::default();
        let ops = parse_patch_ops(serde_json::json!([
            {"op": "replace", "path": "/gateway/host", "value": "127.0.0.2"}
        ]))
        .expect("parses");

        let results = apply_patch_ops(&mut config, &ops).expect("applies");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, "gateway.host");
        assert_eq!(config.gateway.host, "127.0.0.2");
    }

    /// The property the preview path depends on: applying to a clone must leave
    /// the original untouched, so a caller can compute "what would this do?"
    /// without committing anything.
    #[test]
    fn applying_to_a_clone_leaves_the_original_alone() {
        let config = Config::default();
        let original_host = config.gateway.host.clone();
        let ops = parse_patch_ops(serde_json::json!([
            {"op": "replace", "path": "/gateway/host", "value": "10.0.0.1"}
        ]))
        .expect("parses");

        let mut preview = config.clone();
        apply_patch_ops(&mut preview, &ops).expect("applies to the copy");

        assert_eq!(preview.gateway.host, "10.0.0.1");
        assert_eq!(
            config.gateway.host, original_host,
            "the source config must not move when a preview is computed"
        );
    }
}
