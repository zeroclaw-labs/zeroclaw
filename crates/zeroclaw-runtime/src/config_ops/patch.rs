//! A JSON Patch (RFC 6902) applied to the configuration, as the dashboard's
//! config routes apply one. Shared by the gateway's `PATCH /api/config` and
//! the RPC `config/set-many` `ops` batch, so both stage the same operations
//! with the same checks and report the same results.
//!
//! Everything here works on a working copy of the configuration. Persisting
//! it, authorizing its write set and applying the comments stay with each
//! surface's own config-write path.

use std::collections::{BTreeSet, HashSet};

use zeroclaw_config::api_error::{ConfigApiCode, ConfigApiError};
use zeroclaw_config::schema::Config;
use zeroclaw_config::traits::PropFieldInfo;
use zeroclaw_config::typed_value::coerce_for_set_prop;
use zeroclaw_config::validation_warnings::ValidationWarning;
use zeroclaw_rpc_proto::types::{ConfigPatchOp, ConfigPatchOpResult};

/// A JSON Pointer (`/agents/main/model_provider`) as the dotted property
/// path it names; a dotted path passes through unchanged.
pub fn json_pointer_to_dotted(path: &str) -> String {
    if path.starts_with('/') {
        path.trim_start_matches('/').replace('/', ".")
    } else {
        path.to_string()
    }
}

/// The property metadata for `path`, or for a secret path the schema
/// reaches only through a map entry, a synthesized secret entry.
pub fn lookup_prop_field(config: &Config, path: &str) -> Option<PropFieldInfo> {
    config
        .prop_fields()
        .into_iter()
        .find(|info| info.name == path)
        .or_else(|| {
            Config::prop_is_secret(path).then(|| PropFieldInfo {
                name: path.to_string(),
                category: "Secrets",
                display_value: zeroclaw_config::traits::UNSET_DISPLAY.to_string(),
                type_hint: "String",
                kind: zeroclaw_config::traits::PropKind::String,
                is_secret: true,
                enum_variants: None,
                description: "",
                derived_from_secret: false,
                credential_class: Some(
                    zeroclaw_config::traits::CredentialSurfaceClass::EncryptedSecret,
                ),
                tab: zeroclaw_config::traits::ConfigTab::None,
                alias_source: None,
                multiline: false,
            })
        })
}

/// Reject masked or empty values from writes to secret-bearing properties.
/// Dashboard surfaces may send the masked display sentinel when no real edit
/// was made; accepting it would replace the live secret with that sentinel.
pub fn reject_masked_secret_value(
    path: &str,
    is_sensitive: bool,
    value: &str,
) -> Result<(), ConfigApiError> {
    if is_sensitive
        && (value == zeroclaw_config::traits::MASKED_SECRET || value == "****" || value.is_empty())
    {
        return Err(ConfigApiError::new(
            ConfigApiCode::ValidationFailed,
            format!("Refusing to overwrite secret `{path}` with a masked or empty value"),
        )
        .with_path(path));
    }
    Ok(())
}

/// Validate `working` as a write's result: a failure on one of its dirty
/// paths (or one naming no path) refuses the write; a failure elsewhere was
/// already there, so the write goes ahead and the failure is returned as a
/// `pre_existing_validation_error` warning.
pub fn scoped_validate(working: &Config) -> Result<Vec<ValidationWarning>, ConfigApiError> {
    if let Err(e) = working.validate() {
        let api_err = ConfigApiError::from_validation(e);
        let err_path = api_err.path.as_deref().unwrap_or("");
        let touches_dirty = !err_path.is_empty()
            && working.dirty_paths.iter().any(|d| {
                err_path == d.as_str()
                    || err_path.starts_with(&format!("{d}."))
                    || d.starts_with(&format!("{err_path}."))
            });
        if touches_dirty || err_path.is_empty() {
            return Err(api_err);
        }
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"path": err_path})),
            &format!(
                "validate() failed on a path outside this PATCH's dirty set; saving anyway and \
             surfacing as a warning: {}",
                api_err.message
            )
        );
        return Ok(vec![ValidationWarning::new(
            "pre_existing_validation_error",
            api_err.message,
            err_path.to_string(),
        )]);
    }
    Ok(Vec::new())
}

/// The agents whose configuration a patch writes, for their lifecycle
/// reservations.
pub fn agent_aliases(ops: &[ConfigPatchOp]) -> BTreeSet<String> {
    ops.iter()
        .filter(|op| matches!(op.op.as_str(), "add" | "replace" | "remove"))
        .filter_map(|op| {
            let path = json_pointer_to_dotted(&op.path);
            zeroclaw_config::alias_refs::agent_alias_for_prop_path(&path).map(str::to_owned)
        })
        .collect()
}

/// The paths a patch removes. Clearing a property leaves its field
/// declared, so these are pinned as deletions in the write set.
pub fn removed_paths(ops: &[ConfigPatchOp]) -> Vec<String> {
    ops.iter()
        .filter(|op| op.op == "remove")
        .map(|op| json_pointer_to_dotted(&op.path))
        .collect()
}

/// The refusal for a patch naming a path whose file value drifted from the
/// running configuration, or `None` when no named path drifted.
pub fn drift_conflict<'a>(
    drifted: impl IntoIterator<Item = &'a str>,
    ops: &[ConfigPatchOp],
) -> Option<ConfigApiError> {
    let touched: HashSet<String> = ops
        .iter()
        .map(|op| json_pointer_to_dotted(&op.path))
        .collect();
    let conflict_paths: Vec<&str> = drifted
        .into_iter()
        .filter(|path| touched.contains(*path))
        .collect();
    if conflict_paths.is_empty() {
        return None;
    }
    Some(ConfigApiError::new(
        ConfigApiCode::ConfigChangedExternally,
        format!(
            "on-disk config has drifted from in-memory state on \
             {} path(s) being patched: {}. Send `X-ZeroClaw-Override-Drift: true` \
             to overwrite, or GET /api/config/drift to inspect first.",
            conflict_paths.len(),
            conflict_paths.join(", "),
        ),
    ))
}

/// The `(path, comment)` pairs to write once the patch is saved: every
/// operation that carried a comment, at the path it reported.
pub fn annotations(
    ops: &[ConfigPatchOp],
    results: &[ConfigPatchOpResult],
) -> Vec<(String, String)> {
    ops.iter()
        .zip(results.iter())
        .filter_map(|(op, res)| op.comment.as_ref().map(|c| (res.path.clone(), c.clone())))
        .collect()
}

/// Stage `ops` on `working` in order. A `test` compares without writing and
/// is refused on a secret; `add` and `replace` check the value against the
/// field's declared kind and refuse a masked or empty secret; `remove`
/// resets the property; `comment` only checks the path exists. The first
/// operation that fails refuses the patch, naming its index; the caller then
/// drops `working`.
pub fn apply_patch_ops(
    working: &mut Config,
    ops: &[ConfigPatchOp],
) -> Result<Vec<ConfigPatchOpResult>, ConfigApiError> {
    let mut results = Vec::with_capacity(ops.len());

    for (idx, op) in ops.iter().enumerate() {
        let path = json_pointer_to_dotted(&op.path);
        if matches!(op.op.as_str(), "add" | "replace") && working.ensure_map_key_for_path(&path) {
            // Refused to vivify the reserved `default` agent: surface the same
            // reserved error the explicit create surfaces do, not a generic 404.
            return Err(ConfigApiError::new(
                ConfigApiCode::ValidationFailed,
                "alias `default` is reserved and cannot be created",
            )
            .with_path(&path)
            .with_op_index(idx));
        }
        let info = lookup_prop_field(working, &path);
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
                let actual_str = working
                    .get_prop(&path)
                    .map_err(|e| ConfigApiError::for_prop(e, &path).with_op_index(idx))?;
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
                results.push(ConfigPatchOpResult {
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
                reject_masked_secret_value(&path, is_sensitive, &value_str)
                    .map_err(|e| e.with_op_index(idx))?;
                working
                    .set_prop_persistent(&path, &value_str)
                    .map_err(|e| ConfigApiError::for_prop(e, &path).with_op_index(idx))?;
                if is_sensitive {
                    results.push(ConfigPatchOpResult {
                        op: op.op.clone(),
                        path,
                        value: None,
                        populated: Some(!value_str.is_empty()),
                        comment: op.comment.clone(),
                    });
                } else {
                    results.push(ConfigPatchOpResult {
                        op: op.op.clone(),
                        path,
                        value: Some(serde_json::Value::String(value_str)),
                        populated: None,
                        comment: op.comment.clone(),
                    });
                }
            }
            "remove" => {
                working
                    .set_prop_persistent(&path, "")
                    .map_err(|e| ConfigApiError::for_prop(e, &path).with_op_index(idx))?;
                if is_sensitive {
                    results.push(ConfigPatchOpResult {
                        op: op.op.clone(),
                        path,
                        value: None,
                        populated: Some(false),
                        comment: op.comment.clone(),
                    });
                } else {
                    results.push(ConfigPatchOpResult {
                        op: op.op.clone(),
                        path,
                        value: Some(serde_json::Value::Null),
                        populated: None,
                        comment: op.comment.clone(),
                    });
                }
            }
            "comment" => {
                // Comment-only update: the caller writes the (path, comment)
                // pair after the patch commits and skips `set_prop` entirely,
                // so an operator can annotate a secret without rotating it.
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
                results.push(ConfigPatchOpResult {
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

    fn op(op: &str, path: &str, value: Option<serde_json::Value>) -> ConfigPatchOp {
        ConfigPatchOp {
            op: op.into(),
            path: path.into(),
            value,
            comment: None,
        }
    }

    #[test]
    fn a_json_pointer_names_the_dotted_path() {
        assert_eq!(
            json_pointer_to_dotted("/agents/main/model_provider"),
            "agents.main.model_provider"
        );
        assert_eq!(json_pointer_to_dotted("memory.backend"), "memory.backend");
    }

    #[test]
    fn a_failed_operation_refuses_the_patch_and_names_its_index() {
        let mut working = Config::default();
        let ops = [
            op("replace", "/memory/backend", Some("none".into())),
            op("move", "/memory/backend", None),
        ];
        let refused = apply_patch_ops(&mut working, &ops).unwrap_err();
        assert_eq!(refused.code, ConfigApiCode::OpNotSupported);
        assert_eq!(refused.op_index, Some(1));
    }

    #[test]
    fn each_operation_reports_what_it_did() {
        let mut working = Config::default();
        let ops = [
            op("replace", "/memory/backend", Some("none".into())),
            op("test", "memory.backend", Some("none".into())),
            op("remove", "/memory/backend", None),
        ];
        let results = apply_patch_ops(&mut working, &ops).unwrap();
        let wire = serde_json::to_value(&results).unwrap();
        assert_eq!(
            wire,
            serde_json::json!([
                {"op": "replace", "path": "memory.backend", "value": "none"},
                {"op": "test", "path": "memory.backend", "value": "none"},
                {"op": "remove", "path": "memory.backend", "value": null},
            ])
        );
    }

    #[test]
    fn only_a_drifted_path_the_patch_names_conflicts() {
        let ops = [op("replace", "/memory/backend", Some("none".into()))];
        assert!(drift_conflict(["agents.main.temperature"], &ops).is_none());
        let refused = drift_conflict(["memory.backend", "other"], &ops).expect("a conflict");
        assert_eq!(refused.code, ConfigApiCode::ConfigChangedExternally);
        assert!(
            refused.message.contains("memory.backend"),
            "{}",
            refused.message
        );
    }
}
