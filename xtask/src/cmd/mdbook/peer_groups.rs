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

/// `supports <renderer>`: every renderer is supported (we only touch content).
pub fn supports() -> ! {
    std::process::exit(0);
}

pub fn run() -> anyhow::Result<()> {
    let params = load_params()?;

    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let pair: Value = serde_json::from_str(&input)?;
    let mut book = pair
        .get(1)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("preprocessor input missing book element"))?;

    if let Some(items) = book.get_mut("items").and_then(Value::as_array_mut) {
        for item in items.iter_mut() {
            expand_section(item, &params)?;
        }
    }

    println!("{}", serde_json::to_string(&book)?);
    Ok(())
}

fn expand_section(section: &mut Value, params: &[PeerParams]) -> anyhow::Result<()> {
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
            let replaced = expand_directives(&content, params, depth)?;
            chapter["content"] = Value::String(replaced);
        }
        if let Some(sub) = chapter.get_mut("sub_items").and_then(Value::as_array_mut) {
            for item in sub.iter_mut() {
                expand_section(item, params)?;
            }
        }
    }
    Ok(())
}

fn expand_directives(content: &str, params: &[PeerParams], depth: usize) -> anyhow::Result<String> {
    // Directives, longest marker first so a prefix never shadows a longer name.
    const MARKERS: &[&str] = &[
        "{{#peer-group-example ",
        "{{#config-where ",
        "{{#secret-config ",
        "{{#peer-group ",
    ];
    let mut out = String::with_capacity(content.len());
    let mut rest = content;
    loop {
        let Some((start, marker)) = MARKERS
            .iter()
            .filter_map(|m| rest.find(m).map(|i| (i, *m)))
            .min_by_key(|(i, _)| *i)
        else {
            break;
        };
        out.push_str(&rest[..start]);
        let after = &rest[start + marker.len()..];
        let end = after
            .find("}}")
            .ok_or_else(|| anyhow::anyhow!("unterminated {marker} directive"))?;
        let arg = after[..end].trim();
        let rendered = match marker {
            "{{#config-where " => render_config_where(arg, depth),
            "{{#secret-config " => render_secret_config(arg),
            "{{#peer-group-example " => render_example(lookup(params, arg)?),
            _ => render(lookup(params, arg)?, depth),
        };
        out.push_str(&rendered);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Relative prefix from a page at `depth` directories deep up to `src/`.
fn rel_prefix(depth: usize) -> String {
    if depth == 0 {
        "./".to_string()
    } else {
        "../".repeat(depth)
    }
}

fn lookup<'a>(params: &'a [PeerParams], key: &str) -> anyhow::Result<&'a PeerParams> {
    params
        .iter()
        .find(|p| p.key == key)
        .ok_or_else(|| anyhow::anyhow!("unknown peer-group channel '{key}'"))
}

/// Render a "where to configure this" widget for a dotted config path. Tabs by
/// config surface: the config file, the zerocode Config pane, and the gateway
/// dashboard. Emits `.os-tabs-src` markup so it reuses the existing
/// tabbed-widget CSS/JS. Paths are snake_case, matching the TOML.
fn render_config_where(path: &str, depth: usize) -> String {
    format!(
        r#"<div class="os-tabs-src">

#### Gateway dashboard

Open [`/config/{path}`](http://127.0.0.1:42617/config/{path}) in the web dashboard.

#### zerocode

In the **Config** pane, under the `{path}` section.

#### config.toml

In `~/.zeroclaw/config.toml`, under the [`[{path}]`]({prefix}reference/config.md#{path}) section.

</div>"#,
        path = path,
        prefix = rel_prefix(depth),
    )
}

/// Render a secret-field setter widget. Secrets are stored encrypted; they must
/// never be hand-written into `config.toml`. Tabs cover only the surfaces that
/// encrypt on write: the gateway dashboard, zerocode, and `zeroclaw config set`
/// (masked input). The arg is the full dotted path to the secret field.
fn render_secret_config(path: &str) -> String {
    // Section = first dotted segment, for the dashboard deep-link.
    let section = path.split('.').next().unwrap_or(path);
    format!(
        r#"> **`{path}` is a secret.** It is stored encrypted — never put it in plain
> `config.toml`. Set it through one of these, which encrypt on write:

<div class="os-tabs-src">

#### Gateway dashboard

Open [`/config/{section}`](http://127.0.0.1:42617/config/{section}) and set the field there.

#### zerocode

In the **Config** pane, set the `{path}` field (input is masked).

#### zeroclaw config

```sh
zeroclaw config set {path}    # prompts for masked input, stores encrypted
```

</div>"#,
        path = path,
        section = section,
    )
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

fn render(p: &PeerParams, depth: usize) -> String {
    format!(
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
        where_widget = render_config_where("peer_groups", depth),
    )
}

fn load_params() -> anyhow::Result<Vec<PeerParams>> {
    let root = repo_root();
    let path = book_dir(&root).join("peer-groups.toml");
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
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
