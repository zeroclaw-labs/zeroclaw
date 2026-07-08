use clap::Parser;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use xtask::cmd::mdbook::check::introduced_local_absolute_path;
use xtask::cmd::mdbook::protected::{contains_generated_toml_block, preservation_prompt};

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
    /// ModelProvider alias from `[providers.models.<kind>.<alias>]` in config.toml
    #[arg(long)]
    model_provider: String,
    /// Config directory holding config.toml and .secret-key (default:
    /// ~/.zeroclaw). Mirrors `zeroclaw --config-dir`.
    #[arg(long)]
    config_dir: Option<String>,
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
    IntroducedLocalPath(String),
}

/// Detect prompt leaks and local-path leaks before writing a translation.
///
/// When a model leaks its instructions it translates them into the target language and
/// often appends the actual translation at the end. The leak is structural: the response
/// is far longer than any plausible translation of `source`, or starts with a bullet list.
fn check_for_leak(source: &str, response: &str) -> LeakCheck {
    if contains_generated_toml_block(source, response) {
        return LeakCheck::Unrecoverable;
    }

    let leak_threshold = source.len().saturating_mul(4).max(120);
    let looks_like_bullets = response.trim_start().starts_with("- ")
        && (response.contains("\n- ") || response.contains("\\n- "));
    let too_long = response.len() > leak_threshold;
    if !too_long && !looks_like_bullets {
        return check_for_local_path_leak(source, response, LeakCheck::Clean);
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
        Some(c) if !c.is_empty() && c.len() <= leak_threshold => {
            match introduced_local_absolute_path(source, &c) {
                Some(path) => LeakCheck::IntroducedLocalPath(path),
                None => LeakCheck::Recovered(c),
            }
        }
        _ => LeakCheck::Unrecoverable,
    }
}

fn check_for_local_path_leak(source: &str, response: &str, clean: LeakCheck) -> LeakCheck {
    introduced_local_absolute_path(source, response)
        .map(LeakCheck::IntroducedLocalPath)
        .unwrap_or(clean)
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

fn replace_msgstr_line(lines: &mut Vec<String>, msgstr_line: usize, value: &str) {
    lines[msgstr_line] = format!("msgstr \"{}\"", encode_po_string(value));
    let i = msgstr_line + 1;
    while i < lines.len() && lines[i].trim_start().starts_with('"') {
        lines.remove(i);
    }
}

fn normalize_translated_text(msgid: &str, text: String) -> String {
    if !text.is_empty() && msgid.ends_with('\n') && !text.ends_with('\n') {
        format!("{text}\n")
    } else {
        text
    }
}

fn translation_for_write(source: &str, response: &str) -> String {
    let text = match check_for_leak(source, response) {
        LeakCheck::Clean => response.to_string(),
        LeakCheck::Recovered(recovered) => recovered,
        LeakCheck::Unrecoverable => String::new(),
        LeakCheck::IntroducedLocalPath(_) => String::new(),
    };
    normalize_translated_text(source, text)
}

/// Repair entries whose `msgstr` is a prompt-leak or local-path-leak response.
///
/// Iterates in reverse so that removing continuation lines doesn't shift
/// line indices for entries yet to visit. Returns
/// `(recovered, prompt_blanked, path_blanked, first_path_blanked)`.
fn repair_leaks(
    lines: &mut Vec<String>,
    entries: &[Entry],
) -> (usize, usize, usize, Option<String>) {
    let mut leak_recovered = 0;
    let mut leak_blanked = 0;
    let mut path_blanked = 0;
    let mut first_path_blanked = None;
    for entry in entries.iter().rev() {
        if entry.msgstr.is_empty() {
            continue;
        }
        match check_for_leak(&entry.msgid, &entry.msgstr) {
            LeakCheck::Clean => {}
            LeakCheck::Recovered(r) => {
                replace_msgstr_line(lines, entry.msgstr_line, &r);
                leak_recovered += 1;
            }
            LeakCheck::Unrecoverable => {
                replace_msgstr_line(lines, entry.msgstr_line, "");
                leak_blanked += 1;
            }
            LeakCheck::IntroducedLocalPath(path) => {
                replace_msgstr_line(lines, entry.msgstr_line, "");
                path_blanked += 1;
                first_path_blanked.get_or_insert(path);
            }
        }
    }
    (
        leak_recovered,
        leak_blanked,
        path_blanked,
        first_path_blanked,
    )
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

/// Translate each source string via the shared runtime provider. The provider
/// stack handles endpoint, auth, and wire protocol per family — this tool
/// builds no HTTP. One request per source string keeps the per-entry mapping
/// unambiguous (the .po model is one msgid -> one msgstr).
async fn translate_batch(
    provider: &dyn zeroclaw_api::model_provider::ModelProvider,
    model: &str,
    locale: &str,
    batch: &[&str],
) -> BatchResult {
    let system = format!(
        "You translate English technical documentation strings to {locale}.\n\
         - Preserve backticks, bold (**text**), inline code, URLs, and escape sequences where \
           they appear in the source, character-for-character.\n\
         - Do not translate: brand and project names, command names, CLI flags, file paths, \
           environment variables, code literals, function/type names.\n\
         - Do not add examples, configuration snippets, TOML blocks, code fences, or extra \
           explanatory text that is not present in the source.\n\
         - Do not invent, localize, or substitute machine-local absolute paths such as \
           /Users/..., /home/..., /private/tmp/..., /Volumes/..., or C:\\Users\\...; only \
           preserve paths already present in the source.\n\
         - If the input is already in {locale}, a code literal, a URL, or a single identifier, \
           return it unchanged.\n\
         - Use established software-localization terminology in {locale} rather than literal \
           morpheme-by-morpheme translation.\n\
         - Return ONLY the translated string, no quotes, no preamble, no explanation."
    );

    let mut out = Vec::with_capacity(batch.len());
    for source in batch {
        let scoped_system;
        let system_ref = if let Some(prompt) = preservation_prompt(source) {
            scoped_system = format!("{system}\n{prompt}");
            scoped_system.as_str()
        } else {
            system.as_str()
        };
        let content = zeroclaw_providers::ProviderDispatch::from_ref(provider)
            .chat_with_system(Some(system_ref), source, model, None)
            .await
            .map_err(|e| fail(e, String::new()))?;
        out.push(content.trim().to_string());
    }
    Ok(out)
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

    let (provider, model) =
        xtask::util::build_model_provider(&args.model_provider, args.config_dir.as_deref())?;

    let raw = std::fs::read_to_string(&args.po)?;
    let mut lines: Vec<String> = raw.lines().map(str::to_owned).collect();

    let mut entries = parse_po(&lines);

    // Repair entries where the model previously leaked its instructions instead of translating.
    // Recover the real translation from the response tail when possible, otherwise clear to ""
    // so the entry gets re-translated on this run.
    // NB: On master the trailing-\n pre-pass runs AFTER this loop, so there is no stale
    // translations key to remove — the order-of-operations refactoring already fixes #8312
    // at the production-code level. The regression test in the test module pins this
    // invariant (see repair_leaks_drops_stale_translations_key).
    let (leak_recovered, leak_blanked, path_blanked, first_path_blanked) =
        repair_leaks(&mut lines, &entries);
    if leak_recovered + leak_blanked + path_blanked > 0 {
        println!(
            "==> Translation repair: {leak_recovered} prompt leaks recovered, {leak_blanked} prompt leaks cleared, {path_blanked} path leaks cleared for re-translation"
        );
        if let Some(path) = first_path_blanked {
            println!("==> First path leak cleared: {path}");
        }
    }
    if leak_recovered + leak_blanked + path_blanked > 0 {
        entries = parse_po(&lines);
    }

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
        model,
    );

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

        match translate_batch(provider.as_ref(), &model, &args.locale, &msgids).await {
            Ok(translated) => {
                for (entry, text) in chunk.iter().zip(translated.iter()) {
                    let text = translation_for_write(&entry.msgid, text);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_msgstr_line_removes_multiline_continuations() {
        let mut lines = vec![
            "msgid \"source\"".to_string(),
            "msgstr \"old\"".to_string(),
            "\" continuation\"".to_string(),
            String::new(),
            "msgid \"next\"".to_string(),
            "msgstr \"kept\"".to_string(),
        ];

        replace_msgstr_line(&mut lines, 1, "");

        assert_eq!(
            lines,
            vec![
                "msgid \"source\"",
                "msgstr \"\"",
                "",
                "msgid \"next\"",
                "msgstr \"kept\"",
            ]
        );
    }

    #[test]
    fn rejects_translation_that_introduces_local_absolute_path() {
        assert!(matches!(
            check_for_leak(
                "The failure log is next to the catalog.",
                "Le journal est dans /private/tmp/zeroclaw/fr.failures.log.",
            ),
            LeakCheck::IntroducedLocalPath(_)
        ));
        assert_eq!(
            translation_for_write(
                "The failure log is next to the catalog.",
                "Le journal est dans /private/tmp/zeroclaw/fr.failures.log.",
            ),
            ""
        );
    }

    #[test]
    fn empty_unrecoverable_translation_stays_empty_for_newline_source() {
        assert_eq!(normalize_translated_text("source\n", String::new()), "");
    }

    #[test]
    fn allows_translation_that_preserves_source_path() {
        assert!(matches!(
            check_for_leak(
                "Write `/home/alice/zeroclaw/web/dist`.",
                "Écrivez `/home/alice/zeroclaw/web/dist`.",
            ),
            LeakCheck::Clean
        ));
    }

    #[test]
    fn recovers_prompt_leak_before_checking_path_examples() {
        let leaked = "- Do not invent paths such as /Users/... or /private/tmp/...\n\
            - Preserve commands, paths, URLs, environment variables, code literals, and project names exactly as written.\n\n\
            Traduit.";
        assert!(matches!(
            check_for_leak("Translate me.", leaked),
            LeakCheck::Recovered(recovered) if recovered == "Traduit."
        ));
    }

    #[test]
    fn non_empty_translation_preserves_required_trailing_newline() {
        assert_eq!(
            normalize_translated_text("source\n", "translated".to_string()),
            "translated\n"
        );
        assert_eq!(translation_for_write("source\n", "traduit"), "traduit\n");
    }

    /// A leaked msgstr long enough to trip `check_for_leak`'s `too_long` guard
    /// (threshold = max(4*len(msgid), 120)), with the real translation (`保存`)
    /// as the final paragraph after a blank line so it is Recovered.
    /// The msgid ends with `\n` so the trailing-`\n` pre-pass in main() would
    /// seed `translations[msgstr_line]` — the precondition that fired #8312.
    const RECOVERABLE_LEAK_TRAILING_NEWLINE: &str = concat!(
        "msgid \"Save\\n\"\n",
        "msgstr \"\"\n",
        "\"- You translate English technical documentation strings to Japanese.\\n\"\n",
        "\"- Do not translate brand names, command names, CLI flags, or file paths.\\n\"\n",
        "\"\\n\"\n",
        "\"保存\"\n",
    );

    /// Regression for #8312: after leak repair + trailing-`\n` pre-pass,
    /// `write_po` must emit the recovered translation, not the leaked text.
    ///
    /// Uses the shared `repair_leaks` helper (so it tests the real production
    /// code path) and drives `write_po` end-to-end. Mutation check: reverting
    /// `repair_leaks` to a no-op (bypassing the loop) makes the test fail
    /// because the pre-pass seeds the unrecovered leaked text.
    #[test]
    fn repair_leaks_drops_stale_translations_key() {
        let raw = RECOVERABLE_LEAK_TRAILING_NEWLINE;
        let mut lines: Vec<String> = raw.lines().map(str::to_owned).collect();
        let mut entries = parse_po(&lines);
        assert_eq!(entries.len(), 1);
        let msgstr_line = entries[0].msgstr_line;

        // Blank continuation lines belonging to the leaked msgstr block so that
        // re-parse after repair doesn't re-append them (orthogonal to #8312).
        let mut ci = msgstr_line + 1;
        while ci < lines.len() && lines[ci].trim_start().starts_with('"') {
            lines[ci].clear();
            ci += 1;
        }

        // Step 1: Run the production leak-repair path via the shared helper.
        let (recovered, blanked, path_blanked, first_path_blanked) =
            repair_leaks(&mut lines, &entries);
        assert_eq!(
            (recovered, blanked, path_blanked, first_path_blanked),
            (1, 0, 0, None),
            "entry must be Recovered by leak repair"
        );

        // Re-parse lines after repair (same as main() does).
        if recovered + blanked > 0 {
            entries = parse_po(&lines);
        }
        assert!(
            !entries[0].msgstr.is_empty(),
            "msgstr must not be empty after recovery"
        );

        // Step 2: Simulate the trailing-`\n` pre-pass (same logic as main()).
        let mut translations: HashMap<usize, String> = HashMap::new();
        for entry in &entries {
            if !entry.msgstr.is_empty()
                && entry.msgid.ends_with('\n')
                && !entry.msgstr.ends_with('\n')
            {
                translations.insert(entry.msgstr_line, format!("{}\n", entry.msgstr));
            }
        }
        assert!(
            !translations.contains_key(&msgstr_line)
                || translations
                    .get(&msgstr_line)
                    .is_some_and(|v| v.contains("保存")),
            "if pre-pass seeded, it must contain the recovered translation, not leaked text"
        );

        // Step 3: write_po end-to-end — the final output must contain the
        // recovered translation and must NOT contain the leaked prompt text.
        let path = std::env::temp_dir().join(format!(
            "fill_tr_stale_key_{}_{}.po",
            std::process::id(),
            msgstr_line
        ));
        write_po(&lines, raw, &translations, &[], &[], &path).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert!(
            out.contains("msgstr \"保存"),
            "write_po must emit the recovered translation, got: {out}"
        );
        assert!(
            !out.contains("You translate English"),
            "stale leaked text must not be re-shipped, got: {out}"
        );
    }
}
