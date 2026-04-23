use clap::Parser;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Parser)]
struct Args {
    /// Path to the locale .ftl file to fill
    #[arg(long)]
    ftl: PathBuf,

    /// Path to the English source .ftl file
    #[arg(long)]
    en: PathBuf,

    /// Locale code (e.g. "ja", "fr")
    #[arg(long)]
    locale: String,

    /// Re-translate all entries, not just missing ones
    #[arg(long)]
    force: bool,

    /// Override model (default: claude-haiku-4-5-20251001)
    #[arg(long)]
    model: Option<String>,
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

    let en_entries = parse_ftl(&std::fs::read_to_string(&args.en)?);
    let mut locale_entries: Vec<(String, String)> = if args.ftl.exists() {
        parse_ftl(&std::fs::read_to_string(&args.ftl)?)
    } else {
        vec![]
    };

    let existing: std::collections::HashSet<String> =
        locale_entries.iter().map(|(k, _)| k.clone()).collect();

    let to_translate: Vec<(String, String)> = en_entries
        .iter()
        .filter(|(key, _)| args.force || !existing.contains(key))
        .cloned()
        .collect();

    if to_translate.is_empty() {
        println!("up to date");
        return Ok(());
    }

    let client = reqwest::Client::new();

    for chunk in to_translate.chunks(50) {
        let translated = translate_batch(&client, &api_key, &model, &args.locale, chunk).await?;

        for (key, value) in translated {
            if let Some(entry) = locale_entries.iter_mut().find(|(k, _)| k == &key) {
                entry.1 = value;
            } else {
                locale_entries.push((key, value));
            }
        }

        write_ftl(&args.ftl, &en_entries, &locale_entries)?;
        println!("wrote {} entries", locale_entries.len());
    }

    Ok(())
}

async fn translate_batch(
    client: &reqwest::Client,
    api_key: &str,
    model: &str,
    locale: &str,
    entries: &[(String, String)],
) -> anyhow::Result<Vec<(String, String)>> {
    let input_obj: serde_json::Map<String, serde_json::Value> = entries
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();

    let locale_name = locale_display_name(locale);
    let system = format!(
        "You are a translator. Translate the UI strings from English to {locale_name}. \
         Return ONLY a JSON object mapping each key to its translated string value. \
         Preserve any Fluent special syntax ({{\"{{\"}}...{{\"}}\"}}). \
         Do not translate proper nouns, technical identifiers, or code examples."
    );

    let body = serde_json::json!({
        "model": model,
        "max_tokens": 4096,
        "system": system,
        "messages": [{"role": "user", "content": serde_json::to_string(&input_obj)?}]
    });

    let resp = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&body)
        .send()
        .await?
        .error_for_status()
        .map_err(|e| anyhow::anyhow!("API error: {e}"))?
        .json::<serde_json::Value>()
        .await?;

    let text = resp["content"][0]["text"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("no text in response"))?;

    let json_str = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    let obj: HashMap<String, String> = serde_json::from_str(json_str)
        .map_err(|e| anyhow::anyhow!("failed to parse translation JSON: {e}\n{json_str}"))?;

    Ok(obj.into_iter().collect())
}

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

fn write_ftl(
    path: &std::path::Path,
    en_entries: &[(String, String)],
    locale_entries: &[(String, String)],
) -> anyhow::Result<()> {
    let locale_map: HashMap<&str, &str> =
        locale_entries.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();

    let mut out = String::new();
    for (key, _) in en_entries {
        if let Some(translated) = locale_map.get(key.as_str()) {
            out.push_str(&format!("{key} = {translated}\n"));
        }
    }

    let en_set: std::collections::HashSet<&str> =
        en_entries.iter().map(|(k, _)| k.as_str()).collect();
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
