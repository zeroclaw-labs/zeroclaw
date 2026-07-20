//! Generate plugin ABI snippet numbers for the mdBook from canonical source:
//! the WIT contracts (`wit/v0/*.wit`) and the registry install cap
//! (`src/plugin_registry.rs`). The contracts are the single source of truth;
//! the docs render whatever they carry so a flag or function added to a world

use anyhow::{Context, Result, bail};
use std::path::Path;

const SNIPPET_DIR: &str = "docs/book/src/_snippets";
const CHANNEL_WIT: &str = "wit/v0/channel.wit";
const MEMORY_WIT: &str = "wit/v0/memory.wit";
const PLUGIN_REGISTRY: &str = "src/plugin_registry.rs";
const CHANNEL_GUIDE: &str = "docs/book/src/plugins/writing-a-channel-plugin.md";
const MEMORY_GUIDE: &str = "docs/book/src/plugins/writing-a-memory-plugin.md";

pub fn run(root: &Path) -> Result<()> {
    let dir = root.join(SNIPPET_DIR);
    std::fs::create_dir_all(&dir)?;

    let channel = read(root, CHANNEL_WIT)?;
    let memory = read(root, MEMORY_WIT)?;

    let channel_flags = parse_flags(&channel, "channel-capabilities")?;
    let memory_flags = parse_flags(&memory, "memory-capabilities")?;
    let channel_funcs = count_funcs(interface_block(&channel, "channel")?);
    let channel_required = channel_funcs
        .checked_sub(channel_flags.len())
        .context("channel WIT has more capability flags than exported functions")?;

    assert_flags_documented(root, CHANNEL_GUIDE, &channel_flags)?;
    assert_flags_documented(root, MEMORY_GUIDE, &memory_flags)?;

    write_inline(&dir, "plugin-channel-flag-count.md", channel_flags.len())?;
    write_inline(&dir, "plugin-channel-func-count.md", channel_funcs)?;
    write_inline(&dir, "plugin-channel-required-count.md", channel_required)?;
    write_inline(
        &dir,
        "plugin-channel-multi-delay-default-ms.md",
        parse_delay_default(&channel)? as usize,
    )?;
    write_inline(&dir, "plugin-memory-flag-count.md", memory_flags.len())?;
    write_inline(
        &dir,
        "plugin-archive-max-mib.md",
        parse_zip_mib(&read(root, PLUGIN_REGISTRY)?)? as usize,
    )?;

    eprintln!("==> Generated plugin ABI snippets from WIT contracts + registry cap");
    Ok(())
}

fn read(root: &Path, rel: &str) -> Result<String> {
    std::fs::read_to_string(root.join(rel))
        .with_context(|| format!("{rel} missing; it is a canonical source for plugin docs"))
}

fn write_inline(dir: &Path, name: &str, value: usize) -> Result<()> {
    std::fs::write(dir.join(name), value.to_string())?;
    Ok(())
}

fn interface_block<'a>(src: &'a str, name: &str) -> Result<&'a str> {
    let needle = format!("interface {name} {{");
    let start = src
        .find(&needle)
        .with_context(|| format!("`{needle}` not found in WIT"))?;
    let rest = &src[start..];
    let end = rest
        .find("\n}")
        .with_context(|| format!("unterminated `interface {name}` block"))?;
    Ok(&rest[..end])
}

fn count_funcs(block: &str) -> usize {
    block.matches(": func").count()
}

fn parse_flags(src: &str, name: &str) -> Result<Vec<String>> {
    let needle = format!("flags {name} {{");
    let start = src
        .find(&needle)
        .with_context(|| format!("`{needle}` not found in WIT"))?;
    let rest = &src[start + needle.len()..];
    let end = rest
        .find('}')
        .with_context(|| format!("unterminated `flags {name}` block"))?;
    let flags: Vec<String> = rest[..end]
        .lines()
        .filter_map(|l| {
            let t = l.trim().trim_end_matches(',');
            (!t.is_empty() && !t.starts_with("//")).then(|| t.to_string())
        })
        .collect();
    if flags.is_empty() {
        bail!("parsed zero flags from `{name}`; WIT layout changed, update the parser");
    }
    Ok(flags)
}

/// The default the host substitutes when `multi-message-delay-ms` is unset,
/// from the flag-default doc table in channel.wit (kept in lockstep with
/// `wasm_channel.rs` by the WIT contract's own review gate).
fn parse_delay_default(src: &str) -> Result<u64> {
    src.lines()
        .find_map(|l| {
            if !(l.contains("multi-message-delay-ms") && l.contains('\u{2192}')) {
                return None;
            }
            l.rsplit('`').nth(1)?.parse().ok()
        })
        .context("could not parse the multi-message-delay-ms default from channel.wit")
}

fn parse_zip_mib(src: &str) -> Result<u64> {
    let line = src
        .lines()
        .find(|l| l.contains("MAX_PLUGIN_ZIP_BYTES") && l.contains('='))
        .context("MAX_PLUGIN_ZIP_BYTES not found in plugin_registry.rs")?;
    let expr = line
        .split('=')
        .nth(1)
        .context("malformed MAX_PLUGIN_ZIP_BYTES line")?
        .trim()
        .trim_end_matches(';');
    let mut bytes: u64 = 1;
    for factor in expr.split('*') {
        bytes *= factor
            .trim()
            .parse::<u64>()
            .with_context(|| format!("non-numeric factor `{factor}` in MAX_PLUGIN_ZIP_BYTES"))?;
    }
    Ok(bytes / (1024 * 1024))
}

fn assert_flags_documented(root: &Path, guide: &str, flags: &[String]) -> Result<()> {
    let text = read(root, guide)?;
    let missing: Vec<&String> = flags
        .iter()
        .filter(|f| !text.contains(&format!("`{f}`")))
        .collect();
    if !missing.is_empty() {
        bail!(
            "{guide} does not document capability flags {missing:?}; the WIT flag set \
             changed — update the guide's capability table"
        );
    }
    Ok(())
}
