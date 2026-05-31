//! Architecture gate: user-facing strings must route through Fluent, not ship
//! as bare literals. A PR that adds an un-localized user-facing string fails
//! this test (and therefore CI), so it can never land.
//!
//! Two classes are caught in `src/` (the user-facing CLI surface):
//!   1. `clap` help literals — `about`/`long_about`/`help = "..."` render into
//!      `--help` output the user reads.
//!   2. Terminal output macros with a bare string literal as the format arg —
//!      `println!("Done.")`, `eprint!("error: ...")`, etc. A literal ships
//!      English in every locale; the text must come from
//!      `zeroclaw_runtime::i18n` (a `cli-*` Fluent key). `println!("{}", t(..))`
//!      and `println!()` are fine — the format arg is not a bare literal.
//!
//! Doc-comments are out of scope. To exempt a specific line deliberately (a
//! genuinely non-localized diagnostic, a build directive, etc.), add
//! `// i18n-exempt: <reason>` on it.

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
        for (idx, raw) in src.lines().enumerate() {
            let line = raw.trim_start();
            if line.contains("// i18n-exempt:") {
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

/// A print/output macro whose format argument is a bare string literal.
/// `println!("hi")` → true. `println!("{}", t("k"))` is also literal-first, so
/// it is still flagged — the format string itself is English text the user
/// sees, so it must be a Fluent key, not an inline literal with `{}` holes.
/// `println!(value)`, `println!()`, `writeln!(f, ...)` → not flagged here.
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
/// character? Pure punctuation/escape separators (`"\n"`, `"==="`) do not.
fn literal_has_letters(arg: &str) -> bool {
    // Take from the first quote to a reasonable end; cheap heuristic on the line.
    let bytes = arg.as_bytes();
    let mut i = 0;
    // skip optional raw-string prefix
    if arg.starts_with('r') {
        i = 1;
        while i < bytes.len() && bytes[i] == b'#' {
            i += 1;
        }
    }
    if i >= bytes.len() || bytes[i] != b'"' {
        return false;
    }
    i += 1;
    let mut escaped = false;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            break;
        } else if c.is_alphabetic() {
            return true;
        }
        i += 1;
    }
    false
}
