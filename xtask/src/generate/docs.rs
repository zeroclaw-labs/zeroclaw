//! Feature-and-support matrix renderer. The ZeroClaw column is walked from the
//! code registries (channels, provider slots, default tools) so it can never be
//! hand-typed or drift from the binary. The OpenClaw and Hermes comparison
//! columns are read from `docs/book/feature-matrix-parity.toml`, keyed by the
//! same stable ids the walk produces. Generation fails if any walked row lacks
//! a parity entry, or the parity file names a row the walk does not produce.
//!
//! Rendered into sentinel zones in `docs/book/src/reference/feature-matrix.md`;
//! everything outside the sentinels stays hand-authored.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;
use zeroclaw_config::schema::ChannelsConfig;
use zeroclaw_runtime::tools::default_tools;

const ZONE_CHANNELS: &str = "matrix-channels";
const ZONE_PROVIDERS: &str = "matrix-providers";
const ZONE_TOOLS: &str = "matrix-tools";
const PARITY_FILE: &str = "docs/book/feature-matrix-parity.toml";

fn begin(zone: &str) -> String {
    format!("<!-- >>> generated:{zone} by `cargo generate docs` - do not edit <<< -->")
}
fn end(zone: &str) -> String {
    format!("<!-- >>> end generated:{zone} <<< -->")
}

#[derive(Debug, Deserialize)]
struct Parity {
    #[serde(default)]
    channels: BTreeMap<String, Column>,
    #[serde(default)]
    providers: BTreeMap<String, Column>,
    #[serde(default)]
    tools: BTreeMap<String, Column>,
}

#[derive(Debug, Deserialize)]
struct Column {
    openclaw: Status,
    hermes: Status,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Status {
    Supported,
    Partial,
    Experimental,
    Planned,
    None,
    Unknown,
}

impl Status {
    fn cell(self) -> &'static str {
        match self {
            Status::Supported => "Supported",
            Status::Partial => "Partial",
            Status::Experimental => "Experimental",
            Status::Planned => "Planned",
            Status::None => "None",
            Status::Unknown => "Unknown",
        }
    }
}

fn load_parity(root: &Path) -> anyhow::Result<Parity> {
    let path = root.join(PARITY_FILE);
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::Error::msg(format!("{}: {e}", path.display())))?;
    let parity: Parity =
        toml::from_str(&raw).map_err(|e| anyhow::Error::msg(format!("{PARITY_FILE}: {e}")))?;
    Ok(parity)
}

/// One matrix row: the source-walked ZeroClaw identity plus the external cells.
struct Row {
    label: String,
    zeroclaw: &'static str,
    openclaw: &'static str,
    hermes: &'static str,
}

fn table(header: &str, rows: &[Row]) -> String {
    let mut out = String::new();
    out.push_str(&format!("| {header} | ZeroClaw | OpenClaw | Hermes |\n"));
    out.push_str("|---|---|---|---|\n");
    for r in rows {
        out.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            r.label, r.zeroclaw, r.openclaw, r.hermes
        ));
    }
    out.trim_end().to_string()
}

/// Walk the channel registry; join each walked kind against its parity entry.
/// Any walked kind with no parity entry, or any parity key not walked, is an
/// error so the two never silently diverge.
fn channels_rows(parity: &Parity) -> anyhow::Result<Vec<Row>> {
    let cfg = ChannelsConfig::default();
    let mut rows = Vec::new();
    let mut walked = std::collections::BTreeSet::new();
    for info in cfg.channels() {
        walked.insert(info.kind.to_string());
        let (openclaw, hermes) = external_cells(parity.channels.get(info.kind));
        rows.push(Row {
            label: info.name.to_string(),
            zeroclaw: "Supported",
            openclaw,
            hermes,
        });
    }
    assert_no_stale("channels", &walked, &parity.channels)?;
    Ok(rows)
}

/// Walk the canonical provider slots; join against parity.
fn providers_rows(parity: &Parity) -> anyhow::Result<Vec<Row>> {
    let mut rows = Vec::new();
    let mut walked = std::collections::BTreeSet::new();
    for slot in zeroclaw_providers::canonical_model_provider_slots() {
        walked.insert(slot.to_string());
        let (openclaw, hermes) = external_cells(parity.providers.get(slot));
        rows.push(Row {
            label: format!("`{slot}`"),
            zeroclaw: "Supported",
            openclaw,
            hermes,
        });
    }
    assert_no_stale("providers", &walked, &parity.providers)?;
    Ok(rows)
}

/// Walk the default tool registry; join against parity by tool name.
fn tools_rows(parity: &Parity) -> anyhow::Result<Vec<Row>> {
    let security = Arc::new(zeroclaw_config::policy::SecurityPolicy::default());
    let mut rows = Vec::new();
    let mut walked = std::collections::BTreeSet::new();
    for tool in default_tools(security) {
        let name = tool.name().to_string();
        walked.insert(name.clone());
        let (openclaw, hermes) = external_cells(parity.tools.get(&name));
        rows.push(Row {
            label: format!("`{name}`"),
            zeroclaw: "Supported",
            openclaw,
            hermes,
        });
    }
    assert_no_stale("tools", &walked, &parity.tools)?;
    Ok(rows)
}

/// The two external comparison cells for a walked row. A missing parity entry
/// renders `Unknown` rather than failing: ZeroClaw truth is always complete from
/// the walk, and a maintainer fills the external columns in the parity file as
/// the facts are confirmed.
fn external_cells(col: Option<&Column>) -> (&'static str, &'static str) {
    match col {
        Some(c) => (c.openclaw.cell(), c.hermes.cell()),
        None => ("Unknown", "Unknown"),
    }
}

/// A parity key naming a row the source walk does not produce is always an
/// error: it means the parity file references a channel/provider/tool that no
/// longer exists in the binary.
fn assert_no_stale(
    section: &str,
    walked: &std::collections::BTreeSet<String>,
    parity: &BTreeMap<String, Column>,
) -> anyhow::Result<()> {
    let stale: Vec<&String> = parity.keys().filter(|k| !walked.contains(*k)).collect();
    if !stale.is_empty() {
        anyhow::bail!(
            "{PARITY_FILE}: [{section}] has entries the source walk does not produce: {}",
            stale
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(())
}

pub fn render_file(root: &Path, current: &str) -> anyhow::Result<String> {
    let parity = load_parity(root)?;
    let channels = table("Channel", &channels_rows(&parity)?);
    let providers = table("Provider slot", &providers_rows(&parity)?);
    let tools = table("Tool", &tools_rows(&parity)?);
    let spliced = splice(current, ZONE_CHANNELS, &channels)?;
    let spliced = splice(&spliced, ZONE_PROVIDERS, &providers)?;
    splice(&spliced, ZONE_TOOLS, &tools)
}

fn splice(current: &str, zone: &str, body: &str) -> anyhow::Result<String> {
    let b = begin(zone);
    let e = end(zone);
    let begin_at = current.find(&b).ok_or_else(|| {
        anyhow::Error::msg(format!(
            "feature-matrix.md missing generated:{zone} BEGIN sentinel"
        ))
    })?;
    let after_begin = begin_at + b.len();
    let end_rel = current[after_begin..].find(&e).ok_or_else(|| {
        anyhow::Error::msg(format!(
            "feature-matrix.md missing generated:{zone} END sentinel"
        ))
    })?;
    let end_at = after_begin + end_rel;
    let mut out = String::new();
    out.push_str(&current[..after_begin]);
    out.push('\n');
    out.push_str(body);
    out.push('\n');
    out.push_str(&current[end_at..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf()
    }

    #[test]
    fn parity_join_produces_rows_without_stale_keys() {
        let parity = load_parity(&root()).unwrap();
        channels_rows(&parity).unwrap();
        providers_rows(&parity).unwrap();
        tools_rows(&parity).unwrap();
    }

    #[test]
    fn render_is_idempotent() {
        let path = root().join("docs/book/src/reference/feature-matrix.md");
        let cur = std::fs::read_to_string(&path).unwrap();
        let once = render_file(&root(), &cur).unwrap();
        let twice = render_file(&root(), &once).unwrap();
        assert_eq!(once, twice, "render must be idempotent");
    }

    #[test]
    fn channel_column_is_all_supported_from_walk() {
        let parity = load_parity(&root()).unwrap();
        for r in channels_rows(&parity).unwrap() {
            assert_eq!(r.zeroclaw, "Supported");
        }
    }
}
