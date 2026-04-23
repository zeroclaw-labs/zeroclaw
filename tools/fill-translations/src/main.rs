use clap::Parser;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "Fill empty/fuzzy .po entries via Anthropic API")]
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
    /// Model override (env FILL_MODEL also works)
    #[arg(long)]
    model: Option<String>,
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

async fn translate_batch(
    client: &reqwest::Client,
    api_key: &str,
    model: &str,
    locale: &str,
    batch: &[&str],
) -> anyhow::Result<Vec<String>> {
    let numbered: String = batch
        .iter()
        .enumerate()
        .map(|(i, s)| format!("{}. {}", i + 1, s))
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = format!(
        "Translate these English documentation strings to locale '{locale}'.\n\
         Return ONLY a JSON array of translated strings in the same order.\n\
         No explanation. Preserve backticks, bold (**), and code spans exactly.\n\
         If a string is already in the target language or is a code literal, return it unchanged.\n\n\
         {numbered}"
    );

    let body = serde_json::json!({
        "model": model,
        "max_tokens": 8192,
        "messages": [{"role": "user", "content": prompt}]
    });

    let resp = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json::<serde_json::Value>()
        .await?;

    let text = resp["content"][0]["text"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("no text in response"))?;

    // Extract the JSON array — find first '[' and last ']'
    let start = text.find('[').ok_or_else(|| anyhow::anyhow!("no JSON array in response"))?;
    let end   = text.rfind(']').ok_or_else(|| anyhow::anyhow!("no closing ] in response"))?;
    let arr: Vec<String> = serde_json::from_str(&text[start..=end])?;
    Ok(arr)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;

    let model = args
        .model
        .or_else(|| std::env::var("FILL_MODEL").ok())
        .unwrap_or_else(|| "claude-haiku-4-5-20251001".to_string());

    let raw = std::fs::read_to_string(&args.po)?;
    let lines: Vec<String> = raw.lines().map(str::to_owned).collect();

    let entries = parse_po(&lines);

    let to_translate: Vec<&Entry> = entries
        .iter()
        .filter(|e| args.force || e.msgstr.is_empty() || e.fuzzy_line.is_some())
        .collect();

    if to_translate.is_empty() {
        println!("Nothing to translate.");
        return Ok(());
    }

    let total = to_translate.len();
    let total_chunks = total.div_ceil(args.batch);
    println!("==> {total} entries to translate, batch={}, model={model}", args.batch);

    let client = reqwest::Client::new();

    // Map from msgstr line index -> translated text
    let mut translations: HashMap<usize, String> = HashMap::new();

    for (chunk_idx, chunk) in to_translate.chunks(args.batch).enumerate() {
        let msgids: Vec<&str> = chunk.iter().map(|e| e.msgid.as_str()).collect();
        println!("==> Chunk {}/{total_chunks} ({} entries)", chunk_idx + 1, chunk.len());

        match translate_batch(&client, &api_key, &model, &args.locale, &msgids).await {
            Ok(translated) => {
                for (entry, text) in chunk.iter().zip(translated.iter()) {
                    translations.insert(entry.msgstr_line, text.clone());
                }
            }
            Err(e) => {
                eprintln!("  warning: chunk {} failed: {e}", chunk_idx + 1);
            }
        }
    }

    // Collect fuzzy lines to remove (only for entries we successfully translated)
    let fuzzy_lines_to_remove: std::collections::HashSet<usize> = to_translate
        .iter()
        .filter(|e| e.fuzzy_line.is_some() && translations.contains_key(&e.msgstr_line))
        .filter_map(|e| e.fuzzy_line)
        .collect();

    let mut output_lines: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        // Skip old fuzzy flags for translated entries
        if fuzzy_lines_to_remove.contains(&i) {
            i += 1;
            continue;
        }

        if let Some(translated) = translations.get(&i) {
            output_lines.push(format!("msgstr \"{}\"", encode_po_string(translated)));
            i += 1;
            // Skip any continuation lines that were part of the old msgstr
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
    std::fs::write(&args.po, out)?;

    println!("==> Done: {}/{total} entries translated.", translations.len());
    Ok(())
}
