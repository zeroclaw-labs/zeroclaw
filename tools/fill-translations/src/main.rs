use clap::Parser;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Parser)]
#[command(about = "Fill empty/fuzzy .po entries via a configured model_provider")]
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
    /// ModelProvider name from [providers.models.<name>] in config.toml
    #[arg(long)]
    model_provider: String,
    /// Path for appending full input/output on every failure (default: {po}.failures.log)
    #[arg(long)]
    log_failures: Option<PathBuf>,
}

/// Append-only logger for failed translation attempts — records the exact source string,
/// raw model response, and error so failure patterns can be inspected after the run.
struct FailureLog {
    file: Mutex<std::fs::File>,
}

impl FailureLog {
    fn open(path: &std::path::Path) -> anyhow::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }

    fn record(&self, chunk: usize, source: &str, response: &str, err: &anyhow::Error) {
        let mut f = self.file.lock().expect("failure log mutex poisoned");
        let _ = writeln!(
            f,
            "==== chunk {chunk} — {}\n-- error: {err}\n-- source: {source:?}\n-- response: {response:?}\n",
            chrono_now()
        );
    }
}

fn chrono_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("epoch={secs}")
}

struct ProviderConfig {
    base_url: String,
    model: String,
}

fn read_model_provider_config(provider_name: &str) -> anyhow::Result<ProviderConfig> {
    let home =
        std::env::var("HOME").unwrap_or_else(|_| std::env::var("USERPROFILE").unwrap_or_default());
    let candidates = [
        format!("{home}/.zeroclaw/config.toml"),
        format!("{home}/.config/zeroclaw/config.toml"),
    ];
    let raw = candidates
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .ok_or_else(|| anyhow::anyhow!("config.toml not found (tried ~/.zeroclaw/config.toml)"))?;
    let table: toml::Table = raw.parse()?;
    let model_provider = table
        .get("providers")
        .and_then(|v| v.get("models"))
        .and_then(|v| v.get(provider_name))
        .ok_or_else(|| {
            anyhow::anyhow!("[providers.models.{provider_name}] not found in config.toml")
        })?;
    let model = model_provider.get("model").and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("No model set for model_provider '{provider_name}' — add `model = \"...\"` to [providers.models.{provider_name}]"))?
        .to_string();
    Ok(ProviderConfig {
        base_url: model_provider
            .get("base_url")
            .and_then(|v| v.as_str())
            .unwrap_or("http://localhost:11434")
            .to_string(),
        model,
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
                        Some(other) => {
                            out.push('\\');
                            out.push(other);
                        }
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

/// Outcome of checking a model response against its source string.
enum LeakCheck {
    Clean,
    Recovered(String),
    Unrecoverable,
}

/// Detect a prompt-leak response and attempt to recover the real translation.
///
/// When a model leaks its instructions it translates them into the target language and
/// often appends the actual translation at the end. The leak is structural: the response
/// is far longer than any plausible translation of `source`, or starts with a bullet list.
fn check_for_leak(source: &str, response: &str) -> LeakCheck {
    let leak_threshold = source.len().saturating_mul(4).max(120);
    let looks_like_bullets = response.trim_start().starts_with("- ") && response.contains("\\n- ");
    let too_long = response.len() > leak_threshold;
    if !too_long && !looks_like_bullets {
        return LeakCheck::Clean;
    }
    // Try to recover: prefer the last paragraph after a blank line, else everything
    // after the final terminal punctuation ('. ' or '.').
    let candidate = response
        .trim()
        .rsplit("\n\n")
        .find(|s| !s.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            response
                .trim()
                .rsplit(". ")
                .next()
                .map(|s| s.trim_end_matches('.').trim().to_string())
        });
    match candidate {
        Some(c) if !c.is_empty() && c.len() <= leak_threshold => LeakCheck::Recovered(c),
        _ => LeakCheck::Unrecoverable,
    }
}

/// Encode a plain string into a single-line po `msgstr "..."` value.
fn encode_po_string(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
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
    let Some(ms_line) = msgstr_line_idx else {
        return;
    };
    let msgid = decode_po_string(msgid_lines);
    let msgstr = decode_po_string(msgstr_lines);
    if msgid.is_empty() {
        return; // header entry
    }
    entries.push(Entry {
        msgstr_line: ms_line,
        fuzzy_line,
        msgid,
        msgstr,
    });
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
            commit_entry(
                &mut entries,
                fuzzy_line,
                msgstr_line_idx,
                &msgid_lines,
                &msgstr_lines,
            );
            fuzzy_line = Some(idx);
            in_msgid = false;
            in_msgstr = false;
            msgid_lines.clear();
            msgstr_lines.clear();
            msgstr_line_idx = None;
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("msgid ") {
            if msgstr_line_idx.is_some() {
                commit_entry(
                    &mut entries,
                    fuzzy_line,
                    msgstr_line_idx,
                    &msgid_lines,
                    &msgstr_lines,
                );
                fuzzy_line = None;
                msgid_lines.clear();
                msgstr_lines.clear();
                msgstr_line_idx = None;
            }
            in_msgid = true;
            in_msgstr = false;
            msgid_lines.clear();
            msgid_lines.push(rest.to_string());
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("msgstr ") {
            in_msgid = false;
            in_msgstr = true;
            msgstr_lines.clear();
            msgstr_line_idx = Some(idx);
            msgstr_lines.push(rest.to_string());
            continue;
        }

        if trimmed.starts_with('"') {
            if in_msgid {
                msgid_lines.push(trimmed.to_string());
            }
            if in_msgstr {
                msgstr_lines.push(trimmed.to_string());
            }
            continue;
        }

        if trimmed.is_empty() || trimmed.starts_with('#') {
            in_msgid = false;
            in_msgstr = false;
        }
    }
    commit_entry(
        &mut entries,
        fuzzy_line,
        msgstr_line_idx,
        &msgid_lines,
        &msgstr_lines,
    );
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

/// Strip wrapping characters the model added that weren't present in the source.
///
/// Handles the common failure modes observed in logs: a whole translation wrapped in
/// backticks, corner brackets (`「」`/`『』`), straight or curly quotes, or the JSON field
/// leak `t="..."`. Applies each rule only when the wrapper is symmetric AND absent from
/// the source, so legitimate source-side wrapping is preserved.
/// Outcome of a translate_batch call. On failure, `raw_response` carries the full model
/// response (empty if the failure was before we got one — e.g. network) for logging.
struct BatchFailure {
    err: anyhow::Error,
    raw_response: String,
}

type BatchResult = Result<Vec<String>, BatchFailure>;

fn fail(err: anyhow::Error, raw_response: impl Into<String>) -> BatchFailure {
    BatchFailure {
        err,
        raw_response: raw_response.into(),
    }
}

/// Call Ollama's native `/api/chat` endpoint with a JSON schema constraining the output.
/// Ollama enforces the schema at generation time, so the model cannot emit anything but the
/// exact shape we request — no wrapping characters, no JSON leaks, no prose.
async fn translate_batch(
    client: &reqwest::Client,
    model_provider: &ProviderConfig,
    locale: &str,
    batch: &[&str],
) -> BatchResult {
    let system = format!(
        "You translate English technical documentation strings to {locale}.\n\
         - Preserve backticks, bold (**text**), inline code, URLs, and escape sequences where \
           they appear in the source, character-for-character.\n\
         - Do not translate: brand and project names, command names, CLI flags, file paths, \
           environment variables, code literals, function/type names.\n\
         - If the input is already in {locale}, a code literal, a URL, or a single identifier, \
           return it unchanged.\n\
         - Use established software-localization terminology in {locale} rather than literal \
           morpheme-by-morpheme translation."
    );

    // Send each source as its own user message; the model responds with one translation per
    // request as plain text in `message.content`. Ollama's schema enforcement is unreliable
    // in practice (varies by model and version), so we ask for plain text and trust the prompt.
    let mut out = Vec::with_capacity(batch.len());
    for source in batch {
        let body = serde_json::json!({
            "model": model_provider.model,
            "messages": [
                {"role": "system", "content": &system},
                {"role": "user", "content": *source},
            ],
            "stream": false,
            // Disable reasoning/thinking — field name differs by Ollama endpoint and version, so
            // include every variant we've seen. Unknown fields are silently ignored.
            "think": false,
            "reasoning_effort": "none",
            "options": {"temperature": 0},
        });
        let content = fetch_ollama_content(client, model_provider, &body).await?;
        out.push(content.trim().to_string());
    }
    Ok(out)
}

/// POST to Ollama's native `/api/chat` and return `message.content` from the response.
async fn fetch_ollama_content(
    client: &reqwest::Client,
    model_provider: &ProviderConfig,
    body: &serde_json::Value,
) -> Result<String, BatchFailure> {
    let resp = client
        .post(format!("{}/api/chat", model_provider.base_url))
        .json(body)
        .send()
        .await
        .map_err(|e| fail(e.into(), String::new()))?;
    let status = resp.status();
    let raw = resp
        .text()
        .await
        .map_err(|e| fail(e.into(), String::new()))?;
    if !status.is_success() {
        return Err(fail(anyhow::anyhow!("HTTP {status}"), raw));
    }
    let parsed: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
        fail(
            anyhow::anyhow!("response body JSON parse: {e}"),
            raw.clone(),
        )
    })?;
    parsed["message"]["content"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| fail(anyhow::anyhow!("no message.content"), raw))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    if args.po.extension().and_then(|e| e.to_str()) != Some("po") {
        anyhow::bail!("--po path must have a .po extension: {}", args.po.display());
    }
    if !args.po.exists() {
        anyhow::bail!("--po path does not exist: {}", args.po.display());
    }

    let model_provider = read_model_provider_config(&args.model_provider)?;

    let raw = std::fs::read_to_string(&args.po)?;
    let lines: Vec<String> = raw.lines().map(str::to_owned).collect();

    let entries = parse_po(&lines);

    let mut translations: HashMap<usize, String> = HashMap::new();

    // Repair entries where msgid ends with \n but msgstr doesn't — corrupted by
    // interrupted runs. Pre-populate into translations so write_po fixes them inline.
    let mut repaired = 0;
    for entry in &entries {
        if !entry.msgstr.is_empty() && entry.msgid.ends_with('\n') && !entry.msgstr.ends_with('\n')
        {
            translations.insert(entry.msgstr_line, format!("{}\n", entry.msgstr));
            repaired += 1;
        }
    }
    if repaired > 0 {
        println!("==> Repairing {repaired} entries missing trailing \\n");
    }

    // Repair entries where the model previously leaked its instructions instead of translating.
    // Recover the real translation from the response tail when possible, otherwise clear to ""
    // so the entry gets re-translated on this run.
    let mut leak_recovered = 0;
    let mut leak_blanked = 0;
    let mut lines: Vec<String> = lines;
    for entry in &entries {
        if entry.msgstr.is_empty() {
            continue;
        }
        match check_for_leak(&entry.msgid, &entry.msgstr) {
            LeakCheck::Clean => {}
            LeakCheck::Recovered(r) => {
                lines[entry.msgstr_line] = format!("msgstr \"{}\"", encode_po_string(&r));
                leak_recovered += 1;
            }
            LeakCheck::Unrecoverable => {
                lines[entry.msgstr_line] = "msgstr \"\"".to_string();
                leak_blanked += 1;
            }
        }
    }
    if leak_recovered + leak_blanked > 0 {
        println!(
            "==> Leak repair: {leak_recovered} recovered, {leak_blanked} cleared for re-translation"
        );
    }
    // Re-parse with repaired lines
    let entries = if leak_recovered + leak_blanked > 0 {
        parse_po(&lines)
    } else {
        entries
    };

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
        "==> {} to translate, {} fuzzy accepted as-is, model_provider={}, model={}",
        to_translate.len(),
        to_accept.len(),
        args.model_provider,
        model_provider.model,
    );

    let client = reqwest::Client::new();
    let total = to_translate.len();
    let total_chunks = total.div_ceil(args.batch).max(1);

    let log_path = args
        .log_failures
        .clone()
        .unwrap_or_else(|| args.po.with_extension("failures.log"));
    let failure_log = FailureLog::open(&log_path)?;
    println!("==> Logging failures to {}", log_path.display());

    for (chunk_idx, chunk) in to_translate.chunks(args.batch).enumerate() {
        let msgids: Vec<&str> = chunk.iter().map(|e| e.msgid.as_str()).collect();
        println!(
            "==> Chunk {}/{total_chunks} ({} entries)",
            chunk_idx + 1,
            chunk.len()
        );

        match translate_batch(&client, &model_provider, &args.locale, &msgids).await {
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
                write_po(
                    &lines,
                    &raw,
                    &translations,
                    &to_translate,
                    &to_accept,
                    &args.po,
                )?;
            }
            Err(f) => {
                let source_joined = msgids.join(" | ");
                eprintln!(
                    "  warning: chunk {} failed: {}\n    source: {:?}\n    response: {:?}",
                    chunk_idx + 1,
                    f.err,
                    source_joined,
                    f.raw_response
                );
                failure_log.record(chunk_idx + 1, &source_joined, &f.raw_response, &f.err);
            }
        }
    }

    // Final write — handles to_accept fuzzy removals even when to_translate is empty
    write_po(
        &lines,
        &raw,
        &translations,
        &to_translate,
        &to_accept,
        &args.po,
    )?;
    println!(
        "==> Done: {}/{total} entries translated.",
        translations.len()
    );
    Ok(())
}
