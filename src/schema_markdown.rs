use std::fmt::Write as _;

use serde_json::{Map, Value};

/// Generates a markdown config reference by walking the schemars JSON Schema value in memory.
/// No intermediate JSON file, no external tools.
pub fn generate(root: &Value) -> String {
    let empty = Map::new();
    let defs = root
        .get("$defs")
        .and_then(Value::as_object)
        .unwrap_or(&empty);

    let mut out = String::new();
    out.push_str("# Config Reference\n\n");
    out.push_str(
        "ZeroClaw is configured via a TOML file. All fields are optional unless noted.\n\n",
    );

    let Some(props) = root.get("properties").and_then(Value::as_object) else {
        return out;
    };

    // Index table
    out.push_str("| Section | Description |\n");
    out.push_str("|---------|-------------|\n");
    for (key, schema) in props {
        let resolved = resolve(schema, defs);
        let desc = first_line(resolved.get("description").and_then(Value::as_str));
        let _ = writeln!(out, "| `{key}` | {desc} |");
    }
    out.push('\n');

    // Per-section details
    for (key, schema) in props {
        let resolved = resolve(schema, defs);
        write_section(&mut out, &[key.as_str()], resolved, defs);
    }

    out
}

fn write_section(out: &mut String, path: &[&str], schema: &Value, defs: &Map<String, Value>) {
    let hashes = "#".repeat(path.len() + 1);
    let path_str = path.join(".");
    let _ = writeln!(out, "{hashes} `{path_str}`\n");

    if let Some(desc) = schema.get("description").and_then(Value::as_str) {
        out.push_str(desc);
        out.push_str("\n\n");
    }

    let empty = Map::new();
    let props = schema
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    if props.is_empty() {
        return;
    }

    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    out.push_str("| Key | Type | Default | Description |\n");
    out.push_str("|-----|------|---------|-------------|\n");

    let mut recurse: Vec<(Vec<String>, Value)> = Vec::new();

    for (key, prop_schema) in props {
        let resolved = resolve(prop_schema, defs);
        let ty = type_label(resolved, defs);
        let default = fmt_default(resolved);
        let desc =
            first_line(resolved.get("description").and_then(Value::as_str)).replace('|', "\\|");
        let req = if required.contains(&key.as_str()) {
            "\\*"
        } else {
            ""
        };
        let secret = if resolved.get("x-secret").and_then(Value::as_bool) == Some(true) {
            " 🔑"
        } else {
            ""
        };

        let has_sub = resolved
            .get("properties")
            .and_then(Value::as_object)
            .map(|p| !p.is_empty())
            .unwrap_or(false);

        let _ = writeln!(out, "| `{key}`{req}{secret} | {ty} | {default} | {desc} |");

        // Only recurse up to depth 3 (e.g. agent.auto_classify.something)
        if has_sub && path.len() < 3 {
            let mut sub_path: Vec<String> = path.iter().map(|s| (*s).to_owned()).collect();
            sub_path.push(key.clone());
            recurse.push((sub_path, resolved.clone()));
        }
    }
    out.push('\n');

    for (sub_path_owned, sub_schema) in &recurse {
        let refs: Vec<&str> = sub_path_owned.iter().map(String::as_str).collect();
        write_section(out, &refs, sub_schema, defs);
    }
}

/// Resolves a `$ref` to its definition. Also unwraps single-type `anyOf` (Option<T>).
fn resolve<'a>(schema: &'a Value, defs: &'a Map<String, Value>) -> &'a Value {
    if let Some(ref_str) = schema.get("$ref").and_then(Value::as_str) {
        let name = ref_str
            .trim_start_matches("#/$defs/")
            .trim_start_matches("#/definitions/");
        if let Some(def) = defs.get(name) {
            return resolve(def, defs);
        }
    }
    if let Some(any_of) = schema.get("anyOf").and_then(Value::as_array) {
        let non_null: Vec<&Value> = any_of
            .iter()
            .filter(|s| s.get("type").and_then(Value::as_str) != Some("null"))
            .collect();
        if non_null.len() == 1 {
            return resolve(non_null[0], defs);
        }
    }
    schema
}

fn type_label(schema: &Value, defs: &Map<String, Value>) -> String {
    if let Some(any_of) = schema.get("anyOf").and_then(Value::as_array) {
        let non_null: Vec<&Value> = any_of
            .iter()
            .filter(|s| s.get("type").and_then(Value::as_str) != Some("null"))
            .collect();
        if non_null.len() == 1 {
            return format!("{}?", type_label(non_null[0], defs));
        }
        return non_null
            .iter()
            .map(|s| type_label(s, defs))
            .collect::<Vec<_>>()
            .join(" \\| ");
    }

    if let Some(ref_str) = schema.get("$ref").and_then(Value::as_str) {
        let name = ref_str
            .trim_start_matches("#/$defs/")
            .trim_start_matches("#/definitions/");
        if let Some(def) = defs.get(name) {
            return type_label(def, defs);
        }
        return name.to_owned();
    }

    if schema.get("oneOf").is_some() || schema.get("enum").is_some() {
        if let Some(title) = schema.get("title").and_then(Value::as_str) {
            return title.to_owned();
        }
        if let Some(vals) = schema.get("enum").and_then(Value::as_array) {
            let s: Vec<String> = vals
                .iter()
                .filter_map(Value::as_str)
                .map(|v| format!("`{v}`"))
                .collect();
            if !s.is_empty() {
                return s.join(" \\| ");
            }
        }
    }

    match schema.get("type").and_then(Value::as_str) {
        Some("boolean") => "bool".to_owned(),
        Some("string") => "string".to_owned(),
        Some("integer") => "integer".to_owned(),
        Some("number") => "number".to_owned(),
        Some("array") => {
            let item_type = schema
                .get("items")
                .map(|i| type_label(i, defs))
                .unwrap_or_else(|| "any".to_owned());
            format!("{item_type}[]")
        }
        Some("object") => {
            if schema.get("additionalProperties").is_some() {
                "map".to_owned()
            } else {
                "object".to_owned()
            }
        }
        _ => {
            if schema.get("properties").is_some() {
                "object".to_owned()
            } else {
                schema
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("any")
                    .to_owned()
            }
        }
    }
}

fn fmt_default(schema: &Value) -> String {
    match schema.get("default") {
        Some(Value::Bool(b)) => format!("`{b}`"),
        Some(Value::String(s)) if s.is_empty() => "`\"\"`".to_owned(),
        Some(Value::String(s)) => format!("`\"{s}\"`"),
        Some(Value::Number(n)) => format!("`{n}`"),
        Some(Value::Null) => "`null`".to_owned(),
        Some(Value::Array(a)) if a.is_empty() => "`[]`".to_owned(),
        Some(v) => format!("`{v}`"),
        None => "—".to_owned(),
    }
}

fn first_line(s: Option<&str>) -> String {
    s.and_then(|d| d.lines().next()).unwrap_or("").to_owned()
}
