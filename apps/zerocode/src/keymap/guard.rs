//! Source guard: handlers must decide behavior through typed action
//! events, never by reading a raw key or constructing a `Chord` inline.

#[cfg(test)]
mod tests {
    use std::path::Path;

    const SRC: &str = env!("CARGO_MANIFEST_DIR");

    /// Files exempt from the guard: the keymap module owns the chord
    /// vocabulary and the registry, and the config persistence/preset
    /// layer constructs `Chord` values as data, not as handler logic.
    fn is_exempt(rel: &str) -> bool {
        rel.starts_with("src/keymap/") || rel.starts_with("src/config/") || rel == "src/build.rs"
    }

    /// The single sanctioned text-entry idiom: extract the literal char
    /// the user typed into a buffer, ignored when CONTROL is held. This
    /// is input, not a chord, and has no remappable action.
    fn is_text_entry(line: &str) -> bool {
        let l = line.trim();
        l.starts_with("if let KeyCode::Char(c) = key.code")
            || l.starts_with("KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL)")
            || l == "match key.code {"
    }

    const FORBIDDEN: &[&str] = &[
        "key.code ==",
        "match key.code",
        "KeyCode::",
        "Chord::key(",
        "Chord::char(",
        "Chord::ctrl(",
        "Chord::with(",
        "Chord::shift(",
        ".matches(",
    ];

    /// Lines a handler may keep only with an explicit waiver marker, so
    /// every escape is reviewed and cannot grow silently.
    const WAIVER: &str = "// keyguard: ";

    fn scan(dir: &Path, root: &Path, violations: &mut Vec<String>) {
        let entries = std::fs::read_dir(dir).expect("read_dir");
        for entry in entries {
            let entry = entry.expect("entry");
            let path = entry.path();
            if path.is_dir() {
                scan(&path, root, violations);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if is_exempt(&rel) {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read_to_string");
            let mut in_test = false;
            for (i, line) in text.lines().enumerate() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("#[cfg(test)]") || trimmed.starts_with("mod tests") {
                    in_test = true;
                }
                if in_test {
                    continue;
                }
                if is_text_entry(line) || line.contains(WAIVER) {
                    continue;
                }
                for tok in FORBIDDEN {
                    if line.contains(tok) {
                        violations.push(format!("{rel}:{}: {}", i + 1, line.trim()));
                        break;
                    }
                }
            }
        }
    }

    #[test]
    fn no_hardcoded_chords_in_handlers() {
        let root = Path::new(SRC);
        let src = root.join("src");
        let mut violations = Vec::new();
        scan(&src, root, &mut violations);
        assert!(
            violations.is_empty(),
            "hardcoded key/chord in handler source (route through an action enum, \
             or annotate a reviewed escape with `// keyguard: <reason>`):\n{}",
            violations.join("\n")
        );
    }
}
