//! Architecture gate: `StreamError::Terminal` has exactly one producer.

use std::fs;
use std::path::Path;

/// R4 (spec addendum): a terminal stream error tells the runtime
/// "the provider already exhausted its own retry/fallback budget; do not
/// retry or fall back". Only the router's synthesized arm may produce it:
/// that arm wraps an ALREADY-COMPLETED non-streaming call whose failure
/// survived the full retry ladder. A genuine streaming leg must never emit
/// it: the runtime would strand partial output without recovery, and a
/// mid-stream producer would silently skip the non-streaming fallback the
/// stream contract promises.
const ALLOWED_PRODUCER_FILES: &[&str] = &["zeroclaw-providers/src/router.rs"];

/// Directories scanned for construction sites.
const SCAN_ROOTS: &[&str] = &["crates", "apps"];

#[test]
fn stream_error_terminal_is_produced_only_by_router_synthesis() {
    let workspace_root = workspace_root();
    let mut violations: Vec<String> = Vec::new();
    for root in SCAN_ROOTS {
        scan_dir(&workspace_root.join(root), &mut violations);
    }
    assert!(
        violations.is_empty(),
        "StreamError::Terminal construction outside the router's synthesized \
         arm detected. Terminal means 'do not retry or fall back'; a genuine \
         streaming leg must never emit it. To override, add `// SOT: <reason>` \
         on the offending line.\n\nViolations:\n{}",
        violations.join("\n")
    );
}

/// The synthesized path's failure logging must keep its two lines distinct:
/// the genuine-stream recovery logs `llm_stream_fallback`, and the terminal
/// branch (which must NOT re-run the non-streaming call) logs its own
/// `llm_stream_terminal` line. Counting both pins R4-D3's "log line distinct
/// from llm_stream_fallback" at the source level.
#[test]
fn synthesized_failure_logging_keeps_fallback_and_terminal_lines_distinct() {
    let src = fs::read_to_string(
        workspace_root().join("crates/zeroclaw-runtime/src/agent/turn/provider_call.rs"),
    )
    .expect("provider_call.rs must exist");
    let fallback_lines = src.matches("llm_stream_fallback").count();
    let terminal_lines = src.matches("llm_stream_terminal").count();
    assert_eq!(
        fallback_lines, 1,
        "llm_stream_fallback must appear exactly once: the genuine-stream \
         recovery branch"
    );
    assert_eq!(
        terminal_lines, 1,
        "llm_stream_terminal must appear exactly once: the terminal branch \
         that skips the non-streaming fallback"
    );
}

fn workspace_root() -> std::path::PathBuf {
    // `CARGO_MANIFEST_DIR` for the workspace's top-level crate (the
    // `zeroclaw` binary) — that's where `cargo test` invokes from.
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
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
        let display = path.display().to_string();
        let allowed = ALLOWED_PRODUCER_FILES
            .iter()
            .any(|allowed| display.contains(allowed));
        let Ok(src) = fs::read_to_string(&path) else {
            continue;
        };
        for (lineno, line) in src.lines().enumerate() {
            if line.contains("// SOT:") {
                continue;
            }
            if !is_terminal_construction(line) {
                continue;
            }
            if allowed {
                continue;
            }
            violations.push(format!(
                "  {}:{}: {}",
                display,
                lineno + 1,
                line.trim_start()
            ));
        }
    }
}

/// A construction site names the variant with a real argument
/// (`StreamError::Terminal(error.to_string())`). A pattern match binds `_`
/// (`StreamError::Terminal(_)`) and is allowed anywhere. Line-based, like
/// the sibling gates; a construction split across lines evades this scan
/// the same way `no_duplicate_state`'s heuristics can.
fn is_terminal_construction(line: &str) -> bool {
    const NEEDLE: &str = "StreamError::Terminal(";
    let Some(idx) = line.find(NEEDLE) else {
        return false;
    };
    let after = &line[idx + NEEDLE.len()..];
    !after.starts_with('_')
}
