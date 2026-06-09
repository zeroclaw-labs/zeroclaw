use std::fmt::Write as _;

use serde_json::{Map, Value};

/// Build the channel streaming-capability table by walking the `channels`
/// section of the `Config` schema. Capability is derived from each channel
/// struct's fields, never hand-listed:
///   - has `stream_mode` (the off/partial/multi_message enum) -> draft updates
///     and multi-message streaming are both supported.
///   - has `stream_drafts` (a partial-only boolean) -> draft updates only.
///   - neither -> no streaming.
///
/// Returns a Markdown table sorted by channel key.
pub fn channel_streaming_matrix(root: &Value) -> String {
    let empty = Map::new();
    let defs = root
        .get("$defs")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let root = resolve(root, defs);
    let Some(channels) = root
        .get("properties")
        .and_then(Value::as_object)
        .and_then(|p| p.get("channels"))
        .map(|c| resolve(c, defs))
    else {
        return String::new();
    };
    let Some(props) = channels.get("properties").and_then(Value::as_object) else {
        return String::new();
    };

    let mut rows: Vec<(String, &'static str, &'static str)> = Vec::new();
    for (key, schema) in props {
        let mut node = resolve(schema, defs);
        // Descend a map (HashMap) to the per-alias struct.
        if let Some(add) = node.get("additionalProperties")
            && add.is_object()
        {
            node = resolve(add, defs);
        }
        let Some(fields) = node.get("properties").and_then(Value::as_object) else {
            continue;
        };
        let has = |f: &str| fields.contains_key(f);
        let (draft, multi) = if has("stream_mode") {
            ("yes", "yes")
        } else if has("stream_drafts") {
            ("yes", "no")
        } else {
            continue; // no streaming -> omit from the table
        };
        rows.push((key.clone(), draft, multi));
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));

    let mut out = String::new();
    out.push_str("| Channel | Draft updates | Multi-message |\n|---|:---:|:---:|\n");
    for (ch, draft, multi) in rows {
        let cell = |v: &str| if v == "yes" { "✓" } else { "" };
        let _ = writeln!(out, "| `{ch}` | {} | {} |", cell(draft), cell(multi));
    }
    out
}

/// Navigate the full `Config` schema (`schema_for!(Config)`) to the section at
/// `path` (dotted, e.g. `channels.matrix`, `providers.models`, `acp`) and
/// render that section's fields via [`field_table`]. Map nodes (Rust
/// `HashMap<String, T>`, rendered by schemars with `additionalProperties`) are
/// transparently descended into their value type, and an `<alias>` placeholder
/// is inserted into the displayed config prefix at each crossing so the
/// per-field deep-links and `config set` commands carry the right path
/// (`channels.matrix` -> `channels.matrix.<alias>`).
///
/// Returns an error string (as a visible HTML comment) when the path does not
/// resolve, so a typo in a directive fails loudly in the rendered page rather
/// than silently emitting nothing.
/// Navigate the full `Config` schema (`schema_for!(Config)`) to the section at
/// `path` (dotted, e.g. `channels.matrix`, `providers.models`, `acp`) and
/// render that section's fields via [`field_table`]. Map nodes (Rust
/// `HashMap<String, T>`, rendered by schemars with `additionalProperties`) are
/// transparently descended into their value type, and an `<alias>` placeholder
/// is inserted into the displayed config prefix at each crossing so the
/// per-field deep-links and `config set` commands carry the right path
/// (`channels.matrix` -> `channels.matrix.<alias>`).
///
/// `defaults` is the serialized `Default::default()` of the section struct that
/// `path` resolves to (for a map section, the map's *value* type). It lets a
/// field's real default (`false`, `[]`, `{}`, `null`) surface even when
/// schemars omits the schema `default` key for `skip_serializing_if` fields.
/// Pass `None` to fall back to schema-only defaults.
///
/// Returns an error string when the path does not resolve, so a typo in a
/// directive fails loudly in the rendered page rather than silently emitting
/// nothing.
pub fn field_table_for_path(
    root: &Value,
    path: &str,
    include_enabled: bool,
    defaults: Option<&Value>,
) -> Result<String, String> {
    let empty = Map::new();
    let defs = root
        .get("$defs")
        .and_then(Value::as_object)
        .unwrap_or(&empty);

    let mut node = resolve(root, defs);
    let mut display_segments: Vec<String> = Vec::new();

    for seg in path.split('.') {
        // Descend a map (HashMap) before matching the next key: the segment
        // names a concrete entry, and crossing the map adds an `<alias>` level.
        let props = node.get("properties").and_then(Value::as_object);
        let next = props.and_then(|p| p.get(seg)).map(|s| resolve(s, defs));
        let Some(next) = next else {
            return Err(format!(
                "config-fields: path segment `{seg}` not found in `{path}`"
            ));
        };
        display_segments.push(seg.to_string());
        node = next;
        // If this node is a map, step into its value type and record `<alias>`.
        if let Some(add) = node.get("additionalProperties")
            && add.is_object()
        {
            node = resolve(add, defs);
            display_segments.push("<alias>".to_string());
        }
    }

    if node.get("properties").and_then(Value::as_object).is_none() {
        return Err(format!("config-fields: `{path}` has no fields to render"));
    }

    let prefix = display_segments.join(".");
    Ok(field_table(node, include_enabled, Some(&prefix), defaults))
}

/// The set of field names a section path resolves to (after descending any map
/// to its value type). Used to compute the shared base field set across many
/// sibling sections (e.g. every model-provider slot) so a directive can render
/// the common fields once and only the per-section extras per entry.
pub fn section_field_names(root: &Value, path: &str) -> std::collections::BTreeSet<String> {
    let empty = Map::new();
    let defs = root
        .get("$defs")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let mut node = resolve(root, defs);
    for seg in path.split('.') {
        let Some(next) = node
            .get("properties")
            .and_then(Value::as_object)
            .and_then(|p| p.get(seg))
            .map(|s| resolve(s, defs))
        else {
            return Default::default();
        };
        node = next;
        if let Some(add) = node.get("additionalProperties")
            && add.is_object()
        {
            node = resolve(add, defs);
        }
    }
    node.get("properties")
        .and_then(Value::as_object)
        .map(|p| p.keys().cloned().collect())
        .unwrap_or_default()
}

/// Like [`field_table_for_path`] but omits every field whose name is in
/// `exclude`. Lets a directive render a shared base table once and then only
/// the per-section extras, instead of repeating the common fields for every
/// sibling section. Returns an empty string (not an error) when nothing remains
/// after exclusion, so callers can render a "no extra fields" note.
pub fn field_table_for_path_excluding(
    root: &Value,
    path: &str,
    include_enabled: bool,
    defaults: Option<&Value>,
    exclude: &std::collections::BTreeSet<String>,
) -> Result<String, String> {
    let empty = Map::new();
    let defs = root
        .get("$defs")
        .and_then(Value::as_object)
        .unwrap_or(&empty);

    let mut node = resolve(root, defs).clone();
    let mut display_segments: Vec<String> = Vec::new();
    {
        let mut cur = &node as &Value;
        for seg in path.split('.') {
            let Some(next) = cur
                .get("properties")
                .and_then(Value::as_object)
                .and_then(|p| p.get(seg))
                .map(|s| resolve(s, defs).clone())
            else {
                return Err(format!(
                    "model-provider-fields: path segment `{seg}` not found in `{path}`"
                ));
            };
            display_segments.push(seg.to_string());
            node = next;
            if let Some(add) = node.get("additionalProperties").cloned()
                && add.is_object()
            {
                node = resolve(&add, defs).clone();
                display_segments.push("<alias>".to_string());
            }
            cur = &node;
        }
    }

    // Strip excluded fields from the resolved node's properties.
    if let Some(props) = node.get_mut("properties").and_then(Value::as_object_mut) {
        props.retain(|k, _| !exclude.contains(k));
        if props.is_empty() {
            return Ok(String::new());
        }
    } else {
        return Ok(String::new());
    }

    // Carry `$defs` into the detached node so `$ref` field types still resolve.
    if let (Some(node_obj), Some(defs_val)) = (node.as_object_mut(), root.get("$defs")) {
        node_obj.insert("$defs".to_string(), defs_val.clone());
    }

    let prefix = display_segments.join(".");
    Ok(field_table(&node, include_enabled, Some(&prefix), defaults))
}

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
pub fn field_table(
    root: &Value,
    include_enabled: bool,
    prefix: Option<&str>,
    defaults: Option<&Value>,
) -> String {
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
    // Dashboard deep-link path. The web dashboard routes `/config/<section>/
    // <type>` where `<type>` is the map key and `<section>` is the dot-joined
    // prefix before it. `channels.mattermost.<alias>` -> `channels/mattermost`;
    // `providers.models.venice.<alias>` -> `providers.models/venice`; a bare
    // `acp` section (no `<alias>`) stays `acp`.
    let section_owned = {
        let segs: Vec<&str> = prefix.split('.').collect();
        if let Some(alias_idx) = segs.iter().position(|s| *s == "<alias>") {
            let type_idx = alias_idx.saturating_sub(1);
            let head = segs[..type_idx].join(".");
            if head.is_empty() {
                segs[type_idx].to_string()
            } else {
                format!("{head}/{}", segs[type_idx])
            }
        } else {
            prefix.to_string()
        }
    };
    let section = section_owned.as_str();

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
        let fallback = defaults.and_then(|d| d.get(key));
        let default = fmt_default_for(resolved, fallback);
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

    // Family-map container (e.g. `providers.models`, `channels`): every field
    // is a `HashMap<String, T>` slot. Listing all slots here, each an empty
    // `map | —` row, then recursing into every one, duplicates the per-slot
    // detail that already lives on the dedicated section page. Collapse to a
    // single note instead. Detected structurally (all fields are maps), not by
    // a hardcoded path, so it can never drift.
    let all_maps = !props.is_empty()
        && props.values().all(|v| {
            resolve(v, defs)
                .get("additionalProperties")
                .map(Value::is_object)
                .unwrap_or(false)
        });
    if all_maps {
        let slots: Vec<String> = props.keys().map(|k| format!("`{k}`")).collect();
        let _ = writeln!(
            out,
            "One slot per family ({}). Each slot is a `[{path_str}.<slot>.<alias>]` map; \
             see the dedicated section page for the per-field reference.\n",
            slots.join(", ")
        );
        return;
    }

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
                // A field with no `type`/`properties` is a free-form
                // `serde_json::Value` (TOML inline table), e.g. `provider_extra`
                // or `chat_template_kwargs`. Prefer the explicit title if the
                // schema carries one, else label it `table` rather than `any`.
                schema
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("table")
                    .to_owned()
            }
        }
    }
}

/// Format a default value for the table. Prefers the schema's own `default`
/// key; when absent (schemars omits it for `skip_serializing_if` fields), falls
/// back to the field's value in the struct's `Default::default()` instance, so
/// `false`, `[]`, `{}`, and `null` defaults still surface instead of `—`.
fn fmt_default_for(schema: &Value, fallback: Option<&Value>) -> String {
    let value = schema.get("default").or(fallback);
    fmt_value(value)
}

fn fmt_default(schema: &Value) -> String {
    fmt_value(schema.get("default"))
}

fn fmt_value(value: Option<&Value>) -> String {
    match value {
        Some(Value::Bool(b)) => format!("`{b}`"),
        Some(Value::String(s)) if s.is_empty() => "`\"\"`".to_owned(),
        Some(Value::String(s)) => format!("`\"{s}\"`"),
        Some(Value::Number(n)) => format!("`{n}`"),
        Some(Value::Null) => "`null`".to_owned(),
        Some(Value::Array(a)) if a.is_empty() => "`[]`".to_owned(),
        Some(Value::Object(o)) if o.is_empty() => "`{}`".to_owned(),
        Some(v) => format!("`{v}`"),
        None => "`—`".to_owned(),
    }
}

fn first_line(s: Option<&str>) -> String {
    s.and_then(|d| d.lines().next()).unwrap_or("").to_owned()
}
