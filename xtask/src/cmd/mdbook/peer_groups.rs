//! mdBook preprocessor: expand `{{#peer-group <channel>}}` directives.
//!
//! Implements the mdBook preprocessor protocol directly over JSON (no `mdbook`
//! crate dependency). mdBook invokes this as:
//!
//!   * `mdbook preprocess supports <renderer>` — exit 0 if supported.
//!   * `mdbook preprocess` — stdin is `[context, book]` JSON; stdout is the
//!     modified `book` JSON.
//!
//! A page writes `{{#peer-group matrix}}`; the preprocessor renders the single
//! canonical peer-group block from `docs/book/peer-groups.toml` inline, so the
//! page passes the parameter and exactly one template exists. Channel keys are
//! validated against the canonical channel inventory in `zeroclaw-config`.

use crate::util::{book_dir, repo_root};
use serde::Deserialize;
use serde_json::Value;
use std::io::Read;

#[derive(Deserialize)]
struct PeerParams {
    key: String,
    sender_desc: String,
    sender_example: String,
    #[serde(default)]
    agents_example: Vec<String>,
    #[serde(default)]
    ignore_example: Option<String>,
}

#[derive(Deserialize)]
struct ParamFile {
    channel: Vec<PeerParams>,
}

#[derive(Deserialize)]
struct EnvVarParams {
    path: String,
    #[serde(default)]
    comment: Option<String>,
    value: String,
    group: String,
    #[serde(default)]
    table: bool,
    #[serde(default)]
    prefix: Option<String>,
    #[serde(default)]
    suffix: Option<String>,
    #[serde(default)]
    assign: Option<String>,
}

#[derive(Deserialize)]
struct EnvVarFile {
    var: Vec<EnvVarParams>,
}

/// `supports <renderer>`: every renderer is supported (we only touch content).
pub fn supports() -> ! {
    std::process::exit(0);
}

pub fn run() -> anyhow::Result<()> {
    let params = load_params()?;
    let env_vars = load_env_var_params()?;

    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let pair: Value = serde_json::from_str(&input)?;
    let mut book = pair
        .get(1)
        .cloned()
        .ok_or_else(|| anyhow::Error::msg("preprocessor input missing book element"))?;

    if let Some(items) = book.get_mut("items").and_then(Value::as_array_mut) {
        for item in items.iter_mut() {
            expand_section(item, &params, &env_vars)?;
        }
    }

    println!("{}", serde_json::to_string(&book)?);
    Ok(())
}

fn expand_section(
    section: &mut Value,
    params: &[PeerParams],
    env_vars: &[EnvVarParams],
) -> anyhow::Result<()> {
    if let Some(chapter) = section.get_mut("Chapter") {
        // Depth = number of path separators in the chapter's source path, so a
        // page at `channels/matrix.md` (depth 1) links to the reference with
        // one `../`, and a root page (`introduction.md`, depth 0) with none.
        let depth = chapter
            .get("path")
            .and_then(Value::as_str)
            .map(|p| p.matches('/').count())
            .unwrap_or(0);
        let content_owned = chapter
            .get("content")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let Some(content) = content_owned {
            let replaced = expand_directives(&content, params, env_vars, depth)?;
            chapter["content"] = Value::String(replaced);
        }
        if let Some(sub) = chapter.get_mut("sub_items").and_then(Value::as_array_mut) {
            for item in sub.iter_mut() {
                expand_section(item, params, env_vars)?;
            }
        }
    }
    Ok(())
}

fn expand_directives(
    content: &str,
    params: &[PeerParams],
    env_vars: &[EnvVarParams],
    depth: usize,
) -> anyhow::Result<String> {
    // Directives, longest marker first so a prefix never shadows a longer name.
    const MARKERS: &[&str] = &[
        "{{#peer-group-example ",
        "{{#model-provider-catalog-table",
        "{{#thread-context ",
        "{{#config-fields ",
        "{{#env-var-bridge",
        "{{#env-var-table",
        "{{#env-var-name ",
        "{{#config-where ",
        "{{#secret-config ",
        "{{#peer-group ",
        "{{#env-var ",
    ];
    let mut out = String::with_capacity(content.len());
    let mut rest = content;
    while let Some((start, marker)) = MARKERS
        .iter()
        .filter_map(|m| rest.find(m).map(|i| (i, *m)))
        .min_by_key(|(i, _)| *i)
    {
        out.push_str(&rest[..start]);
        let after = &rest[start + marker.len()..];
        let end = after
            .find("}}")
            .ok_or_else(|| anyhow::Error::msg(format!("unterminated {marker} directive")))?;
        let arg = after[..end].trim();
        let rendered = match marker {
            "{{#config-where " => render_config_where(arg, depth)?,
            "{{#config-fields " => render_config_fields(arg)?,
            "{{#secret-config " => render_secret_config(arg),
            "{{#thread-context " => render_thread_context(arg)?,
            "{{#peer-group-example " => render_example(lookup(params, arg)?),
            "{{#env-var-table" => render_env_var_table(env_vars),
            "{{#model-provider-catalog-table" => render_model_provider_catalog_table(),
            "{{#env-var-bridge" => render_env_var_bridge(env_vars)?,
            "{{#env-var-name " => render_env_var_name(arg)?,
            "{{#env-var " => render_env_var_block(env_vars, arg)?,
            _ => render(lookup(params, arg)?, depth)?,
        };
        out.push_str(&rendered);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

fn lookup<'a>(params: &'a [PeerParams], key: &str) -> anyhow::Result<&'a PeerParams> {
    params
        .iter()
        .find(|p| p.key == key)
        .ok_or_else(|| anyhow::Error::msg(format!("unknown peer-group channel '{key}'")))
}

/// Render a "where to configure this" widget for a config section path. Tabs by
/// surface: the gateway dashboard and the zerocode Config pane. The section is
/// validated against the canonical section registry; the zerocode label comes
/// from `Section::label()`, so a non-existent section fails the build and the
/// label can never drift from the real UI.
fn render_config_where(path: &str, depth: usize) -> anyhow::Result<String> {
    let _ = depth;
    let label = config_section_label(path)?;
    Ok(format!(
        r#"<div class="os-tabs-src">

#### Gateway dashboard

Open [`/config/{path}`](http://127.0.0.1:42617/config/{path}) in the web dashboard.

#### zerocode

In the **Config** pane, under **{label}**.

</div>"#,
        path = path,
        label = label,
    ))
}

/// Render a channel's full field-reference table directly from its config
/// struct's JSON Schema, so the table can never drift from the schema. The arg
/// is the channel key (`mattermost`, `matrix`, …), parsed into the canonical
/// `ChannelKind` registry. New promoted channel pages map their variant here
/// once; the schema stays the single source of truth for fields, types,
/// defaults, and descriptions.
fn render_config_fields(arg: &str) -> anyhow::Result<String> {
    use std::str::FromStr;
    use zeroclaw_api::attribution::ChannelKind;
    use zeroclaw_config::schema;

    let key = arg.trim();
    // Doc pages address a channel by its config-section key (`whatsapp`,
    // `acp`, `nextcloud_talk`), which can differ from the `ChannelKind`
    // snake_case serialize form (`whatsapp_business`, `acp_channel`). Try the
    // registry first, then the two page-key aliases, then match on the enum
    // variant so there is no string-literal dispatch.
    const WHATSAPP_PAGE_KEY: &str = "whatsapp";
    const ACP_PAGE_KEY: &str = "acp";
    let kind = if let Ok(k) = ChannelKind::from_str(key) {
        k
    } else if key == WHATSAPP_PAGE_KEY {
        ChannelKind::WhatsappBusiness
    } else if key == ACP_PAGE_KEY {
        ChannelKind::AcpChannel
    } else {
        anyhow::bail!("config-fields: `{key}` is not a known channel key");
    };

    macro_rules! table_for {
        ($ty:ty, $prefix:expr) => {{
            let s = schemars::schema_for!($ty);
            zeroclaw_config::schema_markdown::field_table(&s.to_value(), false, Some($prefix))
        }};
    }

    let table = match kind {
        ChannelKind::Matrix => table_for!(schema::MatrixConfig, "channels.matrix.<alias>"),
        ChannelKind::Mattermost => {
            table_for!(schema::MattermostConfig, "channels.mattermost.<alias>")
        }
        ChannelKind::Slack => table_for!(schema::SlackConfig, "channels.slack.<alias>"),
        ChannelKind::Discord => table_for!(schema::DiscordConfig, "channels.discord.<alias>"),
        ChannelKind::Telegram => table_for!(schema::TelegramConfig, "channels.telegram.<alias>"),
        ChannelKind::Signal => table_for!(schema::SignalConfig, "channels.signal.<alias>"),
        ChannelKind::Webhook => table_for!(schema::WebhookConfig, "channels.webhook.<alias>"),
        ChannelKind::Line => table_for!(schema::LineConfig, "channels.line.<alias>"),
        ChannelKind::Irc => table_for!(schema::IrcConfig, "channels.irc.<alias>"),
        ChannelKind::Qq => table_for!(schema::QQConfig, "channels.qq.<alias>"),
        ChannelKind::Nostr => table_for!(schema::NostrConfig, "channels.nostr.<alias>"),
        ChannelKind::NextcloudTalk => {
            table_for!(
                schema::NextcloudTalkConfig,
                "channels.nextcloud_talk.<alias>"
            )
        }
        ChannelKind::WhatsappBusiness | ChannelKind::WhatsappWeb => {
            table_for!(schema::WhatsAppConfig, "channels.whatsapp.<alias>")
        }
        ChannelKind::AcpChannel => table_for!(schema::AcpConfig, "acp"),
        other => {
            let name: &'static str = other.into();
            anyhow::bail!(
                "config-fields: channel `{name}` has no promoted page mapping yet. Add its config struct to render_config_fields in xtask/src/cmd/mdbook/peer_groups.rs."
            )
        }
    };
    Ok(table)
}

/// Resolve the display label for a config section path. Prefers the curated
/// `Section` registry label; falls back to the schema-humanized key for real
/// schema sections that aren't curated quickstart sections (e.g. `browser`).
/// Errors only when the path matches neither — so a fabricated section fails
/// the build.
fn config_section_label(path: &str) -> anyhow::Result<String> {
    use zeroclaw_config::schema::Config;
    if let Some(section) = zeroclaw_config::sections::Section::from_key(path) {
        return Ok(section.label());
    }
    let prefix = format!("{path}.");
    let is_schema_section = Config::map_key_sections().iter().any(|s| s.path == path)
        || Config::default()
            .prop_fields()
            .iter()
            .any(|f| f.name == path || f.name.starts_with(&prefix));
    if !is_schema_section {
        anyhow::bail!("config-where section '{path}' is not a known config section");
    }
    Ok(zeroclaw_config::sections::humanize_section_key(path))
}

/// Render a secret-field setter widget. Secrets are stored encrypted; they must
/// never be hand-written into `config.toml`. Tabs cover only the surfaces that
/// encrypt on write: the gateway dashboard, zerocode, and `zeroclaw config set`
/// (masked input). The arg is the full dotted path to the secret field.
fn render_secret_config(path: &str) -> String {
    let path = path.trim();
    // Dashboard deep-link path: dotted prefix minus `<alias>` and the field,
    // slash-joined (`channels.matrix.<alias>.password` -> `channels/matrix`).
    let section = dashboard_section(path);
    format!(
        r#"> **`{path}` is a secret.** Stored encrypted, never in plain
> `config.toml`. Set it through one of these, which encrypt on write:

<div class="os-tabs-src">

#### Gateway dashboard

Open [`/config/{section}`](http://127.0.0.1:42617/config/{section}) and set the `{path}` field there.

#### zerocode

In the **Config** pane, set the `{path}` field (input is masked).

#### zeroclaw config

```sh
zeroclaw config set {path}    # prompts for masked input, stores encrypted
```

</div>"#,
    )
}

/// Dashboard deep-link section path from a dotted config field path. Drops the
/// `<alias>` placeholder and the trailing field name, slash-joining the rest:
/// `channels.matrix.<alias>.password` -> `channels/matrix`. A bare section like
/// `acp.foo` -> `acp`. The gateway resolves these `/config/<section>` routes.
fn dashboard_section(field_path: &str) -> String {
    let segs: Vec<&str> = field_path.split('.').filter(|s| *s != "<alias>").collect();
    // Drop the trailing field name; keep the section prefix.
    let keep = segs.len().saturating_sub(1).max(1);
    segs[..keep].join("/")
}

/// live in threads. Args are `key="value"` pairs:
///   - `channel` (required): display name, e.g. `Slack`, `Matrix`.
///   - `prop` (optional): the channel's thread-reply config property, e.g.
///     `thread_replies`. When present, the channel exposes a toggle and the
///     copy names it; when absent (threads are native, no toggle, e.g.
///     Discord), the toggle sentence is dropped.
///   - `path` (optional): the full dotted config path to `prop`, e.g.
///     `channels.matrix.<alias>.reply_in_thread`. When present, renders the
///     set-it-three-ways surface tabs so the section is actionable.
fn render_thread_context(arg: &str) -> anyhow::Result<String> {
    let kv = parse_kv_args(arg);
    let channel = kv.get("channel").filter(|s| !s.is_empty()).ok_or_else(|| {
        anyhow::Error::msg(format!(
            "thread-context directive needs channel=\"...\"; got `{arg}`"
        ))
    })?;
    let prop = kv.get("prop").filter(|s| !s.is_empty());
    let path = kv.get("path").filter(|s| !s.is_empty());

    let toggle = match prop {
        Some(p) => format!(
            " For {channel} this is controlled by `{p}`: when it is on, top-level \
             messages open a thread and each thread is a separate conversation; \
             when off, replies post at the channel root and history is keyed by \
             sender and target instead of by thread."
        ),
        None => format!(
            " For {channel}, threads are native channels, so each thread is \
             already a separate conversation: no toggle to set."
        ),
    };

    let configure = match path {
        Some(p) => {
            let section = dashboard_section(p);
            format!(
                r#"

Set the thread behavior on any surface:

<div class="os-tabs-src">

#### Gateway dashboard

Open [`/config/{section}`](http://127.0.0.1:42617/config/{section}) and toggle the `{p}` field.

#### zerocode

In the **Config** pane, set the `{p}` field.

#### zeroclaw config

```sh
zeroclaw config set {p} true     # thread replies on
zeroclaw config set {p} false    # replies at the channel root
```

</div>"#
            )
        }
        None => String::new(),
    };

    Ok(format!(
        r#"When a {channel} conversation happens in a thread, that thread is its own
conversation. ZeroClaw derives a distinct session key per thread, so every
thread carries an independent context window and history: messages in one
thread never bleed into another, and the agent does not see a sibling thread's
earlier turns.{toggle}

- **Isolation is the point.** Each thread's context is self-contained: it does
  not leak outside the thread, and nothing from outside the thread leaks in.
  Parallel threads hold separate conversational state, so unrelated tasks never
  contaminate each other.
- **Long threads grow context.** A thread accumulates history while it stays
  active, so a very long thread eventually fills the model's context window
  like any other long conversation. Start a new thread to reset.
- **In-flight work is scoped per thread.** A new message in one thread does not
  cancel an in-flight response in another; each thread's task stands alone.{configure}"#
    ))
}

/// Parse `key="value"` (and bare `key=value`) pairs from a directive arg into a
/// map. Tolerant of extra whitespace; values may contain spaces only when
/// double-quoted.
fn parse_kv_args(arg: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let bytes = arg.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let key_start = i;
        while i < bytes.len() && bytes[i] != b'=' && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if key_start == i {
            break;
        }
        let key = arg[key_start..i].to_string();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            map.insert(key, String::new());
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let value = if i < bytes.len() && bytes[i] == b'"' {
            i += 1;
            let start = i;
            while i < bytes.len() && bytes[i] != b'"' {
                i += 1;
            }
            let v = arg[start..i].to_string();
            if i < bytes.len() {
                i += 1;
            }
            v
        } else {
            let start = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            arg[start..i].to_string()
        };
        map.insert(key, value);
    }
    map
}

fn render_example(p: &PeerParams) -> String {
    let agents = if p.agents_example.is_empty() {
        "no peer agents".to_string()
    } else {
        format!("peer agents {}", p.agents_example.join(", "))
    };
    let ignore = match &p.ignore_example {
        Some(i) => format!(", and blocks `{i}` via `ignore`"),
        None => String::new(),
    };
    format!(
        "A {key} peer group named e.g. `my_{key}_group` sets `channel = \"{key}\"`, \
allows `{example}` in `external_peers`, names {agents}{ignore}. Set it through \
the gateway dashboard, zerocode, or `zeroclaw config set`.",
        key = p.key,
        agents = agents,
        example = p.sender_example,
        ignore = ignore,
    )
}

fn render(p: &PeerParams, depth: usize) -> anyhow::Result<String> {
    Ok(format!(
        r#"Inbound senders are gated against the **peer set** resolved for the bound
agent, drawn from the `peer_groups` config the agent belongs to. Matching strips
a leading `@` and is case-insensitive against the channel's native sender
identifier. An **empty** set denies everyone; a set containing `"*"` accepts
anyone; otherwise only the listed external peers (and peer agents) are accepted.
This is separate from gateway pairing (`gateway.require_pairing`), which
authenticates HTTP/WebSocket clients, not chat-channel senders.

A peer group for {key} sets `channel` to `{key}`, lists the allowed senders in
`external_peers` (for {key}, {sender_desc}; `["*"]` accepts anyone), optionally
names peer `agents` for cross-agent dispatch, an `ignore` blocklist, and an
`output_modality` (`mirror`, `voice`, or `text`). See [Peer Groups](peer-groups.md)
for the field reference.

Where to set this:

{where_widget}"#,
        key = p.key,
        sender_desc = p.sender_desc,
        where_widget = render_config_where("peer_groups", depth)?,
    ))
}

fn load_params() -> anyhow::Result<Vec<PeerParams>> {
    let root = repo_root();
    let path = book_dir(&root).join("peer-groups.toml");
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::Error::msg(format!("reading {}: {e}", path.display())))?;
    let parsed: ParamFile = toml::from_str(&raw)?;
    validate_keys(&parsed.channel)?;
    Ok(parsed.channel)
}

fn validate_keys(params: &[PeerParams]) -> anyhow::Result<()> {
    let inventory = zeroclaw_config::schema::ChannelsConfig::default();
    let known: Vec<&'static str> = inventory.channels().iter().map(|c| c.kind).collect();
    for p in params {
        if !known.contains(&p.key.as_str()) {
            anyhow::bail!(
                "peer-group param key '{}' is not a known channel type; known: {}",
                p.key,
                known.join(", ")
            );
        }
    }
    Ok(())
}

/// Render the two-tab "bridge ecosystem env vars" widget (sh + PowerShell) from
/// the `bridge_sh` and `bridge_ps` groups. The schema-mirror name on the left
/// is derived from a validated path; the ecosystem var on the right lives in
/// each row's `value`. One widget, both tabs, no literal `ZEROCLAW_...` name.
fn render_env_var_bridge(vars: &[EnvVarParams]) -> anyhow::Result<String> {
    let sh = env_var_lines(vars, "bridge_sh")?;
    let ps = env_var_lines(vars, "bridge_ps")?;
    Ok(format!(
        "<div class=\"os-tabs-src\">\n\n#### sh\n\n```sh\n{sh}```\n\n\
#### PowerShell\n\n```powershell\n{ps}```\n\n</div>"
    ))
}

/// Shared line-builder for a single env-var group: honors comment, prefix,
/// assign, and suffix per row. Returns the body text (no fence/tabs).
fn env_var_lines(vars: &[EnvVarParams], group: &str) -> anyhow::Result<String> {
    let mut body = String::new();
    let mut any = false;
    for v in vars.iter().filter(|v| v.group == group) {
        if let Some(comment) = &v.comment {
            body.push_str(&format!("# {comment}\n"));
        }
        let prefix = v.prefix.as_deref().unwrap_or("");
        let suffix = v.suffix.as_deref().unwrap_or("");
        let assign = v.assign.as_deref().unwrap_or("=");
        body.push_str(&format!(
            "{prefix}{}{assign}{}{suffix}\n",
            env_form(&v.path),
            v.value
        ));
        any = true;
    }
    if !any {
        anyhow::bail!("no env-var rows in group '{group}'");
    }
    Ok(body)
}

/// Render a single bare `ZEROCLAW_...` env-var name for inline prose or a
/// one-line code block. The path is validated against the schema exactly like
/// the `env-vars.toml` rows, so an inline reference cannot drift either.
fn render_env_var_name(path: &str) -> anyhow::Result<String> {
    validate_env_var_path(path)?;
    Ok(format!("`{}`", env_form(path)))
}

/// Render the complete model-provider catalog as a table grouped by registry
/// category: one row per canonical slot with its default endpoint and a local
/// marker, all from `zeroclaw_providers::list_model_providers()` +
/// `default_model_provider_url()`. Replaces the hand-typed catalog table so it
/// can never drift from the constructible slot set.
fn render_model_provider_catalog_table() -> String {
    use zeroclaw_providers::ModelProviderCategory as C;
    let category_title = |c: C| match c {
        C::Primary => "Primary",
        C::OpenAiCompatible => "OpenAI-compatible",
        C::FastInference => "Fast inference",
        C::ModelHosting => "Model hosting platforms",
        C::ChineseAi => "Chinese AI",
        C::CloudEndpoint => "Cloud AI endpoints",
    };
    let providers = zeroclaw_providers::list_model_providers();
    let mut out = String::new();
    for category in C::all() {
        let rows: Vec<_> = providers
            .iter()
            .filter(|p| p.category == *category)
            .collect();
        if rows.is_empty() {
            continue;
        }
        out.push_str(&format!("\n### {}\n\n", category_title(*category)));
        out.push_str("| Slot | Default endpoint | Local |\n|---|---|---|\n");
        for p in rows {
            let url = zeroclaw_providers::default_model_provider_url(p.name)
                .map(|u| format!("`{u}`"))
                .unwrap_or_else(|| "—".to_string());
            let local = if p.local { "✓" } else { "" };
            out.push_str(&format!("| `{}` | {} | {} |\n", p.name, url, local));
        }
    }
    out
}

/// `ZEROCLAW_`-prefixed env-var name for a dotted schema path. This is the exact
/// inverse of the runtime resolver in `zeroclaw_config::env_overrides`, which
/// matches an env tail by `field.name.replace('.', "__")`. Keeping the same
/// rule here means a rendered example and the value the runtime accepts can
/// never disagree.
fn env_form(path: &str) -> String {
    format!("ZEROCLAW_{}", path.replace('.', "__"))
}

/// Render the `## Examples` code block from the curated, schema-validated rows
/// in the `example` group. Comments become `#` lines; each row becomes one
/// `ZEROCLAW_...=value` line. No env-var name is literal in the page — every
/// one is derived from a validated schema path.
fn render_env_var_block(vars: &[EnvVarParams], group: &str) -> anyhow::Result<String> {
    let mut body = String::new();
    let mut first = true;
    for v in vars.iter().filter(|v| v.group == group) {
        if let Some(comment) = &v.comment {
            if !first {
                body.push('\n');
            }
            body.push_str(&format!("# {comment}\n"));
        }
        let prefix = v.prefix.as_deref().unwrap_or("");
        let suffix = v.suffix.as_deref().unwrap_or("");
        let assign = v.assign.as_deref().unwrap_or("=");
        body.push_str(&format!(
            "{prefix}{}{assign}{}{suffix}\n",
            env_form(&v.path),
            v.value
        ));
        first = false;
    }
    if first {
        anyhow::bail!("no env-var rows in group '{group}'");
    }
    Ok(format!(
        "<div class=\"os-tabs-src\">\n\n#### sh\n\n```sh\n{body}```\n\n</div>"
    ))
}

/// Render the TOML<->env mapping table from the rows flagged `table = true`.
/// The left column is the dotted path in its `[section] field = "..."` shape;
/// the right is the derived env-var name. Both come from the same validated
/// path, so the table cannot drift from the schema.
fn render_env_var_table(vars: &[EnvVarParams]) -> String {
    let mut rows = String::new();
    for v in vars.iter().filter(|v| v.table) {
        let (section, field) = v
            .path
            .rsplit_once('.')
            .unwrap_or((v.path.as_str(), v.path.as_str()));
        let toml_repr = format!("`[{section}] {field} = \"...\"`");
        rows.push_str(&format!("| {toml_repr} | `{}=...` |\n", env_form(&v.path)));
    }
    format!("| TOML | Env var |\n|---|---|\n{rows}")
}

fn load_env_var_params() -> anyhow::Result<Vec<EnvVarParams>> {
    let root = repo_root();
    let path = book_dir(&root).join("env-vars.toml");
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::Error::msg(format!("reading {}: {e}", path.display())))?;
    let parsed: EnvVarFile = toml::from_str(&raw)?;
    validate_env_var_paths(&parsed.var)?;
    Ok(parsed.var)
}

/// Validate every example `path` against the canonical schema, the same way the
/// runtime resolver does: alias-bearing paths must sit under a real
/// `map_key_sections()` entry; every other path must be a real `prop_fields()`
/// leaf. A renamed or removed field fails the doc build loudly instead of
/// silently rotting into a stale literal.
fn validate_env_var_paths(vars: &[EnvVarParams]) -> anyhow::Result<()> {
    for v in vars {
        validate_env_var_path(&v.path)?;
    }
    Ok(())
}

/// Validate one dotted `path` against the canonical schema, the same way the
/// runtime resolver does: alias-bearing paths must sit under a real
/// `map_key_sections()` entry; every other path must be a real `prop_fields()`
/// leaf. A renamed or removed field fails the doc build loudly instead of
/// silently rotting into a stale literal.
fn validate_env_var_path(path: &str) -> anyhow::Result<()> {
    use zeroclaw_config::schema::Config;
    let config = Config::default();
    let is_leaf = config.prop_fields().into_iter().any(|f| f.name == path);
    if is_leaf {
        return Ok(());
    }
    // Alias-bearing path: `<section>.<alias>[.<field>...]`. The segment after
    // the section is the operator-chosen alias (not a schema field), so it
    // won't appear in `prop_fields()` — validating the section is the correct
    // check.
    let under_section = Config::map_key_sections().into_iter().any(|s| {
        path.strip_prefix(s.path)
            .and_then(|rest| rest.strip_prefix('.'))
            .is_some_and(|rest| !rest.is_empty())
    });
    if !under_section {
        anyhow::bail!(
            "env-var param path '{path}' is not a known schema prop-field and sits \
under no map section; it cannot be derived from the schema"
        );
    }
    Ok(())
}
