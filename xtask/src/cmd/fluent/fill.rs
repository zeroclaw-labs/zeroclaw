use crate::util::*;
use std::path::{Path, PathBuf};

const BATCH_SIZE: usize = 50;
const DEFAULT_MODEL: &str = "claude-haiku-4-5-20251001";

pub fn run(locale: Option<&str>, force: bool) -> anyhow::Result<()> {
    let root = repo_root();
    let locales_dir = fluent_locales_dir(&root);
    let en_dir = locales_dir.join("en");

    if !en_dir.exists() {
        anyhow::bail!("English locale dir not found: {}", en_dir.display());
    }

    let targets: Vec<&str> = match locale {
        Some(l) => vec![l],
        None => locales().iter().copied().filter(|&l| l != "en").collect(),
    };

    for target_locale in &targets {
        let target_dir = locales_dir.join(target_locale);
        std::fs::create_dir_all(&target_dir)?;

        for ftl_path in ftl_files_in(&en_dir)? {
            let filename = ftl_path.file_name().unwrap();
            let target_ftl = target_dir.join(filename);

            fill_ftl_file(&ftl_path, &target_ftl, target_locale, force)?;
        }
    }

    Ok(())
}

fn fill_ftl_file(
    en_path: &Path,
    target_path: &PathBuf,
    locale: &str,
    force: bool,
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

    let api_key = std::env::var("ANTHROPIC_API_TOKEN")
        .or_else(|_| std::env::var("ANTHROPIC_API_KEY"))
        .unwrap_or_default();
    if api_key.is_empty() {
        println!(
            "==> {locale}/{}: {} entries need translation (set ANTHROPIC_API_TOKEN to auto-fill)",
            en_path.file_name().unwrap().to_string_lossy(),
            to_translate.len()
        );
        return Ok(());
    }

    println!(
        "==> {locale}/{}: AI-filling {} entries",
        en_path.file_name().unwrap().to_string_lossy(),
        to_translate.len()
    );

    let model = std::env::var("FILL_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
    let locale_name = locale_display_name(locale);

    for chunk in to_translate.chunks(BATCH_SIZE) {
        let translated = call_api(&api_key, &model, locale_name, chunk)?;

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
    api_key: &str,
    model: &str,
    locale_name: &str,
    entries: &[(String, String)],
) -> anyhow::Result<Vec<(String, String)>> {
    let input_obj: serde_json::Map<String, serde_json::Value> = entries
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();

    let system = format!(
        "You are a translator. Translate the UI strings from English to {locale_name}. \
         Return ONLY a JSON object mapping each key to its translated string value. \
         Preserve any Fluent special syntax ({{\"{{\"}}...{{\"}}\"}}). \
         Do not translate proper nouns, technical identifiers, or code examples."
    );

    let body = serde_json::json!({
        "model": model,
        "max_tokens": 8192,
        "system": system,
        "messages": [
            {
                "role": "user",
                "content": serde_json::to_string(&input_obj)?
            }
        ]
    });

    let (auth_name, auth_value) = if api_key.starts_with("sk-ant-oat") {
        ("Authorization", format!("Bearer {api_key}"))
    } else {
        ("x-api-key", api_key.to_string())
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let (status, text) = rt.block_on(async {
        let client = reqwest::Client::new();
        let response = client
            .post("https://api.anthropic.com/v1/messages")
            .header(auth_name, auth_value)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        let text = response.text().await?;
        Ok::<_, reqwest::Error>((status, text))
    })?;

    if !status.is_success() {
        anyhow::bail!("Anthropic API error {status}: {text}");
    }

    let parsed: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("Failed to parse API response: {e}\n{text}"))?;

    let content = parsed["content"][0]["text"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("No text in API response: {text}"))?;

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
/// Comment lines and blank lines are ignored; the order of keys is preserved.
fn parse_ftl(src: &str) -> Vec<(String, String)> {
    let mut entries = vec![];
    for line in src.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('-') {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once(" = ") {
            entries.push((key.trim().to_string(), value.to_string()));
        }
    }
    entries
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

    // Write keys in en order
    for (key, _en_value) in en_entries {
        if let Some(translated) = locale_map.get(key.as_str()) {
            out.push_str(&format!("{key} = {translated}\n"));
        }
        // Keys not yet translated are omitted (runtime falls back to English)
    }

    // Append any locale-only keys not in en (shouldn't normally exist, but be safe)
    let en_set: std::collections::HashSet<&str> = en_entries.iter().map(|(k, _)| k.as_str()).collect();
    for (key, value) in locale_entries {
        if !en_set.contains(key.as_str()) {
            out.push_str(&format!("{key} = {value}\n"));
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
