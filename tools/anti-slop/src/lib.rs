//! Opinionated, stable-toolchain Rust checks for low-evidence code patterns.

pub mod changed;
mod rules;

use std::path::{Path, PathBuf};

use syn::visit::Visit;

pub use rules::RULES;

/// One source-level anti-slop violation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub path: PathBuf,
    pub line: usize,
    pub column: usize,
    pub rule: &'static str,
    pub message: &'static str,
}

/// Parse and check one Rust source file.
pub fn check_source(path: &Path, source: &str) -> syn::Result<Vec<Diagnostic>> {
    let file = syn::parse_file(source)?;
    let mut analyzer = rules::Analyzer::new(path, source);
    analyzer.visit_file(&file);
    let mut diagnostics = analyzer.finish();
    diagnostics.sort_by(|left, right| {
        (left.line, left.column, left.rule).cmp(&(right.line, right.column, right.rule))
    });
    diagnostics.dedup();
    Ok(diagnostics)
}
