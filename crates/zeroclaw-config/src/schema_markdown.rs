use std::fmt::Write as _;

use serde_json::{Map, Value};

/// Renders a single struct's fields as an interactive config table from that
/// struct's `schema_for!` JSON value. Top-level `enabled` is skipped by default
/// since channel pages document it separately; pass `include_enabled = true` to
/// keep it. `$ref` types resolve against the schema's own `$defs`. This is the
/// same type/default/description extraction used by [`generate`], so a
/// per-channel field table can never drift from the global config reference.
///
/// When `prefix` is `Some` (the struct's dotted config path, e.g.
/// `channels.mattermost.<alias>`), the table is emitted as raw HTML with each
/// field name as an accordion trigger: clicking a field expands a detail row
/// directly beneath it carrying the per-field gateway-dashboard deep-link,
/// zerocode location, and `zeroclaw config set` command. The
/// `pc-enhance.js` `installConfigFieldRows` handler wires the toggle. When
/// `prefix` is `None`, a plain Markdown table is emitted (no accordion).
pub fn field_table(root: &Value, include_enabled: bool, prefix: Option<&str>) -> String {
    let empty = Map::new();
    let defs = root
        .get("$defs")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let Some(props) = root.get("properties").and_then(Value::as_object) else {
        return String::new();
    };
    let required: Vec<&str> = root
        .get("required")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    let Some(prefix) = prefix else {
        return plain_field_table(props, &required, defs, include_enabled);
    };
    let section = prefix.split('.').next().unwrap_or(prefix);

    let mut rows = String::new();
    for (key, prop_schema) in props {
        if key == "enabled" && !include_enabled {
            continue;
        }
        let resolved = resolve(prop_schema, defs);
        let is_secret = resolved.get("x-secret").and_then(Value::as_bool) == Some(true);
        let ty = if is_secret {
            "secret".to_owned()
        } else {
            type_label(resolved, defs)
        };
        let default = fmt_default(resolved);
        let req = if required.contains(&key.as_str()) {
            "*"
        } else {
            ""
        };
        let secret_mark = if is_secret { " 🔑" } else { "" };
        let full_path = format!("{prefix}.{key}");
        let set_cmd = if is_secret {
            format!("zeroclaw config set {full_path}    # masked input, stored encrypted")
        } else {
            format!("zeroclaw config set {full_path} <value>")
        };
        let full_desc = resolved
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("");
        let grp = format!(
            "cfgtab-{}",
            full_path
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect::<String>()
        );

        let tabs = format!(
            concat!(
                "<div class=\"os-tabs\">",
                "<input type=\"radio\" name=\"{grp}\" id=\"{grp}-0\" checked>",
                "<input type=\"radio\" name=\"{grp}\" id=\"{grp}-1\">",
                "<input type=\"radio\" name=\"{grp}\" id=\"{grp}-2\">",
                "<nav class=\"os-tab-labels\">",
                "<label for=\"{grp}-0\">Gateway dashboard</label>",
                "<label for=\"{grp}-1\">zerocode</label>",
                "<label for=\"{grp}-2\">zeroclaw config</label>",
                "</nav>",
                "<div class=\"os-tab-panel\"><p>Open <a href=\"http://127.0.0.1:42617/config/{section}\"><code>/config/{section}</code></a> and set the <code>{full_path}</code> field.</p></div>",
                "<div class=\"os-tab-panel\"><p>In the <strong>Config</strong> pane, set the <code>{full_path}</code> field.</p></div>",
                "<div class=\"os-tab-panel\"><pre><code>{set_cmd}</code></pre></div>",
                "</div>",
            ),
            grp = grp,
            section = html_escape(section),
            full_path = html_escape(&full_path),
            set_cmd = html_escape(&set_cmd),
        );

        let _ = write!(
            rows,
            concat!(
                "<tr class=\"cfg-field-row\" tabindex=\"0\" role=\"button\" aria-expanded=\"false\">",
                "<td class=\"cfg-field-name\"><code>{key}</code>{req}{secret_mark}</td>",
                "<td>{ty}</td><td>{default}</td>",
                "</tr>\n",
                "<tr class=\"cfg-field-detail\" hidden><td colspan=\"3\">",
                "<p>{full_desc}</p>",
                "{tabs}",
                "</td></tr>\n",
            ),
            key = html_escape(key),
            req = req,
            secret_mark = secret_mark,
            ty = html_escape(&ty),
            default = inline_code_html(&default),
            full_desc = desc_html(full_desc),
            tabs = tabs,
        );
    }

    format!(
        "<div class=\"cfg-fields\">\n<table>\n<thead><tr><th>field</th><th>type</th><th>default</th></tr></thead>\n<tbody>\n{rows}</tbody>\n</table>\n</div>\n"
    )
}

/// Plain Markdown field table (no accordion), used when no config prefix is
/// supplied.
fn plain_field_table(
    props: &Map<String, Value>,
    required: &[&str],
    defs: &Map<String, Value>,
    include_enabled: bool,
) -> String {
    let mut out = String::new();
    out.push_str("| field | type | default | meaning |\n");
    out.push_str("|---|---|---|---|\n");
    for (key, prop_schema) in props {
        if key == "enabled" && !include_enabled {
            continue;
        }
        let resolved = resolve(prop_schema, defs);
        let is_secret = resolved.get("x-secret").and_then(Value::as_bool) == Some(true);
        let ty = if is_secret {
            "secret".to_owned()
        } else {
            type_label(resolved, defs)
        };
        let default = fmt_default(resolved);
        let desc =
            first_line(resolved.get("description").and_then(Value::as_str)).replace('|', "\\|");
        let req = if required.contains(&key.as_str()) {
            "\\*"
        } else {
            ""
        };
        let secret = if is_secret { " 🔑" } else { "" };
        let _ = writeln!(out, "| `{key}`{req}{secret} | {ty} | {default} | {desc} |");
    }
    out
}

/// Escape text for inclusion in HTML body content.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// HTML-escape a description, then render Markdown `` `code` `` spans as
/// `<code>`. Newlines collapse to spaces so multi-line doc comments read as a
/// single paragraph in the expanded panel.
fn desc_html(s: &str) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::new();
    let mut in_code = false;
    let mut buf = String::new();
    for ch in collapsed.chars() {
        if ch == '`' {
            if in_code {
                out.push_str("<code>");
                out.push_str(&html_escape(&buf));
                out.push_str("</code>");
            } else {
                out.push_str(&html_escape(&buf));
            }
            buf.clear();
            in_code = !in_code;
        } else {
            buf.push(ch);
        }
    }
    // Trailing buffer (or an unbalanced backtick) renders as plain escaped text.
    out.push_str(&html_escape(&buf));
    out
}

/// Render a `fmt_default`-style value (which may be wrapped in backticks) as
/// inline-code HTML, escaping the inner text.
fn inline_code_html(s: &str) -> String {
    let trimmed = s.trim();
    if let Some(inner) = trimmed.strip_prefix('`').and_then(|t| t.strip_suffix('`')) {
        format!("<code>{}</code>", html_escape(inner))
    } else {
        html_escape(trimmed)
    }
}

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

    // schemars 1.x renders `Option<T>` as `{"type": ["T", "null"]}`. Unwrap the
    // nullable wrapper to `T?` so the table shows the real underlying type
    // instead of falling through to `any`.
    if let Some(types) = schema.get("type").and_then(Value::as_array) {
        let non_null: Vec<&str> = types
            .iter()
            .filter_map(Value::as_str)
            .filter(|t| *t != "null")
            .collect();
        if non_null.len() == 1 {
            let mut inner = schema.clone();
            inner["type"] = Value::String(non_null[0].to_owned());
            return format!("{}?", type_label(&inner, defs));
        }
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
