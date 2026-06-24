use crate::cmd::fluent::catalog::message_ids_from_file;
use crate::util::*;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

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
        for key in message_ids_from_file(&ftl_path)? {
            en_keys.insert(key);
        }
    }

    println!("==> {} keys in en FTL files", en_keys.len());

    let source_index = SourceIndex::build(&root)?;

    // Check which keys are referenced in Rust source
    let mut stale: Vec<String> = vec![];
    let mut dynamic_cli: Vec<String> = vec![];

    for key in &en_keys {
        if is_dynamic_cli_key(key) {
            dynamic_cli.push(key.clone());
        } else if source_index.references_key(key) {
            // Referenced by a static Rust literal or known tool-name convention.
        } else {
            stale.push(key.clone());
        }
    }

    stale.sort();
    dynamic_cli.sort();

    if stale.is_empty() {
        println!("==> All statically checked keys referenced in Rust source");
    } else {
        println!("\nStale keys (in en.ftl but not referenced in Rust source):");
        for key in &stale {
            println!("  - {key}");
        }
    }

    if !dynamic_cli.is_empty() {
        println!(
            "\nDynamic CLI keys (shape matches runtime-generated Clap keys; not stale-checked):"
        );
        for key in &dynamic_cli {
            println!("  ~ {key}");
        }
    }

    // Find tool names in Rust source that have no en.ftl key
    let source_tool_names = source_index.tool_names();
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
        println!("==> en.ftl has no static stale keys or missing tool keys");
    }

    Ok(())
}

fn is_dynamic_cli_key(key: &str) -> bool {
    key.starts_with("cli-") && (key.ends_with("-about") || key.ends_with("-long-about"))
}

#[derive(Debug)]
struct SourceIndex {
    files: Vec<SourceFile>,
}

#[derive(Debug)]
struct SourceFile {
    text: String,
}

impl SourceIndex {
    fn build(root: &Path) -> anyhow::Result<Self> {
        let mut paths = Vec::new();
        collect_rs_files(&root.join("crates"), &mut paths)?;
        collect_rs_files(&root.join("src"), &mut paths)?;

        let files = paths
            .into_iter()
            .map(|path| {
                let text = std::fs::read_to_string(&path)?;
                let text = strip_line_comments(&text);
                let text = strip_cfg_test_modules(&text);
                Ok(SourceFile { text })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        Ok(Self { files })
    }

    fn references_key(&self, key: &str) -> bool {
        if self.contains_literal(key) {
            return true;
        }

        if let Some(tool_name) = key.strip_prefix("tool-") {
            return self.contains_literal(&tool_name.replace('-', "_"));
        }

        false
    }

    fn contains_literal(&self, needle: &str) -> bool {
        self.files.iter().any(|file| file.text.contains(needle))
    }

    fn tool_names(&self) -> Vec<String> {
        let mut names = self
            .files
            .iter()
            .flat_map(|file| extract_tool_names_from_source(&file.text))
            .collect::<Vec<_>>();
        names.sort();
        names.dedup();
        names
    }
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_rs_files(&path, out)?;
        } else if path.extension().is_some_and(|extension| extension == "rs")
            && path.file_name().is_none_or(|name| name != "tests.rs")
        {
            out.push(path);
        }
    }
    Ok(())
}

fn strip_line_comments(src: &str) -> String {
    src.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn strip_cfg_test_modules(src: &str) -> String {
    let mut stripped = String::new();
    let mut cursor = 0;

    while let Some(cfg_offset) = src[cursor..].find("#[cfg(test)]") {
        let cfg_start = cursor + cfg_offset;
        let after_cfg = cfg_start + "#[cfg(test)]".len();
        let after_ws = skip_ws(src, after_cfg);

        if !src[after_ws..].starts_with("mod ") {
            stripped.push_str(&src[cursor..after_cfg]);
            cursor = after_cfg;
            continue;
        }

        let Some(block_start) = src[after_ws..].find('{').map(|offset| after_ws + offset) else {
            break;
        };
        let Some(block_end) = matching_brace(src, block_start) else {
            break;
        };

        stripped.push_str(&src[cursor..cfg_start]);
        cursor = block_end + 1;
    }

    stripped.push_str(&src[cursor..]);
    stripped
}

fn skip_ws(src: &str, start: usize) -> usize {
    src[start..]
        .char_indices()
        .find_map(|(offset, ch)| (!ch.is_whitespace()).then_some(start + offset))
        .unwrap_or(src.len())
}

fn extract_tool_names_from_source(src: &str) -> Vec<String> {
    let mut names = vec![];
    let mut search_from = 0;

    while let Some(impl_start) = src[search_from..].find("impl Tool for") {
        let impl_start = search_from + impl_start;
        let Some(block_start) = src[impl_start..]
            .find('{')
            .map(|offset| impl_start + offset)
        else {
            break;
        };
        let Some(block_end) = matching_brace(src, block_start) else {
            break;
        };
        let block = &src[block_start + 1..block_end];

        if let Some(name_start) = block.find("fn name")
            && let Some(name_body) = function_body(&block[name_start..])
            && let Some(name) = first_string_literal(name_body)
        {
            names.push(name);
        }

        search_from = block_end + 1;
    }

    names
}

fn function_body(src: &str) -> Option<&str> {
    let body_start = src.find('{')?;
    let body_end = matching_brace(src, body_start)?;
    Some(&src[body_start + 1..body_end])
}

fn matching_brace(src: &str, open_index: usize) -> Option<usize> {
    let mut depth = 0usize;
    let bytes = src.as_bytes();
    let mut index = open_index;

    while index < bytes.len() {
        if let Some(end) = raw_string_end(src, index) {
            index = end;
            continue;
        }

        match bytes[index] {
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index = bytes[index..]
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map(|offset| index + offset + 1)
                    .unwrap_or(bytes.len());
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = bytes[index + 2..]
                    .windows(2)
                    .position(|window| window == b"*/")
                    .map(|offset| index + 2 + offset + 2)
                    .unwrap_or(bytes.len());
            }
            b'"' => index = quoted_string_end(bytes, index, b'"'),
            b'{' => {
                depth += 1;
                index += 1;
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(index);
                }
                index += 1;
            }
            _ => index += 1,
        }
    }

    None
}

fn quoted_string_end(bytes: &[u8], start: usize, quote: u8) -> usize {
    let mut escaped = false;
    let mut index = start + 1;
    while index < bytes.len() {
        if escaped {
            escaped = false;
        } else if bytes[index] == b'\\' {
            escaped = true;
        } else if bytes[index] == quote {
            return index + 1;
        }
        index += 1;
    }
    bytes.len()
}

fn raw_string_end(src: &str, start: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    if bytes.get(start) != Some(&b'r') {
        return None;
    }

    let mut hashes = 0usize;
    let mut cursor = start + 1;
    while bytes.get(cursor) == Some(&b'#') {
        hashes += 1;
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'"') {
        return None;
    }

    let terminator = format!("\"{}", "#".repeat(hashes));
    src[cursor + 1..]
        .find(&terminator)
        .map(|offset| cursor + 1 + offset + terminator.len())
}

fn first_string_literal(src: &str) -> Option<String> {
    let mut chars = src.char_indices();
    while let Some((start, ch)) = chars.next() {
        if ch != '"' {
            continue;
        }
        let mut value = String::new();
        let mut escaped = false;
        for (_, ch) in chars.by_ref() {
            if escaped {
                value.push(ch);
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '"' => return Some(value),
                _ => value.push(ch),
            }
        }
        return Some(src[start + 1..].to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_brace_ignores_braces_inside_strings_and_lifetimes() {
        let src = r##"{
            let value: &'static str = "ok";
            let json = r#"{"value": true}"#;
            let text = "{not a block}";
        }"##;

        assert_eq!(matching_brace(src, 0), Some(src.len() - 1));
    }

    #[test]
    fn source_tool_name_extraction_skips_rust_doc_comments() {
        let root = tempfile::tempdir().unwrap();
        let crates_dir = root.path().join("crates/robot-kit/src");
        std::fs::create_dir_all(&crates_dir).unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(
            crates_dir.join("traits.rs"),
            r#"
/// impl Tool for BeepTool {
///     fn name(&self) -> &str { "beep" }
/// }
impl Tool for RealTool {
    fn name(&self) -> &str { "real_tool" }
}
"#,
        )
        .unwrap();

        let index = SourceIndex::build(root.path()).unwrap();
        assert_eq!(index.tool_names(), vec!["real_tool"]);
    }

    #[test]
    fn source_tool_name_extraction_skips_cfg_test_modules() {
        let src = r#"
#[cfg(test)]
mod tests {
    impl Tool for FakeTool {
        fn name(&self) -> &str {
            "fake"
        }
    }
}

impl Tool for RealTool {
    fn name(&self) -> &str {
        "real_tool"
    }
}
"#;

        let stripped = strip_cfg_test_modules(&strip_line_comments(src));
        assert_eq!(
            extract_tool_names_from_source(&stripped),
            vec!["real_tool".to_string()]
        );
    }

    #[test]
    fn source_reference_check_handles_tool_keys() {
        let root = tempfile::tempdir().unwrap();
        let crates_dir = root.path().join("crates/example/src");
        std::fs::create_dir_all(&crates_dir).unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(
            crates_dir.join("tool.rs"),
            r#"
impl Tool for FileRead {
    fn name(&self) -> &str {
        "file_read"
    }
}
"#,
        )
        .unwrap();

        let index = SourceIndex::build(root.path()).unwrap();
        assert!(index.references_key("tool-file-read"));
        assert!(!index.references_key("tool-missing-tool"));
    }

    #[test]
    fn source_tool_name_extraction_handles_multiline_tool_impls() {
        let src = r#"
impl Tool for CalculatorTool {
    fn name(&self) -> &str {
        "calculator"
    }

    fn description(&self) -> &str {
        "Perform calculations"
    }
}
"#;

        assert_eq!(
            extract_tool_names_from_source(src),
            vec!["calculator".to_string()]
        );
    }

    #[test]
    fn source_tool_name_extraction_ignores_nonliteral_name_bodies() {
        let src = r#"
impl Tool for DynamicTool {
    fn name(&self) -> &str {
        self.name.as_str()
    }

    fn description(&self) -> &str {
        "description is not the name"
    }
}
"#;

        assert!(extract_tool_names_from_source(src).is_empty());
    }

    #[test]
    fn dynamic_cli_key_detection_is_explicitly_classified() {
        assert!(is_dynamic_cli_key("cli-agent-long-about"));
        assert!(is_dynamic_cli_key("cli-agent-about"));
        assert!(!is_dynamic_cli_key("cli-wechat-connected"));
    }
}
