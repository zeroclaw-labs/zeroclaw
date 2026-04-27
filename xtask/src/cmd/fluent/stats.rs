use crate::util::*;

pub fn run() -> anyhow::Result<()> {
    let root = repo_root();
    let locales_dir = fluent_locales_dir(&root);

    if !locales_dir.exists() {
        anyhow::bail!("locales dir not found: {}", locales_dir.display());
    }

    let en_dir = locales_dir.join("en");
    if !en_dir.exists() {
        anyhow::bail!("English locale dir not found: {}", en_dir.display());
    }

    // Collect total key count from en FTL files
    let en_keys = collect_keys(&en_dir)?;
    let total = en_keys.len();

    println!("{:<10} {:>6} {:>6}  coverage", "locale", "keys", "total");
    println!("{}", "-".repeat(36));

    // en is always 100%
    println!("{:<10} {:>6} {:>6}  {:.1}%", "en", total, total, 100.0f64);

    let mut locales = fluent_locales(&root)?;
    locales.retain(|l| l != "en");

    for locale in &locales {
        let locale_dir = locales_dir.join(locale);
        let locale_keys = collect_keys(&locale_dir).unwrap_or_default();
        let present = locale_keys
            .iter()
            .filter(|k| en_keys.contains(k.as_str()))
            .count();
        let pct = if total == 0 {
            100.0
        } else {
            present as f64 / total as f64 * 100.0
        };
        println!("{:<10} {:>6} {:>6}  {:.1}%", locale, present, total, pct);
    }

    Ok(())
}

fn collect_keys(locale_dir: &std::path::Path) -> anyhow::Result<std::collections::HashSet<String>> {
    let mut keys = std::collections::HashSet::new();
    if !locale_dir.exists() {
        return Ok(keys);
    }
    for ftl_path in ftl_files_in(locale_dir)? {
        let src = std::fs::read_to_string(&ftl_path)?;
        for line in src.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('-') {
                continue;
            }
            if let Some(key) = trimmed.split(" = ").next() {
                let key = key.trim().to_string();
                if !key.is_empty() {
                    keys.insert(key);
                }
            }
        }
    }
    Ok(keys)
}
