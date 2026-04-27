use crate::util::*;
use std::collections::HashSet;
use std::process::Command;

pub fn run() -> anyhow::Result<()> {
    let root = repo_root();
    let locales_dir = fluent_locales_dir(&root);
    let en_dir = locales_dir.join("en");

    if !en_dir.exists() {
        anyhow::bail!("English locale dir not found: {}", en_dir.display());
    }

    // Collect all keys from en FTL files
    let mut en_keys: HashSet<String> = HashSet::new();
    for ftl_path in ftl_files_in(&en_dir)? {
        let src = std::fs::read_to_string(&ftl_path)?;
        for line in src.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('-') {
                continue;
            }
            if let Some(key) = trimmed.split(" = ").next() {
                let key = key.trim().to_string();
                if !key.is_empty() {
                    en_keys.insert(key);
                }
            }
        }
    }

    println!("==> {} keys in en FTL files", en_keys.len());

    // Check which keys are referenced in Rust source
    let mut stale: Vec<String> = vec![];
    let mut referenced: Vec<String> = vec![];

    for key in &en_keys {
        if is_referenced_in_source(&root, key) {
            referenced.push(key.clone());
        } else {
            stale.push(key.clone());
        }
    }

    stale.sort();
    referenced.sort();

    if stale.is_empty() {
        println!("==> All keys referenced in Rust source");
    } else {
        println!("\nStale keys (in en.ftl but not referenced in Rust source):");
        for key in &stale {
            println!("  - {key}");
        }
    }

    // Find tool names in Rust source that have no en.ftl key
    let source_tool_names = extract_tool_names_from_source(&root);
    let mut missing: Vec<String> = vec![];
    for name in &source_tool_names {
        let key = format!("tool-{}", name.replace('_', "-"));
        if !en_keys.contains(&key) {
            missing.push(key);
        }
    }
    missing.sort();
    missing.dedup();

    if !missing.is_empty() {
        println!("\nMissing keys (tool names in source with no en.ftl entry):");
        for key in &missing {
            println!("  + {key}");
        }
    }

    if stale.is_empty() && missing.is_empty() {
        println!("==> en.ftl is in sync with source");
    }

    Ok(())
}

fn is_referenced_in_source(root: &std::path::Path, key: &str) -> bool {
    let output = Command::new("grep")
        .args(["-r", "--include=*.rs", "-l", "-F", key])
        .arg(root.join("crates"))
        .arg(root.join("src"))
        .output();
    match output {
        Ok(out) => !out.stdout.is_empty(),
        Err(_) => true, // grep unavailable — assume referenced to avoid false positives
    }
}

fn extract_tool_names_from_source(root: &std::path::Path) -> Vec<String> {
    let output = Command::new("grep")
        .args([
            "-r",
            "--include=*.rs",
            "-h",
            "-o",
            r#"fn name.*"[a-z][a-z0-9_]*""#,
        ])
        .arg(root.join("crates"))
        .output();

    let mut names = vec![];
    if let Ok(out) = output {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            // Extract the string literal from fn name return
            if let Some(start) = line.rfind('"')
                && let Some(end) = line[..start].rfind('"')
            {
                let name = &line[end + 1..start];
                if !name.is_empty() {
                    names.push(name.to_string());
                }
            }
        }
    }
    names
}
