use clap::Parser;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "Fill empty/fuzzy .po entries via a configured provider")]
struct Args {
    #[arg(long)]
    po: PathBuf,
    #[arg(long)]
    locale: String,
    /// Re-translate all entries, not just empty/fuzzy ones
    #[arg(long)]
    force: bool,
    /// Entries per API call
    #[arg(long, default_value = "50")]
    batch: usize,
    /// Provider name from [providers.models.<name>] in config.toml
    #[arg(long)]
    provider: String,
}

struct ProviderConfig {
    base_url: String,
    model: String,
    api_key: Option<String>,
}

fn read_provider_config(provider_name: &str) -> anyhow::Result<ProviderConfig> {
    let home = std::env::var("HOME").unwrap_or_else(|_| std::env::var("USERPROFILE").unwrap_or_default());
    let candidates = [
        format!("{home}/.zeroclaw/config.toml"),
        format!("{home}/.config/zeroclaw/config.toml"),
    ];
    let raw = candidates.iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .ok_or_else(|| anyhow::anyhow!("config.toml not found (tried ~/.zeroclaw/config.toml)"))?;
    let table: toml::Table = raw.parse()?;
    let provider = table
        .get("providers").and_then(|v| v.get("models")).and_then(|v| v.get(provider_name))
        .ok_or_else(|| anyhow::anyhow!("[providers.models.{provider_name}] not found in config.toml"))?;
    let model = provider.get("model").and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("No model set for provider '{provider_name}' — add `model = \"...\"` to [providers.models.{provider_name}]"))?
        .to_string();
    Ok(ProviderConfig {
        base_url: provider.get("base_url").and_then(|v| v.as_str())
            .unwrap_or("http://localhost:11434").to_string(),
        model,
        api_key: provider.get("api_key").and_then(|v| v.as_str()).map(str::to_string),
    })
}

/// A parsed .po entry, carrying line positions so we can rewrite in place.
struct Entry {
    /// 0-based line index of the `msgstr` keyword line
    msgstr_line: usize,
    /// 0-based line index of the `#, fuzzy` flag line, if present
    fuzzy_line: Option<usize>,
    /// Decoded msgid text (po string escapes resolved, concatenated)
    msgid: String,
    /// Decoded msgstr text
    msgstr: String,
}

/// Decode a run of po quoted-string lines into a plain Rust String.
/// Each line looks like `"some text\n"` — strip outer quotes, unescape.
fn decode_po_string(lines: &[String]) -> String {
    let mut out = String::new();
    for line in lines {
        let inner = line.trim();
        if inner.starts_with('"') && inner.ends_with('"') && inner.len() >= 2 {
            let s = &inner[1..inner.len() - 1];
            let mut chars = s.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '\\' {
                    match chars.next() {
                        Some('n') => out.push('\n'),
                        Some('t') => out.push('\t'),
                        Some('\\') => out.push('\\'),
                        Some('"') => out.push('"'),
                        Some(other) => { out.push('\\'); out.push(other); }
                        None => out.push('\\'),
                    }
                } else {
                    out.push(c);
                }
            }
        }
    }
    out
}

/// Detect a prompt-leak response and attempt to recover the real translation.
///
/// When a model leaks its instructions it translates them into the target language and
/// appends the actual translation at the end (separated by a blank line). The leak is
/// structural: the response is far longer than any plausible translation of `source`.
/// Returns `Some(recovered)` when a leak is detected, `None` when the response looks clean.
fn recover_from_leak(source: &str, response: &str) -> Option<String> {
    let leak_threshold = source.len().saturating_mul(4).max(120);
    if response.len() <= leak_threshold {
        return None;
    }
    // The real translation is always the last non-empty paragraph.
    let candidate = response.trim().rsplit("\n\n").find(|s| !s.trim().is_empty())?;
    let candidate = candidate.trim().to_string();
    // Sanity: recovered part must itself not look like another leak
    if candidate.len() > leak_threshold {
        return None;
    }
    Some(candidate)
}

/// Replace invalid JSON escape sequences (e.g. `\[`, `\(`) with their literal characters.
fn sanitize_json_escapes(s: &str) -> String {
    let valid = ['\"', '\\', '/', 'b', 'f', 'n', 'r', 't', 'u'];
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some(&next) if valid.contains(&next) => {
                    out.push(c);
                }
                Some(_) => {
                    // Invalid escape — drop the backslash, keep the character
                }
                None => { out.push(c); }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Encode a plain string into a single-line po `msgstr "..."` value.
fn encode_po_string(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '"'  => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out
}

fn commit_entry(
    entries: &mut Vec<Entry>,
    fuzzy_line: Option<usize>,
    msgstr_line_idx: Option<usize>,
    msgid_lines: &[String],
    msgstr_lines: &[String],
) {
    let Some(ms_line) = msgstr_line_idx else { return };
    let msgid = decode_po_string(msgid_lines);
    let msgstr = decode_po_string(msgstr_lines);
    if msgid.is_empty() {
        return; // header entry
    }
    entries.push(Entry { msgstr_line: ms_line, fuzzy_line, msgid, msgstr });
}

fn parse_po(lines: &[String]) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut fuzzy_line: Option<usize> = None;
    let mut in_msgid = false;
    let mut in_msgstr = false;
    let mut msgid_lines: Vec<String> = Vec::new();
    let mut msgstr_lines: Vec<String> = Vec::new();
    let mut msgstr_line_idx: Option<usize> = None;

    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim_end();

        if trimmed.starts_with("#,") && trimmed.contains("fuzzy") {
            commit_entry(&mut entries, fuzzy_line, msgstr_line_idx, &msgid_lines, &msgstr_lines);
            fuzzy_line = Some(idx);
            in_msgid = false;
            in_msgstr = false;
            msgid_lines.clear();
            msgstr_lines.clear();
            msgstr_line_idx = None;
            continue;
        }

        if trimmed.starts_with("msgid ") {
            if msgstr_line_idx.is_some() {
                commit_entry(&mut entries, fuzzy_line, msgstr_line_idx, &msgid_lines, &msgstr_lines);
                fuzzy_line = None;
                msgid_lines.clear();
                msgstr_lines.clear();
                msgstr_line_idx = None;
            }
            in_msgid = true;
            in_msgstr = false;
            msgid_lines.clear();
            msgid_lines.push(trimmed[6..].to_string());
            continue;
        }

        if trimmed.starts_with("msgstr ") {
            in_msgid = false;
            in_msgstr = true;
            msgstr_lines.clear();
            msgstr_line_idx = Some(idx);
            msgstr_lines.push(trimmed[7..].to_string());
            continue;
        }

        if trimmed.starts_with('"') {
            if in_msgid  { msgid_lines.push(trimmed.to_string()); }
            if in_msgstr { msgstr_lines.push(trimmed.to_string()); }
            continue;
        }

        if trimmed.is_empty() || trimmed.starts_with('#') {
            in_msgid = false;
            in_msgstr = false;
        }
    }
    commit_entry(&mut entries, fuzzy_line, msgstr_line_idx, &msgid_lines, &msgstr_lines);
    entries
}

fn write_po(
    lines: &[String],
    raw: &str,
    translations: &HashMap<usize, String>,
    translated_entries: &[&Entry],
    to_accept: &[&Entry],
    path: &std::path::Path,
) -> anyhow::Result<()> {
    // Remove fuzzy flags for entries we translated + entries accepted as-is
    let fuzzy_lines_to_remove: std::collections::HashSet<usize> = translated_entries
        .iter()
        .filter(|e| e.fuzzy_line.is_some() && translations.contains_key(&e.msgstr_line))
        .chain(to_accept.iter())
        .filter_map(|e| e.fuzzy_line)
        .collect();

    let mut output_lines: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        if fuzzy_lines_to_remove.contains(&i) {
            i += 1;
            continue;
        }
        if let Some(translated) = translations.get(&i) {
            output_lines.push(format!("msgstr \"{}\"", encode_po_string(translated)));
            i += 1;
            while i < lines.len() && lines[i].trim_start().starts_with('"') {
                i += 1;
            }
            continue;
        }
        output_lines.push(lines[i].clone());
        i += 1;
    }

    let mut out = output_lines.join("\n");
    if raw.ends_with('\n') {
        out.push('\n');
    }
    std::fs::write(path, out)?;
    Ok(())
}

async fn translate_batch(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    locale: &str,
    batch: &[&str],
) -> anyhow::Result<Vec<String>> {
    // Single-entry: skip JSON entirely, use raw response as the translation
    if batch.len() == 1 {
        let prompt = format!(
            "Translate the following English documentation string to {locale}.\n\
             - Return ONLY the translated string, nothing else.\n\
             - Do NOT translate: proper nouns, brand names (e.g. ZeroClaw, Anthropic, GitHub), command names, or code literals.\n\
             - Preserve exactly: backticks, bold (**text**), inline code, URLs, and escape sequences (\\n, \\t, etc.).\n\
             - If the string is already in {locale} or is a code literal, return it unchanged.\n\n\
             {}",
            batch[0]
        );
        let body = serde_json::json!({
            "model": provider.model,
            "messages": [{"role": "user", "content": prompt}],
            "reasoning_effort": "none"
        });
        let mut req = client.post(format!("{}/v1/chat/completions", provider.base_url)).json(&body);
        if let Some(key) = &provider.api_key {
            req = req.header("Authorization", format!("Bearer {key}"));
        }
        let resp = req.send().await?.error_for_status()?.json::<serde_json::Value>().await?;
        let text = resp["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("no content in response: {resp}"))?
            .trim()
            .to_string();
        let text = if let Some(recovered) = recover_from_leak(batch[0], &text) {
            eprintln!("  warning: prompt leak detected, recovered translation from response tail");
            recovered
        } else {
            text
        };
        return Ok(vec![text]);
    }

    let items: String = batch
        .iter()
        .map(|s| format!("- {s}"))
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = format!(
        "Translate the following English documentation strings to {locale}.\n\
         Rules:\n\
         - Return ONLY a JSON array with exactly {count} strings, in the same order as the input.\n\
         - Do NOT translate: proper nouns, brand names (e.g. ZeroClaw, Anthropic, GitHub), command names, code literals, or strings that are already in {locale}.\n\
         - Preserve exactly: backticks, bold (**text**), inline code, URLs, escape sequences (\\n, \\t, etc.), and leading/trailing whitespace.\n\
         - Do NOT add item numbers, bullet points, or any prefix to translated strings.\n\
         - Do NOT wrap output in markdown code fences.\n\n\
         Strings to translate:\n\
         {items}",
        count = batch.len()
    );

    let body = serde_json::json!({
        "model": provider.model,
        "messages": [{"role": "user", "content": prompt}],
        "reasoning_effort": "none"
    });

    let mut req = client
        .post(format!("{}/v1/chat/completions", provider.base_url))
        .json(&body);
    if let Some(key) = &provider.api_key {
        req = req.header("Authorization", format!("Bearer {key}"));
    }

    let resp = req.send().await?.error_for_status()?.json::<serde_json::Value>().await?;

    let text = resp["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("no content in response: {resp}"))?;

    // Extract the JSON array — find first '[' and last ']'
    let start = text.find('[').ok_or_else(|| anyhow::anyhow!("no JSON array in response"))?;
    let end   = text.rfind(']').ok_or_else(|| anyhow::anyhow!("no closing ] in response"))?;
    let raw_json = &text[start..=end];
    let arr: Vec<String> = serde_json::from_str(raw_json)
        .or_else(|_| serde_json::from_str(&sanitize_json_escapes(raw_json)))?;
    Ok(arr)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let provider = read_provider_config(&args.provider)?;

    let raw = std::fs::read_to_string(&args.po)?;
    let lines: Vec<String> = raw.lines().map(str::to_owned).collect();

    let entries = parse_po(&lines);

    let mut translations: HashMap<usize, String> = HashMap::new();

    // Repair entries where msgid ends with \n but msgstr doesn't — corrupted by
    // interrupted runs. Pre-populate into translations so write_po fixes them inline.
    let mut repaired = 0;
    for entry in &entries {
        if !entry.msgstr.is_empty()
            && entry.msgid.ends_with('\n')
            && !entry.msgstr.ends_with('\n')
        {
            translations.insert(entry.msgstr_line, format!("{}\n", entry.msgstr));
            repaired += 1;
        }
    }
    if repaired > 0 {
        println!("==> Repairing {repaired} entries missing trailing \\n");
    }

    // Repair entries where the model previously leaked its instructions instead of translating.
    // Use the same length-ratio heuristic as the live detection: recover the real translation
    // from the response tail when possible, otherwise clear to "" for re-translation.
    let mut leak_cleared = 0;
    let mut lines: Vec<String> = lines;
    for entry in &entries {
        if entry.msgstr.is_empty() { continue; }
        if let Some(recovered) = recover_from_leak(&entry.msgid, &entry.msgstr) {
            lines[entry.msgstr_line] = format!("msgstr \"{}\"", encode_po_string(&recovered));
            leak_cleared += 1;
        }
    }
    if leak_cleared > 0 {
        println!("==> Repaired {leak_cleared} prompt-leaked entries");
    }
    // Re-parse with repaired lines
    let entries = if leak_cleared > 0 { parse_po(&lines) } else { entries };

    // Entries with empty msgstr need AI translation.
    // Fuzzy entries already have a translation — accept it as-is, just drop the flag.
    // --force retranslates everything regardless.
    let to_translate: Vec<&Entry> = entries
        .iter()
        .filter(|e| args.force || e.msgstr.is_empty())
        .collect();

    let to_accept: Vec<&Entry> = entries
        .iter()
        .filter(|e| !args.force && e.fuzzy_line.is_some() && !e.msgstr.is_empty())
        .collect();

    if to_translate.is_empty() && to_accept.is_empty() && repaired == 0 {
        println!("Nothing to translate.");
        return Ok(());
    }

    println!(
        "==> {} to translate, {} fuzzy accepted as-is, provider={}, model={}",
        to_translate.len(),
        to_accept.len(),
        args.provider,
        provider.model,
    );

    let client = reqwest::Client::new();
    let total = to_translate.len();
    let total_chunks = total.div_ceil(args.batch).max(1);

    for (chunk_idx, chunk) in to_translate.chunks(args.batch).enumerate() {
        let msgids: Vec<&str> = chunk.iter().map(|e| e.msgid.as_str()).collect();
        println!("==> Chunk {}/{total_chunks} ({} entries)", chunk_idx + 1, chunk.len());

        match translate_batch(&client, &provider, &args.locale, &msgids).await {
            Ok(translated) => {
                for (entry, text) in chunk.iter().zip(translated.iter()) {
                    // If msgid ends with \n, msgstr must too — gettext requires it.
                    let text = if entry.msgid.ends_with('\n') && !text.ends_with('\n') {
                        format!("{text}\n")
                    } else {
                        text.clone()
                    };
                    translations.insert(entry.msgstr_line, text);
                }
                write_po(&lines, &raw, &translations, &to_translate, &to_accept, &args.po)?;
            }
            Err(e) => {
                eprintln!("  warning: chunk {} failed: {e}", chunk_idx + 1);
            }
        }
    }

    // Final write — handles to_accept fuzzy removals even when to_translate is empty
    write_po(&lines, &raw, &translations, &to_translate, &to_accept, &args.po)?;
    println!("==> Done: {}/{total} entries translated.", translations.len());
    Ok(())
}
