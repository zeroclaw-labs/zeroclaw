use crate::util::*;
use std::path::Path;

pub fn run() -> anyhow::Result<()> {
    let root = repo_root();
    let locales_dir = fluent_locales_dir(&root);

    if !locales_dir.exists() {
        anyhow::bail!("locales dir not found: {}", locales_dir.display());
    }

    let mut error_count = 0;

    for locale in fluent_locales(&root)? {
        let locale_dir = locales_dir.join(&locale);
        for ftl_path in ftl_files_in(&locale_dir)? {
            let errors = check_ftl_file(&ftl_path)?;
            if errors.is_empty() {
                println!("ok  {}", ftl_path.display());
            } else {
                for (line_no, msg) in &errors {
                    eprintln!("{}:{}: {}", ftl_path.display(), line_no, msg);
                    error_count += 1;
                }
            }
        }
    }

    if error_count > 0 {
        anyhow::bail!("{error_count} FTL syntax error(s) found");
    }
    Ok(())
}

fn check_ftl_file(path: &Path) -> anyhow::Result<Vec<(usize, String)>> {
    let src = std::fs::read_to_string(path)?;
    let mut errors = vec![];

    for (i, line) in src.lines().enumerate() {
        let line_no = i + 1;
        let trimmed = line.trim();

        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Key lines must match: identifier = value
        if let Some((key, value)) = trimmed.split_once(" = ") {
            // Key must be alphanumeric + hyphens only
            if !key.chars().all(|c| c.is_alphanumeric() || c == '-') {
                errors.push((line_no, format!("invalid key '{key}'")));
            }
            // Check for unescaped braces in value (Fluent requires {"{"}...{"}"})
            check_unescaped_braces(value, line_no, &mut errors);
        } else if !trimmed.starts_with('-') {
            errors.push((
                line_no,
                format!("malformed line (expected 'key = value'): {trimmed}"),
            ));
        }
    }

    Ok(errors)
}

fn check_unescaped_braces(value: &str, line_no: usize, errors: &mut Vec<(usize, String)>) {
    let chars: Vec<char> = value.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '{' => {
                // Check if it's a Fluent placeable: {"{"} or {some-term} etc.
                // If the next char is '"' it's an escaped literal — skip to closing }
                if i + 1 < chars.len() && chars[i + 1] == '"' {
                    // find closing "}
                    if let Some(close) = chars[i + 2..].iter().position(|&c| c == '"') {
                        i += 2 + close + 1; // skip past closing "
                        // skip the closing }
                        if i < chars.len() && chars[i] == '}' {
                            i += 1;
                        }
                    } else {
                        errors.push((line_no, "unclosed Fluent escape {\"...\"".to_string()));
                        i += 1;
                    }
                } else {
                    // Any other { is treated as an unescaped brace — Fluent placements
                    // are valid (e.g. {$var}, {-term}) but naked { without . or - or $
                    // followed by non-identifier chars is suspicious. For our simple
                    // tools.ftl which uses only escaped literals, flag bare {.
                    let peek = chars.get(i + 1).copied();
                    if !matches!(peek, Some('$' | '-' | '.')) {
                        errors.push((
                            line_no,
                            format!(
                                "unescaped '{{' at col {i}; use {{\"{{\"}} for a literal brace"
                            ),
                        ));
                    }
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
}
