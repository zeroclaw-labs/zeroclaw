//! Architecture gate: user-facing strings must route through their localization
//! boundary rather than ship as bare literals.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::str::FromStr;

use proc_macro2::{Delimiter, LineColumn, TokenStream, TokenTree};
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{Attribute, Expr, Lit, LitStr, Macro, Meta, Token};

const SCAN_ROOTS: &[&str] = &["src", "crates/zeroclaw-providers/src/auth"];
const LEGACY_ALLOWLIST: &str = include_str!("cli_fluent_legacy_allowlist.tsv");

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ViolationKey {
    path: String,
    kind: String,
    literal: String,
}

#[derive(Clone, Debug)]
struct Violation {
    key: ViolationKey,
    line: usize,
}

#[test]
fn user_facing_strings_route_through_fluent() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut violations = Vec::new();
    for relative_root in SCAN_ROOTS {
        scan_dir(root, &root.join(relative_root), &mut violations)
            .unwrap_or_else(|error| panic!("localization architecture scan failed: {error}"));
    }

    let problems = compare_with_legacy_baseline(&violations, LEGACY_ALLOWLIST);
    assert!(
        problems.is_empty(),
        "Bare user-facing string literal baseline changed. User-facing text must \
         come from the owning localization boundary, not a literal. Root CLI text \
         uses a `cli-*` Fluent key via `zeroclaw_runtime::i18n`; other presentation \
         boundaries must use their documented adapter. Wrap new text in that \
         boundary, or exempt a deliberate line with `// i18n-exempt: <reason>`. \
         Existing debt is count-sensitive: remove stale baseline \
         entries when debt is migrated, but do not add new entries.\n\nProblems:\n{}",
        problems.join("\n")
    );
}

fn scan_dir(root: &Path, dir: &Path, violations: &mut Vec<Violation>) -> Result<(), String> {
    let metadata = fs::symlink_metadata(dir)
        .map_err(|error| format!("could not inspect {}: {error}", dir.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "symlink used as localization scan root: {}",
            dir.display()
        ));
    }
    if !metadata.is_dir() {
        return Err(format!(
            "localization scan root is not a directory: {}",
            dir.display()
        ));
    }
    let entries =
        fs::read_dir(dir).map_err(|error| format!("could not read {}: {error}", dir.display()))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("could not read entry in {}: {error}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("could not classify {}: {error}", path.display()))?;
        if file_type.is_symlink() {
            return Err(format!(
                "symlink under localization scan root: {}",
                path.display()
            ));
        }
        if file_type.is_dir() {
            scan_dir(root, &path, violations)?;
            continue;
        }
        if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
            continue;
        }
        let source = fs::read_to_string(&path)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?;
        let relative_path = path
            .strip_prefix(root)
            .map_err(|error| format!("could not relativize {}: {error}", path.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        scan_source(&relative_path, &source, violations)?;
    }
    Ok(())
}

fn scan_source(path: &str, source: &str, violations: &mut Vec<Violation>) -> Result<(), String> {
    let file =
        syn::parse_file(source).map_err(|error| format!("could not parse {path}: {error}"))?;
    let mut detector = Detector {
        path,
        exemption_lines: exemption_comment_lines(source)?,
        violations,
        errors: Vec::new(),
    };
    detector.visit_file(&file);
    if detector.errors.is_empty() {
        Ok(())
    } else {
        Err(detector.errors.join("; "))
    }
}

struct Detector<'a> {
    path: &'a str,
    exemption_lines: ExemptionLines,
    violations: &'a mut Vec<Violation>,
    errors: Vec<String>,
}

impl Detector<'_> {
    fn record(
        &mut self,
        kind: &str,
        literal: &LitStr,
        target_start: LineColumn,
        target_end: LineColumn,
    ) {
        if is_exempt(&self.exemption_lines, target_start, target_end) {
            return;
        }
        let value = literal.value();
        if !literal_has_letters(&value) {
            return;
        }
        let literal = normalize_whitespace(&literal.token().to_string());
        self.violations.push(Violation {
            key: ViolationKey {
                path: self.path.to_string(),
                kind: kind.to_string(),
                literal: literal.clone(),
            },
            line: target_start.line,
        });
    }

    fn record_nested_prints(&mut self, tokens: TokenStream) {
        let trees = tokens.into_iter().collect::<Vec<_>>();
        let mut index = 0;
        while index < trees.len() {
            if let [
                TokenTree::Ident(identifier),
                TokenTree::Punct(punctuation),
                TokenTree::Group(group),
                ..,
            ] = &trees[index..]
                && punctuation.as_char() == '!'
                && let Some(kind) = print_macro_identifier(identifier)
                && let Some(literal) = first_string_literal_tokens(group.stream())
            {
                self.record(
                    kind,
                    &literal,
                    identifier.span().start(),
                    group.span_close().end(),
                );
                index += 3;
                continue;
            }
            if let [TokenTree::Punct(punctuation), TokenTree::Group(group), ..] = &trees[index..]
                && punctuation.as_char() == '#'
                && group.delimiter() == Delimiter::Bracket
                && nested_attribute_is_clap(group)
            {
                let attribute =
                    TokenStream::from_iter([trees[index].clone(), trees[index + 1].clone()]);
                match Attribute::parse_outer.parse2(attribute) {
                    Ok(attributes) => {
                        for attribute in attributes {
                            match clap_help_literals(&attribute) {
                                Ok(literals) => {
                                    for (kind, literal) in literals {
                                        self.record(
                                            kind,
                                            &literal,
                                            punctuation.span().start(),
                                            group.span_close().end(),
                                        );
                                    }
                                }
                                Err(error) => self.errors.push(format!(
                                    "could not inspect nested clap attribute in {}:{}: {error}",
                                    self.path,
                                    punctuation.span().start().line
                                )),
                            }
                        }
                    }
                    Err(error) => self.errors.push(format!(
                        "could not parse nested clap attribute in {}:{}: {error}",
                        self.path,
                        punctuation.span().start().line
                    )),
                }
                index += 2;
                continue;
            }
            index += 1;
        }
        for tree in trees {
            if let TokenTree::Group(group) = tree {
                self.record_nested_prints(group.stream());
            }
        }
    }
}

impl<'ast> Visit<'ast> for Detector<'_> {
    fn visit_macro(&mut self, node: &'ast Macro) {
        if let Some(kind) = print_macro_kind(node)
            && let Some(literal) = first_string_literal(node)
        {
            self.record(kind, &literal, node.span().start(), node.span().end());
        }
        self.record_nested_prints(node.tokens.clone());
        visit::visit_macro(self, node);
    }

    fn visit_attribute(&mut self, node: &'ast Attribute) {
        if is_clap_attribute(node) {
            match clap_help_literals(node) {
                Ok(literals) => {
                    for (kind, literal) in literals {
                        self.record(kind, &literal, node.span().start(), node.span().end());
                    }
                }
                Err(error) => self.errors.push(format!(
                    "could not inspect clap attribute in {}:{}: {error}",
                    self.path,
                    node.span().start().line
                )),
            }
        }
        visit::visit_attribute(self, node);
    }
}

fn print_macro_kind(node: &Macro) -> Option<&'static str> {
    let identifier = &node.path.segments.last()?.ident;
    print_macro_identifier(identifier)
}

fn print_macro_identifier(identifier: &proc_macro2::Ident) -> Option<&'static str> {
    if identifier == "println" {
        Some("println")
    } else if identifier == "print" {
        Some("print")
    } else if identifier == "eprintln" {
        Some("eprintln")
    } else if identifier == "eprint" {
        Some("eprint")
    } else {
        None
    }
}

fn first_string_literal(node: &Macro) -> Option<LitStr> {
    first_string_literal_tokens(node.tokens.clone())
}

fn first_string_literal_tokens(tokens: TokenStream) -> Option<LitStr> {
    let TokenTree::Literal(token) = tokens.into_iter().next()? else {
        return None;
    };
    syn::parse_str(&token.to_string()).ok()
}

fn is_clap_attribute(attribute: &Attribute) -> bool {
    attribute.path().segments.last().is_some_and(|segment| {
        matches!(
            segment.ident.to_string().as_str(),
            "arg" | "clap" | "command"
        )
    })
}

fn nested_attribute_is_clap(group: &proc_macro2::Group) -> bool {
    let Some(TokenTree::Ident(identifier)) = group.stream().into_iter().next() else {
        return false;
    };
    matches!(identifier.to_string().as_str(), "arg" | "clap" | "command")
}

fn clap_help_literals(attribute: &Attribute) -> syn::Result<Vec<(&'static str, LitStr)>> {
    let Meta::List(list) = &attribute.meta else {
        return Ok(Vec::new());
    };
    let nested = Punctuated::<Meta, Token![,]>::parse_terminated.parse2(list.tokens.clone())?;
    let mut literals = Vec::new();
    for meta in nested {
        let Some(kind) = clap_help_kind(meta.path()) else {
            continue;
        };
        match meta {
            Meta::NameValue(name_value) => {
                if let Expr::Lit(expression) = name_value.value
                    && let Lit::Str(literal) = expression.lit
                {
                    literals.push((kind, literal));
                }
            }
            Meta::List(list) => {
                if let Ok(literal) = syn::parse2::<LitStr>(list.tokens) {
                    literals.push((kind, literal));
                }
            }
            Meta::Path(_) => {}
        }
    }
    Ok(literals)
}

fn clap_help_kind(path: &syn::Path) -> Option<&'static str> {
    let identifier = &path.segments.last()?.ident;
    if identifier == "about" {
        Some("clap-about")
    } else if identifier == "long_about" {
        Some("clap-long-about")
    } else if identifier == "help" {
        Some("clap-help")
    } else {
        None
    }
}

fn is_exempt(
    exemption_lines: &ExemptionLines,
    target_start: LineColumn,
    target_end: LineColumn,
) -> bool {
    exemption_lines.trailing.contains(&target_end.line)
        || target_start.line > 1
            && exemption_lines
                .standalone
                .contains(&(target_start.line - 1))
}

#[derive(Default)]
struct ExemptionLines {
    standalone: BTreeSet<usize>,
    trailing: BTreeSet<usize>,
}

fn exemption_comment_lines(source: &str) -> Result<ExemptionLines, String> {
    let tokens = TokenStream::from_str(source)
        .map_err(|error| format!("could not tokenize source for exemptions: {error}"))?;
    let mut occupied = Vec::new();
    collect_token_ranges(tokens, &mut occupied);
    occupied.sort_unstable_by_key(|range| (range.start, range.end));

    let mut lines = ExemptionLines::default();
    let mut cursor = 0;
    for range in occupied {
        if cursor < range.start {
            collect_exemptions_from_gap(source, cursor, range.start, &mut lines);
        }
        cursor = cursor.max(range.end);
    }
    if cursor < source.len() {
        collect_exemptions_from_gap(source, cursor, source.len(), &mut lines);
    }
    Ok(lines)
}

fn collect_token_ranges(tokens: TokenStream, ranges: &mut Vec<std::ops::Range<usize>>) {
    for token in tokens {
        match token {
            TokenTree::Group(group) => {
                ranges.push(group.span_open().byte_range());
                collect_token_ranges(group.stream(), ranges);
                ranges.push(group.span_close().byte_range());
            }
            token => ranges.push(token.span().byte_range()),
        }
    }
}

fn collect_exemptions_from_gap(source: &str, start: usize, end: usize, lines: &mut ExemptionLines) {
    let bytes = source.as_bytes();
    let mut index = start;
    let mut block_depth = 0usize;
    while index < end {
        if bytes[index..end].starts_with(b"/*") {
            block_depth += 1;
            index += 2;
        } else if block_depth > 0 && bytes[index..end].starts_with(b"*/") {
            block_depth -= 1;
            index += 2;
        } else if block_depth == 0 && bytes[index..end].starts_with(b"//") {
            let comment_end = bytes[index..end]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(end, |offset| index + offset);
            if source[index..comment_end].contains("// i18n-exempt:") {
                let line = source[..index]
                    .bytes()
                    .filter(|byte| *byte == b'\n')
                    .count()
                    + 1;
                let line_start = source[..index]
                    .rfind('\n')
                    .map_or(0, |position| position + 1);
                if source[line_start..index].trim().is_empty() {
                    lines.standalone.insert(line);
                } else {
                    lines.trailing.insert(line);
                }
            }
            index = comment_end;
        } else {
            index += 1;
        }
    }
}

/// Whether resolved literal text contains alphabetic content outside `{...}`
/// placeholders. Punctuation-only and placeholder-only formats are allowed.
fn literal_has_letters(literal: &str) -> bool {
    let mut brace_depth = 0usize;
    let mut characters = literal.chars().peekable();
    while let Some(character) = characters.next() {
        if brace_depth == 0
            && matches!(character, '{' | '}')
            && characters.peek() == Some(&character)
        {
            characters.next();
            continue;
        }
        if character == '{' {
            brace_depth += 1;
        } else if character == '}' {
            brace_depth = brace_depth.saturating_sub(1);
        } else if brace_depth == 0 && character.is_alphabetic() {
            return true;
        }
    }
    false
}

fn normalize_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn parse_legacy_baseline(baseline: &str) -> BTreeMap<ViolationKey, usize> {
    let mut entries = BTreeMap::new();
    for (index, raw_line) in baseline.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let columns: Vec<&str> = line.split('\t').collect();
        assert_eq!(
            columns.len(),
            4,
            "legacy Fluent baseline line {} must have four tab-separated columns",
            index + 1
        );
        let count = columns[2].parse::<usize>().unwrap_or_else(|_| {
            panic!("invalid count on legacy Fluent baseline line {}", index + 1)
        });
        assert!(
            count > 0 && !columns[0].is_empty() && !columns[1].is_empty() && !columns[3].is_empty(),
            "legacy Fluent baseline line {} must have nonempty fields and a positive count",
            index + 1
        );
        assert_eq!(
            columns[3],
            normalize_whitespace(columns[3]),
            "legacy Fluent baseline literal on line {} must be normalized",
            index + 1
        );
        let key = ViolationKey {
            path: columns[0].to_string(),
            kind: columns[1].to_string(),
            literal: columns[3].to_string(),
        };
        assert!(
            entries.insert(key, count).is_none(),
            "duplicate legacy Fluent baseline entry on line {}",
            index + 1
        );
    }
    entries
}

fn compare_with_legacy_baseline(violations: &[Violation], baseline: &str) -> Vec<String> {
    let expected = parse_legacy_baseline(baseline);
    let mut actual: BTreeMap<ViolationKey, Vec<&Violation>> = BTreeMap::new();
    for violation in violations {
        actual
            .entry(violation.key.clone())
            .or_default()
            .push(violation);
    }

    let keys: BTreeSet<_> = expected.keys().chain(actual.keys()).cloned().collect();
    let mut problems = Vec::new();
    for key in keys {
        let expected_count = expected.get(&key).copied().unwrap_or(0);
        let actual_violations = actual.get(&key).map(Vec::as_slice).unwrap_or(&[]);
        let actual_count = actual_violations.len();
        if actual_count == expected_count {
            continue;
        }
        if actual_count > expected_count {
            let locations = actual_violations
                .iter()
                .map(|violation| format!("{}:{}", key.path, violation.line))
                .collect::<Vec<_>>()
                .join(", ");
            problems.push(format!(
                "  new/increased {kind} literal (expected {expected_count}, found {actual_count}) at {locations}: {kind}: {literal}",
                kind = key.kind,
                literal = key.literal
            ));
        } else {
            problems.push(format!(
                "  stale baseline entry (expected {expected_count}, found {actual_count}) for {} {}: {}",
                key.path, key.kind, key.literal
            ));
        }
    }
    problems
}

#[test]
fn fluent_detector_handles_rust_syntax_and_exemptions() {
    let source = r####"
fn sample() {
    let marker = "// i18n-exempt: not a comment";
    let quote = '\"';
    let bytes = br#"println!(\"not code\")"#;
    eprintln!["Error text"];
    println! { r#"Quoted \"text\" stays visible"# }
    println!(/* context */ "After comment");
    println!(
        "Multiline literal"
    );
    wrapper! { println!["Nested text"]; }
    wrapper! {
        #[command(about = "Nested clap text", long_about("Nested method text"))]
        struct NestedCli;
    }
    println!("{}", marker);
    println!("Forged marker {}", "// i18n-exempt: not a comment");
    /*
    // i18n-exempt: not a line comment
    */ println!("After forged block marker");
    // i18n-exempt: stable command example
    println!("zeroclaw auth login");
    print!("same-line exemption {}", marker); // i18n-exempt: fixed protocol text
    println!("Must not inherit trailing exemption");
}
"####;
    let mut violations = Vec::new();
    scan_source("sample.rs", source, &mut violations).unwrap();
    let observed = violations
        .iter()
        .map(|violation| (violation.key.kind.as_str(), violation.key.literal.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        observed,
        vec![
            ("eprintln", "\"Error text\""),
            ("println", "r#\"Quoted \\\"text\\\" stays visible\"#"),
            ("println", "\"After comment\""),
            ("println", "\"Multiline literal\""),
            ("println", "\"Nested text\""),
            ("clap-about", "\"Nested clap text\""),
            ("clap-long-about", "\"Nested method text\""),
            ("println", "\"Forged marker {}\""),
            ("println", "\"After forged block marker\""),
            ("println", "\"Must not inherit trailing exemption\""),
        ]
    );
}

#[test]
fn fluent_detector_covers_each_direct_print_macro() {
    let source = r#"
fn sample() {
    println!("Standard output line");
    print!("Standard output");
    eprintln!("Standard error line");
    eprint!("Standard error");
}
"#;
    let mut violations = Vec::new();
    scan_source("sample.rs", source, &mut violations).unwrap();
    let observed = violations
        .iter()
        .map(|violation| violation.key.kind.as_str())
        .collect::<Vec<_>>();
    assert_eq!(observed, vec!["println", "print", "eprintln", "eprint"]);
}

#[test]
fn fluent_detector_handles_clap_attributes_structurally() {
    let source = r###"
const LOOKALIKE: &str = r#"about = "not an attribute""#;
/* #[command(help = "not code")] */
#[command(
    about("Translate this"),
    long_about("Translate this in detail"),
)]
struct Cli;
#[arg(help("Translate this argument"))]
struct Argument;
// i18n-exempt: compile-time framework fallback
#[arg(help("Exempt help"))]
struct ExemptArgument;
"###;
    let mut violations = Vec::new();
    scan_source("sample.rs", source, &mut violations).unwrap();
    let observed = violations
        .iter()
        .map(|violation| (violation.key.kind.as_str(), violation.key.literal.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        observed,
        vec![
            ("clap-about", "\"Translate this\""),
            ("clap-long-about", "\"Translate this in detail\""),
            ("clap-help", "\"Translate this argument\""),
        ]
    );
}

#[test]
fn fluent_detector_distinguishes_format_fields_from_escaped_braces() {
    let source = r#"
fn sample(value: &str) {
    println!("{value}");
    println!("{{Visible text}}");
}
"#;
    let mut violations = Vec::new();
    scan_source("sample.rs", source, &mut violations).unwrap();
    assert_eq!(violations.len(), 1);
    assert_eq!(violations[0].key.literal, "\"{{Visible text}}\"");
}

#[test]
fn fluent_legacy_baseline_is_count_sensitive_and_shrinks() {
    let violation = Violation {
        key: ViolationKey {
            path: "sample.rs".to_string(),
            kind: "println".to_string(),
            literal: "\"Legacy text\"".to_string(),
        },
        line: 3,
    };
    let baseline = "sample.rs\tprintln\t1\t\"Legacy text\"\n";
    assert!(compare_with_legacy_baseline(std::slice::from_ref(&violation), baseline).is_empty());
    let increased = compare_with_legacy_baseline(&[violation.clone(), violation], baseline);
    assert!(increased[0].contains("new/increased"));
    let decreased = compare_with_legacy_baseline(&[], baseline);
    assert!(decreased[0].contains("stale baseline"));

    let new_signature = Violation {
        key: ViolationKey {
            path: "sample.rs".to_string(),
            kind: "eprintln".to_string(),
            literal: "\"New text\"".to_string(),
        },
        line: 8,
    };
    let new_problems = compare_with_legacy_baseline(&[new_signature], baseline);
    assert!(new_problems.iter().any(|problem| {
        problem.contains("new/increased eprintln literal (expected 0, found 1)")
    }));
}

#[test]
fn fluent_detector_fails_closed_on_invalid_source_and_missing_roots() {
    let mut violations = Vec::new();
    let parse_error = scan_source("invalid.rs", "fn broken(", &mut violations).unwrap_err();
    assert!(parse_error.contains("could not parse invalid.rs"));

    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing");
    let traversal_error = scan_dir(root.path(), &missing, &mut violations).unwrap_err();
    assert!(traversal_error.contains("could not inspect"));
}

#[cfg(unix)]
#[test]
fn fluent_detector_rejects_root_and_descendant_symlinks() {
    use std::os::unix::fs::symlink;

    let workspace = tempfile::tempdir().unwrap();
    let real_root = workspace.path().join("real-root");
    fs::create_dir(&real_root).unwrap();
    let linked_root = workspace.path().join("linked-root");
    symlink(&real_root, &linked_root).unwrap();

    let mut violations = Vec::new();
    let root_error = scan_dir(workspace.path(), &linked_root, &mut violations).unwrap_err();
    assert!(root_error.contains("symlink used as localization scan root"));

    let descendant_target = workspace.path().join("descendant-target.rs");
    fs::write(&descendant_target, "fn sample() {}\n").unwrap();
    symlink(&descendant_target, real_root.join("linked.rs")).unwrap();
    let descendant_error = scan_dir(workspace.path(), &real_root, &mut violations).unwrap_err();
    assert!(descendant_error.contains("symlink under localization scan root"));
}

#[test]
#[should_panic(expected = "nonempty fields and a positive count")]
fn fluent_legacy_baseline_rejects_zero_counts() {
    parse_legacy_baseline("sample.rs\tprintln\t0\t\"Legacy text\"\n");
}
