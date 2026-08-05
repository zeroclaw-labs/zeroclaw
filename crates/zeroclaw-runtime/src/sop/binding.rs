//! Argument bindings for planned tool calls. A string value inside a
//! planned call's args may embed `{{steps.N.path}}` (data captured from a
//! prior step) or `{{calls.K.path}}` (an earlier call in the same step,
//! zero-based). Extraction feeds strict-save validation; resolution
//! substitutes live or pinned run data at execution/preview time.

use std::collections::HashMap;

use serde_json::Value;

/// Scope a binding draws from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingScope {
    /// `steps.N`: step number within the SOP.
    Step(u32),
    /// `calls.K`: zero-based call index within the same step.
    Call(u32),
}

/// One parsed `{{...}}` reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingRef {
    pub scope: BindingScope,
    /// Dotted path into the referenced data; empty means the whole value.
    pub path: String,
    /// Raw body between the braces, for diagnostics.
    pub raw: String,
}

/// Extraction result: valid reference or a malformed body with a reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractedBinding {
    Valid(BindingRef),
    Malformed { raw: String, reason: String },
}

/// Walk a JSON value and extract every `{{...}}` binding found in string
/// leaves, in document order.
pub fn extract_bindings(value: &Value) -> Vec<ExtractedBinding> {
    let mut out = Vec::new();
    collect(value, &mut out);
    out
}

/// Like [`extract_bindings`] but pairs each binding with the dotted arg-field
/// path it sits under (empty for a top-level string, `field.sub` for nested
/// objects, `field.N` for array elements). Authoring surfaces use the path to
/// name the consumer input pin a binding wires into.
pub fn extract_bindings_with_paths(value: &Value) -> Vec<(String, ExtractedBinding)> {
    let mut out = Vec::new();
    collect_paths(value, String::new(), &mut out);
    out
}

fn collect_paths(value: &Value, path: String, out: &mut Vec<(String, ExtractedBinding)>) {
    match value {
        Value::String(s) => {
            let mut found = Vec::new();
            scan_string(s, &mut found);
            for binding in found {
                out.push((path.clone(), binding));
            }
        }
        Value::Array(items) => {
            for (idx, item) in items.iter().enumerate() {
                collect_paths(item, join_path(&path, &idx.to_string()), out);
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                collect_paths(item, join_path(&path, key), out);
            }
        }
        _ => {}
    }
}

fn join_path(prefix: &str, segment: &str) -> String {
    if prefix.is_empty() {
        segment.to_string()
    } else {
        format!("{prefix}.{segment}")
    }
}

fn collect(value: &Value, out: &mut Vec<ExtractedBinding>) {
    match value {
        Value::String(s) => scan_string(s, out),
        Value::Array(items) => {
            for item in items {
                collect(item, out);
            }
        }
        Value::Object(map) => {
            for item in map.values() {
                collect(item, out);
            }
        }
        _ => {}
    }
}

fn scan_string(s: &str, out: &mut Vec<ExtractedBinding>) {
    let mut rest = s;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push(ExtractedBinding::Malformed {
                raw: after.trim().to_string(),
                reason: "unclosed '{{'".into(),
            });
            return;
        };
        let body = after[..end].trim().to_string();
        out.push(parse_binding(&body));
        rest = &after[end + 2..];
    }
}

fn parse_binding(body: &str) -> ExtractedBinding {
    let malformed = |reason: &str| ExtractedBinding::Malformed {
        raw: body.to_string(),
        reason: reason.to_string(),
    };
    let mut parts = body.splitn(3, '.');
    let scope = parts.next().unwrap_or("");
    let Some(index) = parts.next() else {
        return malformed("missing index (expected steps.N or calls.K)");
    };
    let Ok(index) = index.parse::<u32>() else {
        return malformed("index is not a number");
    };
    let scope = match scope {
        "steps" => BindingScope::Step(index),
        "calls" => BindingScope::Call(index),
        _ => return malformed("unknown scope (expected steps or calls)"),
    };
    ExtractedBinding::Valid(BindingRef {
        scope,
        path: parts.next().unwrap_or("").to_string(),
        raw: body.to_string(),
    })
}

/// Data a resolution pass draws from: per-step values keyed by step number
/// and the ordered values of calls already executed in the current step.
pub struct BindingContext<'a> {
    pub steps: &'a HashMap<u32, Value>,
    pub calls: &'a [Value],
}

/// Resolve every binding in `args`, returning a new value. A string that is
/// exactly one binding resolves to the referenced JSON value (any type);
/// bindings embedded in longer strings interpolate as text. Unresolvable
/// references error rather than passing template text to a tool.
pub fn resolve_args(args: &Value, ctx: &BindingContext) -> Result<Value, String> {
    match args {
        Value::String(s) => resolve_string(s, ctx),
        Value::Array(items) => items
            .iter()
            .map(|item| resolve_args(item, ctx))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k.clone(), resolve_args(v, ctx)?);
            }
            Ok(Value::Object(out))
        }
        other => Ok(other.clone()),
    }
}

fn resolve_string(s: &str, ctx: &BindingContext) -> Result<Value, String> {
    let trimmed = s.trim();
    if let Some(body) = trimmed
        .strip_prefix("{{")
        .and_then(|rest| rest.strip_suffix("}}"))
        && !body.contains("{{")
    {
        return resolve_ref(body.trim(), ctx);
    }

    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            return Err(format!("unclosed '{{{{' in '{s}'"));
        };
        let resolved = resolve_ref(after[..end].trim(), ctx)?;
        match resolved {
            Value::String(text) => out.push_str(&text),
            other => out.push_str(&other.to_string()),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(Value::String(out))
}

fn resolve_ref(body: &str, ctx: &BindingContext) -> Result<Value, String> {
    let binding = match parse_binding(body) {
        ExtractedBinding::Valid(b) => b,
        ExtractedBinding::Malformed { raw, reason } => {
            return Err(format!("malformed binding '{raw}': {reason}"));
        }
    };
    let root = match binding.scope {
        BindingScope::Step(n) => ctx
            .steps
            .get(&n)
            .ok_or_else(|| format!("binding '{body}': no data for step {n}"))?,
        BindingScope::Call(k) => ctx
            .calls
            .get(k as usize)
            .ok_or_else(|| format!("binding '{body}': no data for call {k}"))?,
    };
    resolve_path(root, &binding.path)
        .ok_or_else(|| format!("binding '{body}': path does not resolve"))
        .cloned()
}

fn resolve_path<'v>(root: &'v Value, path: &str) -> Option<&'v Value> {
    if path.is_empty() {
        return Some(root);
    }
    let mut current = root;
    for segment in path.split('.') {
        current = match current {
            Value::Object(map) => map.get(segment)?,
            Value::Array(items) => items.get(segment.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}

/// Rewrite `steps.N` references inside string leaves after a step renumber.
/// Dangling references are left as-is for strict validation to surface.
pub fn remap_step_refs(value: &mut Value, remap: &HashMap<u32, u32>) {
    match value {
        Value::String(s) => {
            if let Some(rewritten) = remap_string(s, remap) {
                *s = rewritten;
            }
        }
        Value::Array(items) => {
            for item in items {
                remap_step_refs(item, remap);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                remap_step_refs(item, remap);
            }
        }
        _ => {}
    }
}

fn remap_string(s: &str, remap: &HashMap<u32, u32>) -> Option<String> {
    let mut out = String::new();
    let mut rest = s;
    let mut changed = false;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            break;
        };
        out.push_str(&rest[..start]);
        let body = after[..end].trim();
        match parse_binding(body) {
            ExtractedBinding::Valid(BindingRef {
                scope: BindingScope::Step(n),
                path,
                ..
            }) if remap.get(&n).is_some_and(|m| *m != n) => {
                let mapped = remap[&n];
                if path.is_empty() {
                    out.push_str(&format!("{{{{steps.{mapped}}}}}"));
                } else {
                    out.push_str(&format!("{{{{steps.{mapped}.{path}}}}}"));
                }
                changed = true;
            }
            _ => {
                out.push_str(&format!("{{{{{body}}}}}"));
            }
        }
        rest = &after[end + 2..];
    }
    if !changed {
        return None;
    }
    out.push_str(rest);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_valid_and_malformed_bindings() {
        let args = json!({
            "url": "{{steps.2.response.url}}",
            "note": "status was {{calls.0.status}} today",
            "bad": "{{steps.x.foo}}",
            "nested": [{"deep": "{{oops.1}}"}],
        });
        let found = extract_bindings(&args);
        assert_eq!(found.len(), 4);
        assert!(found.contains(&ExtractedBinding::Valid(BindingRef {
            scope: BindingScope::Step(2),
            path: "response.url".into(),
            raw: "steps.2.response.url".into(),
        })));
        assert!(found.contains(&ExtractedBinding::Valid(BindingRef {
            scope: BindingScope::Call(0),
            path: "status".into(),
            raw: "calls.0.status".into(),
        })));
        let malformed: Vec<_> = found
            .iter()
            .filter(|b| matches!(b, ExtractedBinding::Malformed { .. }))
            .collect();
        assert_eq!(malformed.len(), 2);
    }

    #[test]
    fn unclosed_braces_report_malformed() {
        let found = extract_bindings(&json!("{{steps.1.output"));
        assert!(
            matches!(&found[0], ExtractedBinding::Malformed { reason, .. } if reason.contains("unclosed"))
        );
    }

    #[test]
    fn whole_string_binding_resolves_typed() {
        let steps = HashMap::from([(2u32, json!({"status": 200, "ok": true}))]);
        let ctx = BindingContext {
            steps: &steps,
            calls: &[],
        };
        let resolved = resolve_args(&json!({"code": "{{steps.2.status}}"}), &ctx).unwrap();
        assert_eq!(resolved, json!({"code": 200}));
    }

    #[test]
    fn embedded_binding_interpolates_as_text() {
        let steps = HashMap::new();
        let calls = vec![json!({"status": 200})];
        let ctx = BindingContext {
            steps: &steps,
            calls: &calls,
        };
        let resolved = resolve_args(&json!("code={{calls.0.status}}!"), &ctx).unwrap();
        assert_eq!(resolved, json!("code=200!"));
    }

    #[test]
    fn missing_data_errors_instead_of_passing_template() {
        let steps = HashMap::new();
        let ctx = BindingContext {
            steps: &steps,
            calls: &[],
        };
        let err = resolve_args(&json!("{{steps.9.value}}"), &ctx).unwrap_err();
        assert!(err.contains("step 9"), "got: {err}");
    }

    #[test]
    fn array_index_paths_resolve() {
        let steps = HashMap::from([(1u32, json!({"items": ["a", "b"]}))]);
        let ctx = BindingContext {
            steps: &steps,
            calls: &[],
        };
        let resolved = resolve_args(&json!("{{steps.1.items.1}}"), &ctx).unwrap();
        assert_eq!(resolved, json!("b"));
    }

    #[test]
    fn extracts_binding_arg_field_paths() {
        let args = json!({
            "url": "{{steps.2.response.url}}",
            "nested": {"deep": "{{calls.0.status}}"},
            "list": ["x", "{{steps.1.out}}"],
        });
        let found = extract_bindings_with_paths(&args);
        let by_path: std::collections::HashMap<String, String> = found
            .into_iter()
            .filter_map(|(path, b)| match b {
                ExtractedBinding::Valid(binding) => Some((path, binding.raw)),
                ExtractedBinding::Malformed { .. } => None,
            })
            .collect();
        assert_eq!(
            by_path.get("url").map(String::as_str),
            Some("steps.2.response.url")
        );
        assert_eq!(
            by_path.get("nested.deep").map(String::as_str),
            Some("calls.0.status")
        );
        assert_eq!(
            by_path.get("list.1").map(String::as_str),
            Some("steps.1.out")
        );
    }

    #[test]
    fn extracts_top_level_string_with_empty_path() {
        let found = extract_bindings_with_paths(&json!("{{steps.1.out}}"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "");
    }
}
