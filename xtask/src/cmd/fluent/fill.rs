use crate::util::*;
use std::path::{Path, PathBuf};

const BATCH_SIZE: usize = 50;

struct Backend {
    base_url: String,
    model: String,
    api_key: Option<String>,
}

fn resolve_backend(provider: &str) -> anyhow::Result<Backend> {
    let cfg = read_provider_config(provider)?;
    let model = cfg.model
        .ok_or_else(|| anyhow::anyhow!("No model set for provider '{provider}' — add `model = \"...\"` to [providers.models.{provider}] in config.toml"))?;
    Ok(Backend { base_url: cfg.base_url, model, api_key: cfg.api_key })
}

pub fn run(locale: Option<&str>, force: bool, provider: Option<&str>) -> anyhow::Result<()> {
    let root = repo_root();
    let locales_dir = fluent_locales_dir(&root);
    let en_dir = locales_dir.join("en");

    if !en_dir.exists() {
        anyhow::bail!("English locale dir not found: {}", en_dir.display());
    }

    let targets: Vec<String> = match locale {
        Some(l) => vec![l.to_string()],
        None => locales().into_iter().filter(|l| l != "en").collect(),
    };

    let provider_name = provider.ok_or_else(|| anyhow::anyhow!("--provider <name> is required (configured in [providers.models.<name>] in config.toml)"))?;
    let backend = resolve_backend(provider_name)?;

    for target_locale in &targets {
        let target_dir = locales_dir.join(target_locale);
        std::fs::create_dir_all(&target_dir)?;

        for ftl_path in ftl_files_in(&en_dir)? {
            let filename = ftl_path.file_name().unwrap();
            let target_ftl = target_dir.join(filename);

            fill_ftl_file(&ftl_path, &target_ftl, target_locale, force, &backend)?;
        }
    }

    Ok(())
}

fn fill_ftl_file(
    en_path: &Path,
    target_path: &PathBuf,
    locale: &str,
    force: bool,
    backend: &Backend,
) -> anyhow::Result<()> {
    let en_entries = parse_ftl(&std::fs::read_to_string(en_path)?);
    let mut target_entries: Vec<(String, String)> = if target_path.exists() {
        parse_ftl(&std::fs::read_to_string(target_path)?)
    } else {
        vec![]
    };

    let existing_keys: std::collections::HashSet<String> =
        target_entries.iter().map(|(k, _)| k.clone()).collect();

    let to_translate: Vec<(String, String)> = en_entries
        .iter()
        .filter(|(key, _)| force || !existing_keys.contains(key))
        .cloned()
        .collect();

    if to_translate.is_empty() {
        println!("==> {locale}/{}: up to date, skipping AI step", en_path.file_name().unwrap().to_string_lossy());
        return Ok(());
    }

    println!(
        "==> {locale}/{}: AI-filling {} entries",
        en_path.file_name().unwrap().to_string_lossy(),
        to_translate.len()
    );

    let locale_name = locale_display_name(locale);

    for chunk in to_translate.chunks(BATCH_SIZE) {
        let translated = call_api(backend, locale_name, chunk)?;

        // Merge: update existing or append new
        for (key, value) in translated {
            if let Some(entry) = target_entries.iter_mut().find(|(k, _)| k == &key) {
                entry.1 = value;
            } else {
                target_entries.push((key, value));
            }
        }

        // Write after each batch for incremental safety
        write_ftl(target_path, &en_entries, &target_entries)?;
        println!("    wrote {}", target_path.display());
    }

    Ok(())
}

fn call_api(
    backend: &Backend,
    locale_name: &str,
    entries: &[(String, String)],
) -> anyhow::Result<Vec<(String, String)>> {
    let input_obj: serde_json::Map<String, serde_json::Value> = entries
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();

    let system = format!(
        "You are a translator. Translate UI strings from English to {locale_name}.\n\
         Rules:\n\
         - Return ONLY a JSON object mapping each key to its translated string value.\n\
         - Do NOT translate: proper nouns, brand names (e.g. ZeroClaw, Anthropic), command names, \
           technical identifiers, or code examples.\n\
         - Preserve exactly: Fluent placeholders ({{ variable }}), special syntax, and escape sequences.\n\
         - Do NOT wrap output in markdown code fences."
    );

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let (status, text) = rt.block_on(async {
        let client = reqwest::Client::new();
        let user_content = serde_json::to_string(&input_obj)?;

        let body = serde_json::json!({
            "model": backend.model,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user_content}
            ],
            "reasoning_effort": "none"
        });
        let mut req = client.post(format!("{}/v1/chat/completions", backend.base_url))
            .json(&body);
        if let Some(key) = &backend.api_key {
            req = req.header("Authorization", format!("Bearer {key}"));
        }
        let response = req.send().await?;

        let status = response.status();
        let text = response.text().await?;
        Ok::<_, anyhow::Error>((status, text))
    })?;

    if !status.is_success() {
        anyhow::bail!("API error {status}: {text}");
    }

    let parsed: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("Failed to parse API response: {e}\n{text}"))?;

    let content = parsed["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("No content in response: {text}"))?;

    // Strip markdown code fences if present
    let json_str = content
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    let translations: serde_json::Value = serde_json::from_str(json_str)
        .map_err(|e| anyhow::anyhow!("Failed to parse translation JSON: {e}\n{json_str}"))?;

    let obj = translations
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Expected JSON object in translation response"))?;

    let mut result = vec![];
    for (key, value) in obj {
        if let Some(translated) = value.as_str() {
            result.push((key.clone(), translated.to_string()));
        }
    }

    Ok(result)
}

/// Parse an FTL file into an ordered list of (key, value) pairs.
/// Handles both single-line (`key = value`) and multi-line (indented continuation) FTL values.
fn parse_ftl(src: &str) -> Vec<(String, String)> {
    let mut entries: Vec<(String, String)> = vec![];
    let mut current_key: Option<String> = None;
    let mut current_lines: Vec<String> = vec![];

    for line in src.lines() {
        // Continuation line of a multi-line value (4-space or tab indent)
        if (line.starts_with("    ") || line.starts_with('\t')) && current_key.is_some() {
            current_lines.push(line.trim().to_string());
            continue;
        }

        let trimmed = line.trim();

        // Blank line: part of multi-line value if inside one, otherwise ignored
        if trimmed.is_empty() {
            if current_key.is_some() {
                current_lines.push(String::new());
            }
            continue;
        }

        // Comment or term: flush pending entry and skip
        if trimmed.starts_with('#') || trimmed.starts_with('-') {
            flush_entry(&mut entries, &mut current_key, &mut current_lines);
            continue;
        }

        // New `key = value` or `key =` (multi-line) line
        let parsed_kv = if let Some((k, v)) = trimmed.split_once(" = ") {
            Some((k.trim(), v.trim()))
        } else if let Some(k) = trimmed.strip_suffix(" =") {
            Some((k.trim(), ""))
        } else {
            None
        };
        if let Some((key, value)) = parsed_kv {
            flush_entry(&mut entries, &mut current_key, &mut current_lines);
            current_key = Some(key.to_string());
            if !value.is_empty() {
                current_lines.push(value.to_string());
            }
        }
    }

    flush_entry(&mut entries, &mut current_key, &mut current_lines);
    entries
}

fn flush_entry(entries: &mut Vec<(String, String)>, key: &mut Option<String>, lines: &mut Vec<String>) {
    if let Some(k) = key.take() {
        while lines.last().map_or(false, |l| l.is_empty()) {
            lines.pop();
        }
        let value = lines.join("\n");
        if !value.is_empty() {
            entries.push((k, value));
        }
        lines.clear();
    }
}

fn write_ftl_entry(out: &mut String, key: &str, value: &str) {
    if value.contains('\n') {
        out.push_str(&format!("{key} =\n"));
        for line in value.lines() {
            if line.is_empty() {
                out.push('\n');
            } else {
                out.push_str(&format!("    {line}\n"));
            }
        }
    } else {
        out.push_str(&format!("{key} = {value}\n"));
    }
}

/// Write a locale FTL file, using en_entries to preserve key order and comments.
/// Keys not in en_entries (locale-only additions) are appended at the end.
fn write_ftl(
    path: &Path,
    en_entries: &[(String, String)],
    locale_entries: &[(String, String)],
) -> anyhow::Result<()> {
    let locale_map: std::collections::HashMap<&str, &str> = locale_entries
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let mut out = String::new();

    for (key, _en_value) in en_entries {
        if let Some(translated) = locale_map.get(key.as_str()) {
            write_ftl_entry(&mut out, key, translated);
        }
        // Keys not yet translated are omitted (runtime falls back to English)
    }

    // Append any locale-only keys not in en (shouldn't normally exist, but be safe)
    let en_set: std::collections::HashSet<&str> = en_entries.iter().map(|(k, _)| k.as_str()).collect();
    for (key, value) in locale_entries {
        if !en_set.contains(key.as_str()) {
            write_ftl_entry(&mut out, key, value);
        }
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, out)?;
    Ok(())
}

fn locale_display_name(locale: &str) -> &str {
    match locale {
        "ja" | "ja-JP" => "Japanese",
        "zh" | "zh-CN" => "Simplified Chinese",
        "zh-TW" => "Traditional Chinese",
        "ko" | "ko-KR" => "Korean",
        "fr" | "fr-FR" => "French",
        "de" | "de-DE" => "German",
        "es" | "es-ES" => "Spanish",
        "pt" | "pt-BR" => "Brazilian Portuguese",
        "ru" | "ru-RU" => "Russian",
        "ar" => "Arabic",
        "hi" | "hi-IN" => "Hindi",
        "it" | "it-IT" => "Italian",
        "nl" | "nl-NL" => "Dutch",
        "pl" | "pl-PL" => "Polish",
        "sv" | "sv-SE" => "Swedish",
        "tr" | "tr-TR" => "Turkish",
        "vi" | "vi-VN" => "Vietnamese",
        other => other,
    }
}
