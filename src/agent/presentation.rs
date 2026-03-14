//! LLM presentation layer for tool output.
//!
//! Sits between tool execution and LLM-facing result formatting.
//! Applies three transformations:
//! 1. Strip ANSI escape codes (prevents garbage tokens)
//! 2. Overflow handling (truncate + save to file + exploration hints)
//! 3. Metadata footer (exit code or ok/err + duration)

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

pub use crate::config::schema::PresentationConfig;

static OVERFLOW_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Tools that use shell-style exit codes (0/1) in the metadata footer.
/// All other tools use "ok"/"err" labels instead.
const SHELL_LIKE_TOOLS: &[&str] = &["shell", "bg_run", "bg_status", "process"];

/// Strip ANSI escape codes from tool output.
pub fn strip_ansi(s: &str) -> String {
    strip_ansi_escapes::strip_str(s).to_string()
}

fn handle_overflow(output: &str, config: &PresentationConfig) -> String {
    let line_count = output.lines().count();
    let byte_count = output.len();

    if line_count <= config.max_output_lines && byte_count <= config.max_output_bytes {
        return output.to_string();
    }

    let overflow_path = save_overflow(output, &config.overflow_dir);

    // Truncate to max_output_lines, then enforce byte limit
    let mut truncated: String = output
        .lines()
        .take(config.max_output_lines)
        .collect::<Vec<_>>()
        .join("\n");

    // Safety: if a few long lines still exceed the byte limit, byte-truncate
    if truncated.len() > config.max_output_bytes {
        let mut cutoff = config.max_output_bytes;
        while cutoff > 0 && !truncated.is_char_boundary(cutoff) {
            cutoff -= 1;
        }
        truncated.truncate(cutoff);
    }

    let human_size = format_bytes(byte_count);
    let path_str = overflow_path.display();

    format!(
        "{truncated}\n\n--- output truncated ({line_count} lines, {human_size}) ---\n\
         Full output: {path_str}\n\
         Explore: cat {path_str} | grep <pattern>\n\
         \x20        cat {path_str} | tail 100"
    )
}

fn save_overflow(output: &str, overflow_dir: &str) -> PathBuf {
    let n = OVERFLOW_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = PathBuf::from(overflow_dir);
    fs::create_dir_all(&dir).ok();
    let path = dir.join(format!("cmd-{n}.txt"));
    fs::write(&path, output).ok();
    path
}

fn format_bytes(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1_048_576 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / 1_048_576.0)
    }
}

fn format_duration(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", d.as_secs_f64())
    }
}

/// Process tool output for LLM consumption.
///
/// Applies three transformations in order:
/// 1. Strip ANSI escape codes (prevents garbage tokens)
/// 2. Overflow handling (truncate + save to file + exploration hints)
/// 3. Metadata footer (exit code or ok/err + duration)
pub fn present_for_llm(
    output: &str,
    tool_name: &str,
    success: bool,
    duration: Duration,
    config: &PresentationConfig,
) -> String {
    let mut result = output.to_string();

    // Step 1: Strip ANSI escape codes
    if config.strip_ansi {
        result = strip_ansi(&result);
    }

    // Step 2: Overflow handling
    result = handle_overflow(&result, config);

    // Step 3: Metadata footer
    if config.show_metadata {
        let dur = format_duration(duration);
        let status = if SHELL_LIKE_TOOLS.contains(&tool_name) {
            // Shell-like tools use numeric exit codes (familiar from training data)
            if success {
                "exit:0".to_string()
            } else {
                "exit:1".to_string()
            }
        } else {
            // Non-shell tools use ok/err labels (semantically accurate)
            if success {
                "ok".to_string()
            } else {
                "err".to_string()
            }
        };
        result.push_str(&format!("\n[{status} | {dur}]"));
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ANSI stripping tests ──

    #[test]
    fn strip_ansi_removes_color_codes() {
        let input = "\x1b[31mERROR\x1b[0m: something failed";
        assert_eq!(strip_ansi(input), "ERROR: something failed");
    }

    #[test]
    fn strip_ansi_removes_cursor_movement() {
        let input = "\x1b[2K\x1b[1G50% complete";
        assert_eq!(strip_ansi(input), "50% complete");
    }

    #[test]
    fn strip_ansi_preserves_plain_text() {
        let input = "no escape codes here\nline two";
        assert_eq!(strip_ansi(input), input);
    }

    #[test]
    fn strip_ansi_handles_empty_string() {
        assert_eq!(strip_ansi(""), "");
    }

    #[test]
    fn strip_ansi_handles_nested_codes() {
        let input = "\x1b[1m\x1b[32mBOLD GREEN\x1b[0m normal";
        assert_eq!(strip_ansi(input), "BOLD GREEN normal");
    }

    // ── Overflow tests ──

    #[test]
    fn overflow_short_output_unchanged() {
        let config = PresentationConfig::default();
        let input = "line1\nline2\nline3";
        let result = handle_overflow(input, &config);
        assert_eq!(result, input);
    }

    #[test]
    fn overflow_truncates_at_line_limit() {
        let config = PresentationConfig {
            max_output_lines: 3,
            max_output_bytes: 1_000_000,
            overflow_dir: std::env::temp_dir().to_string_lossy().into_owned(),
            ..Default::default()
        };
        let input = (1..=10)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = handle_overflow(&input, &config);
        assert!(result.contains("line 1\nline 2\nline 3"));
        assert!(result.contains("--- output truncated"));
        assert!(result.contains("10 lines"));
        assert!(result.contains("Full output:"));
        assert!(result.contains("grep <pattern>"));
        assert!(!result.contains("line 4"));
    }

    #[test]
    fn overflow_truncates_at_byte_limit() {
        let config = PresentationConfig {
            max_output_lines: 1_000_000,
            max_output_bytes: 50,
            overflow_dir: std::env::temp_dir().to_string_lossy().into_owned(),
            ..Default::default()
        };
        let input = "a".repeat(200);
        let result = handle_overflow(&input, &config);
        assert!(result.contains("--- output truncated"));
        assert!(result.contains("Full output:"));
    }

    #[test]
    fn overflow_saves_full_output_to_file() {
        let overflow_dir = std::env::temp_dir().join("zeroclaw-test-overflow");
        let config = PresentationConfig {
            max_output_lines: 2,
            max_output_bytes: 1_000_000,
            overflow_dir: overflow_dir.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let input = "line1\nline2\nline3\nline4\nline5";
        let result = handle_overflow(&input, &config);

        // Extract file path from output
        let path_line = result
            .lines()
            .find(|l| l.starts_with("Full output:"))
            .unwrap();
        let path_str = path_line.trim_start_matches("Full output: ").trim();
        let saved = fs::read_to_string(path_str).unwrap();
        assert_eq!(saved, input);

        // Cleanup
        let _ = fs::remove_dir_all(&overflow_dir);
    }

    // ── Duration formatting tests ──

    #[test]
    fn format_duration_milliseconds() {
        assert_eq!(format_duration(Duration::from_millis(42)), "42ms");
        assert_eq!(format_duration(Duration::from_millis(999)), "999ms");
    }

    #[test]
    fn format_duration_seconds() {
        assert_eq!(format_duration(Duration::from_millis(1200)), "1.2s");
        assert_eq!(format_duration(Duration::from_secs(45)), "45.0s");
    }

    // ── present_for_llm integration tests ──

    #[test]
    fn present_for_llm_shell_uses_exit_codes() {
        let config = PresentationConfig::default();
        let result =
            present_for_llm("hello", "shell", true, Duration::from_millis(42), &config);
        assert!(result.ends_with("[exit:0 | 42ms]"));
    }

    #[test]
    fn present_for_llm_shell_failure_exit_code() {
        let config = PresentationConfig::default();
        let result = present_for_llm(
            "Error: not found",
            "shell",
            false,
            Duration::from_millis(5),
            &config,
        );
        assert!(result.ends_with("[exit:1 | 5ms]"));
    }

    #[test]
    fn present_for_llm_non_shell_uses_ok_err() {
        let config = PresentationConfig::default();
        let result = present_for_llm(
            "file contents",
            "file_read",
            true,
            Duration::from_millis(2),
            &config,
        );
        assert!(result.ends_with("[ok | 2ms]"));
        let result = present_for_llm(
            "Error: not found",
            "file_read",
            false,
            Duration::from_millis(3),
            &config,
        );
        assert!(result.ends_with("[err | 3ms]"));
    }

    #[test]
    fn present_for_llm_no_metadata_when_disabled() {
        let config = PresentationConfig {
            show_metadata: false,
            ..Default::default()
        };
        let result =
            present_for_llm("hello", "shell", true, Duration::from_millis(42), &config);
        assert_eq!(result, "hello");
    }

    #[test]
    fn present_for_llm_full_pipeline() {
        let config = PresentationConfig {
            max_output_lines: 2,
            overflow_dir: std::env::temp_dir().to_string_lossy().into_owned(),
            ..Default::default()
        };
        let input = "\x1b[31mline1\x1b[0m\nline2\nline3";
        let result =
            present_for_llm(input, "shell", true, Duration::from_millis(10), &config);
        assert!(result.contains("line1")); // ANSI stripped
        assert!(!result.contains("\x1b")); // no raw escapes
        assert!(result.contains("truncated")); // overflow triggered
        assert!(result.contains("[exit:0 | 10ms]")); // metadata
    }
}
