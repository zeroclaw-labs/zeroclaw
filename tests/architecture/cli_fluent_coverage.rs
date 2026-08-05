//! Architecture gate: user-facing strings must route through Fluent, not ship
//! as bare literals. A PR that adds an un-localized user-facing string fails
//! this test (and therefore CI), so it can never land.

use std::fs;
use std::path::Path;

#[test]
fn user_facing_strings_route_through_fluent() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
    let mut violations: Vec<String> = Vec::new();
    scan_dir(&root.join("src"), &mut violations);
    assert!(
        violations.is_empty(),
        "Bare user-facing string literals detected. User-facing text must come \
         from Fluent (a `cli-*` key via `zeroclaw_runtime::i18n`), not a literal: \
         clap `about`/`long_about`/`help = \"...\"` and `println!`/`eprintln!`/\
         `print!`/`eprint!` with a literal format string. Wrap the text in a \
         Fluent lookup, or exempt a deliberate line with `// i18n-exempt: <reason>`.\n\n\
         Violations:\n{}",
        violations.join("\n")
    );
}

fn scan_dir(dir: &Path, violations: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_dir(&path, violations);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let Ok(src) = fs::read_to_string(&path) else {
            continue;
        };
        let display = path.display().to_string();
        let src_lines: Vec<&str> = src.lines().collect();
        for (idx, raw) in src_lines.iter().enumerate() {
            let line = raw.trim_start();
            // Exemption may be on the flagged line itself, or — for multi-line
            // attribute/string literals where a trailing comment would corrupt
            // the string — on the immediately preceding line.
            if line.contains("// i18n-exempt:") {
                continue;
            }
            if idx > 0 && src_lines[idx - 1].contains("// i18n-exempt:") {
                continue;
            }
            if is_hardcoded_help(line) || is_bare_print_literal(line) {
                violations.push(format!("  {}:{}: {}", display, idx + 1, line.trim()));
            }
        }
    }
}

/// `clap` help attribute literal: `about`/`long_about`/`help = "..."`.
/// `= None` is not a literal and is allowed.
fn is_hardcoded_help(line: &str) -> bool {
    line.contains("about = \"") || line.contains("help = \"")
}

fn is_bare_print_literal(line: &str) -> bool {
    const MACROS: &[&str] = &["println!(", "print!(", "eprintln!(", "eprint!("];
    for m in MACROS {
        if let Some(pos) = line.find(m) {
            let after = &line[pos + m.len()..];
            let arg = after.trim_start();
            // First non-space char after `(` is a quote → bare string literal
            // (covers `"..."` and raw `r"..."` / `r#"..."#`).
            if arg.starts_with('"') || arg.starts_with("r\"") || arg.starts_with("r#") {
                // A literal with no letters (pure separator like "\n" or "----")
                // is not localizable prose; allow it.
                if literal_has_letters(arg) {
                    return true;
                }
            }
        }
    }
    false
}

/// Does the leading string literal on this fragment contain any alphabetic
/// character *outside* `{...}` format placeholders? Pure punctuation/escape
/// separators (`"\n"`, `"==="`, `" — "`) and placeholder-only strings
/// (`"{error:#}"`, `"{} — {}"`) do not — they carry no translatable prose.
fn literal_has_letters(arg: &str) -> bool {
    // Locate the opening quote (after an optional raw-string `r`/`r#…#` prefix).
    let mut chars = arg.chars().peekable();
    if chars.peek() == Some(&'r') {
        chars.next();
        while chars.peek() == Some(&'#') {
            chars.next();
        }
    }
    if chars.next() != Some('"') {
        return false;
    }
    let mut escaped = false;
    let mut brace_depth = 0usize;
    for c in chars {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            break;
        } else if c == '{' {
            brace_depth += 1;
        } else if c == '}' {
            brace_depth = brace_depth.saturating_sub(1);
        } else if brace_depth == 0 && c.is_alphabetic() {
            return true;
        }
    }
    false
}
