use anyhow::{Context, Result};
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::security::constrained_runner::{ConstrainedRunner, warn_if_unsandboxed};
use crate::security::traits::{NoopSandbox, Sandbox};

const TEST_FILE_NAME: &str = "TEST.sh";

/// Wall-clock budget for a single `TEST.sh` case. A skill test that runs
/// longer than this is killed and reported as a failure — previously these
/// commands had no timeout and inherited the daemon environment.
const TEST_CASE_TIMEOUT: Duration = Duration::from_secs(60);

/// Per-stream capture cap for a `TEST.sh` command. Generous (the legacy path
/// captured unbounded output) but still bounded so a runaway command cannot
/// exhaust memory; when a stream hits the cap the failure output says so, so a
/// pattern that appears past the cap does not fail silently.
const TEST_OUTPUT_CAP_BYTES: usize = 4 * 1024 * 1024;

/// Result of running all tests for a single skill.
#[derive(Debug, Clone)]
pub struct SkillTestResult {
    pub skill_name: String,
    pub tests_run: usize,
    pub tests_passed: usize,
    pub failures: Vec<TestFailure>,
}

/// Details about a single failed test case.
#[derive(Debug, Clone)]
pub struct TestFailure {
    pub command: String,
    pub expected_exit: i32,
    pub actual_exit: i32,
    pub expected_pattern: String,
    pub actual_output: String,
}

/// A parsed test case from a TEST.sh line.
#[derive(Debug, Clone)]
struct TestCase {
    command: String,
    expected_exit: i32,
    expected_pattern: String,
}

/// Parse a single TEST.sh line into a `TestCase`.
///
/// Expected format: `command | expected_exit_code | expected_output_pattern`
fn parse_test_line(line: &str) -> Option<TestCase> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }

    // Split on ` | ` (pipe surrounded by spaces) to avoid splitting on shell
    // pipes inside the command itself. Fall back to bare `|` splitting only if
    // the line contains exactly two ` | ` delimiters.
    let parts: Vec<&str> = trimmed.split(" | ").collect();
    if parts.len() < 3 {
        // Try splitting on `|` as fallback
        let parts: Vec<&str> = trimmed.splitn(3, '|').collect();
        if parts.len() < 3 {
            return None;
        }
        let command = parts[0].trim().to_string();
        let expected_exit = parts[1].trim().parse::<i32>().ok()?;
        let expected_pattern = parts[2].trim().to_string();
        return Some(TestCase {
            command,
            expected_exit,
            expected_pattern,
        });
    }

    let command = parts[0].trim().to_string();
    let expected_exit = parts[1].trim().parse::<i32>().ok()?;
    // Rejoin remaining parts in case the pattern itself contains ` | `
    let expected_pattern = parts[2..].join(" | ").trim().to_string();

    Some(TestCase {
        command,
        expected_exit,
        expected_pattern,
    })
}

/// Check whether `output` matches `pattern`.
///
/// If the pattern looks like a regex (contains regex metacharacters beyond a
/// simple `/` path), we attempt a regex match. Otherwise we fall back to a
/// simple substring check.
fn pattern_matches(output: &str, pattern: &str) -> bool {
    if pattern.is_empty() {
        return true;
    }
    // Try regex first
    if let Ok(re) = Regex::new(pattern)
        && re.is_match(output)
    {
        return true;
    }
    // Fallback: substring match
    output.contains(pattern)
}

/// Run a single test case under the [`ConstrainedRunner`] and return a
/// possible failure.
fn run_test_case(
    case: &TestCase,
    skill_dir: &Path,
    sandbox: &Arc<dyn Sandbox>,
    verbose: bool,
) -> Option<TestFailure> {
    if verbose {
        println!("    running: {}", case.command);
    }

    let result = build_test_runner(&case.command, skill_dir, sandbox).run();

    let output = match result {
        Ok(o) => o,
        Err(err) => {
            return Some(TestFailure {
                command: case.command.clone(),
                expected_exit: case.expected_exit,
                actual_exit: -1,
                expected_pattern: case.expected_pattern.clone(),
                actual_output: format!("failed to execute command: {err}"),
            });
        }
    };

    let actual_exit = if output.timed_out {
        -1
    } else {
        output.exit_code.unwrap_or(-1)
    };
    let combined = if output.timed_out {
        format!(
            "{}{}\n{}",
            output.stdout,
            output.stderr,
            crate::i18n::get_required_cli_string_with_args(
                "cli-skills-test-timeout",
                &[("seconds", &TEST_CASE_TIMEOUT.as_secs().to_string())],
            )
        )
    } else if output.output_truncated {
        format!(
            "{}\n{}",
            output.combined(),
            crate::i18n::get_required_cli_string_with_args(
                "cli-skills-test-output-truncated",
                &[("bytes", &TEST_OUTPUT_CAP_BYTES.to_string())],
            )
        )
    } else {
        output.combined()
    };

    if verbose {
        if !output.stdout.is_empty() {
            println!("    stdout: {}", output.stdout.trim());
        }
        if !output.stderr.is_empty() {
            println!("    stderr: {}", output.stderr.trim());
        }
        println!("    exit: {actual_exit}");
    }

    let exit_ok = !output.timed_out && actual_exit == case.expected_exit;
    let pattern_ok = !output.timed_out && pattern_matches(&combined, &case.expected_pattern);

    if exit_ok && pattern_ok {
        None
    } else {
        Some(TestFailure {
            command: case.command.clone(),
            expected_exit: case.expected_exit,
            actual_exit,
            expected_pattern: case.expected_pattern.clone(),
            actual_output: combined,
        })
    }
}

/// Build the constrained runner for one `TEST.sh` command: `sh -c <cmd>`
/// (`cmd /C` on Windows), confined to `skill_dir`, env-scrubbed, timed, and
/// output-capped, wrapped by the active `sandbox` backend.
///
/// On Windows the command is passed via `raw_arg` (not `.args`), so `cmd.exe`
/// receives the `/C "<command>"` form verbatim under its own quoting rules —
/// std's MSVC-style escaping mangles commands containing quotes — and
/// `CREATE_NO_WINDOW` keeps a console from flashing (#7083 quoting contract).
#[cfg(windows)]
fn build_test_runner(
    command: &str,
    skill_dir: &Path,
    sandbox: &Arc<dyn Sandbox>,
) -> ConstrainedRunner {
    let runner = ConstrainedRunner::new("cmd")
        .windows_raw_arg("/C")
        .windows_raw_arg(zeroclaw_config::platform::native::windows_cmd_shell_raw_arg(command))
        .windows_no_window()
        .workdir(skill_dir)
        .timeout(TEST_CASE_TIMEOUT)
        .output_cap_bytes(TEST_OUTPUT_CAP_BYTES)
        .sandbox(sandbox.clone());
    with_inherited_home(runner)
}

#[cfg(not(windows))]
fn build_test_runner(
    command: &str,
    skill_dir: &Path,
    sandbox: &Arc<dyn Sandbox>,
) -> ConstrainedRunner {
    let runner = ConstrainedRunner::new("sh")
        .arg("-c")
        .arg(command)
        .workdir(skill_dir)
        .timeout(TEST_CASE_TIMEOUT)
        .output_cap_bytes(TEST_OUTPUT_CAP_BYTES)
        .sandbox(sandbox.clone());
    with_inherited_home(runner)
}

/// Forward the daemon's `HOME` to the scrubbed child so a `TEST.sh` case that
/// resolves `~` or reads user config works, without re-exposing the rest of the
/// inherited environment. `HOME` is a path, not a secret; the runner's
/// allowlist keeps everything else stripped.
fn with_inherited_home(runner: ConstrainedRunner) -> ConstrainedRunner {
    match std::env::var_os("HOME") {
        Some(home) => runner.with_home(home),
        None => runner,
    }
}

/// Test a single skill by parsing and running its TEST.sh under the active
/// sandbox backend. When the backend provides no OS isolation
/// (`NoopSandbox`), a one-line warning is printed so operators know the
/// commands ran unconfined.
pub fn test_skill(skill_dir: &Path, skill_name: &str, verbose: bool) -> Result<SkillTestResult> {
    test_skill_with_sandbox(skill_dir, skill_name, verbose, &default_sandbox())
}

/// Default sandbox for skill testing when the caller has no policy-resolved
/// backend to pass: no OS isolation. TEST.sh execution then prints the
/// unsandboxed warning. (A policy-driven backend arrives with the detonation
/// work; this keeps the migration behavior-preserving.)
fn default_sandbox() -> Arc<dyn Sandbox> {
    Arc::new(NoopSandbox)
}

/// [`test_skill`] with an explicit sandbox backend.
pub fn test_skill_with_sandbox(
    skill_dir: &Path,
    skill_name: &str,
    verbose: bool,
    sandbox: &Arc<dyn Sandbox>,
) -> Result<SkillTestResult> {
    let test_file = skill_dir.join(TEST_FILE_NAME);
    if !test_file.exists() {
        return Ok(SkillTestResult {
            skill_name: skill_name.to_string(),
            tests_run: 0,
            tests_passed: 0,
            failures: Vec::new(),
        });
    }

    let content = std::fs::read_to_string(&test_file)
        .with_context(|| format!("failed to read {}", test_file.display().to_string()))?;

    let cases: Vec<TestCase> = content.lines().filter_map(parse_test_line).collect();

    let mut result = SkillTestResult {
        skill_name: skill_name.to_string(),
        tests_run: cases.len(),
        tests_passed: 0,
        failures: Vec::new(),
    };

    if !cases.is_empty() {
        warn_if_unsandboxed(sandbox, skill_name);
    }

    for case in &cases {
        match run_test_case(case, skill_dir, sandbox, verbose) {
            None => result.tests_passed += 1,
            Some(failure) => result.failures.push(failure),
        }
    }

    Ok(result)
}

/// Test all skills that have a TEST.sh file within the given skill directories.
pub fn test_all_skills(skills_dirs: &[PathBuf], verbose: bool) -> Result<Vec<SkillTestResult>> {
    test_all_skills_with_sandbox(skills_dirs, verbose, &default_sandbox())
}

/// [`test_all_skills`] with an explicit sandbox backend.
pub fn test_all_skills_with_sandbox(
    skills_dirs: &[PathBuf],
    verbose: bool,
    sandbox: &Arc<dyn Sandbox>,
) -> Result<Vec<SkillTestResult>> {
    let mut results = Vec::new();

    for dir in skills_dirs {
        if !dir.exists() || !dir.is_dir() {
            continue;
        }

        let entries = std::fs::read_dir(dir)
            .with_context(|| format!("failed to read directory {}", dir.display().to_string()))?;

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let test_file = path.join(TEST_FILE_NAME);
            if !test_file.exists() {
                continue;
            }
            let skill_name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();

            if verbose {
                println!(
                    "  Testing skill: {} ({})",
                    skill_name,
                    path.display().to_string()
                );
            }

            let r = test_skill_with_sandbox(&path, &skill_name, verbose, sandbox)?;
            results.push(r);
        }
    }

    Ok(results)
}

/// Pretty-print test results using the `console` crate.
pub fn print_results(results: &[SkillTestResult]) {
    if results.is_empty() {
        println!("No skills with {} found.", TEST_FILE_NAME);
        return;
    }

    println!();
    for r in results {
        if r.tests_run == 0 {
            println!(
                "  {} {} — no test cases",
                console::style("-").dim(),
                r.skill_name,
            );
            continue;
        }

        if r.failures.is_empty() {
            println!(
                "  {} {} — {}/{} passed",
                console::style("✓").green().bold(),
                console::style(&r.skill_name).white().bold(),
                r.tests_passed,
                r.tests_run,
            );
        } else {
            println!(
                "  {} {} — {}/{} passed",
                console::style("✗").red().bold(),
                console::style(&r.skill_name).white().bold(),
                r.tests_passed,
                r.tests_run,
            );
            for f in &r.failures {
                println!("    command:  {}", console::style(&f.command).dim(),);
                println!(
                    "    expected: exit={}, pattern={}",
                    f.expected_exit, f.expected_pattern,
                );
                println!(
                    "    actual:   exit={}, output={}",
                    f.actual_exit,
                    truncate_output(&f.actual_output, 200),
                );
                println!();
            }
        }
    }

    let total_run: usize = results.iter().map(|r| r.tests_run).sum();
    let total_passed: usize = results.iter().map(|r| r.tests_passed).sum();
    let total_failed = total_run - total_passed;

    println!();
    if total_failed == 0 {
        println!(
            "  {} All {total_run} test(s) passed across {} skill(s).",
            console::style("✓").green().bold(),
            results.len(),
        );
    } else {
        println!(
            "  {} {total_failed} of {total_run} test(s) failed across {} skill(s).",
            console::style("✗").red().bold(),
            results.len(),
        );
    }
    println!();
}

fn truncate_output(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    if trimmed.len() <= max {
        trimmed.replace('\n', " ")
    } else {
        // `max` is a byte count; slicing at it directly panics when it lands
        // inside a multi-byte UTF-8 char (non-ASCII skill output). Round down
        // to the nearest char boundary first (matches skills/review.rs).
        let end = trimmed.floor_char_boundary(max);
        format!("{}...", &trimmed[..end].replace('\n', " "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn truncate_output_does_not_panic_on_multibyte_boundary() {
        // Regression for #7828: `max` landing inside a multi-byte UTF-8 char
        // must not panic. "🦀" is 4 bytes; max=2 is mid-char.
        let out = truncate_output("🦀🦀🦀", 2);
        assert!(
            out.ends_with("..."),
            "long output must be truncated: {out:?}"
        );
        // Whatever is kept must be valid UTF-8 (no byte-boundary split).
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());

        // A multi-byte string longer than max but whose boundary is mid-char.
        let out2 = truncate_output("héllo wörld with áccents", 5);
        assert!(out2.ends_with("..."));
        assert!(std::str::from_utf8(out2.as_bytes()).is_ok());

        // ASCII within the limit is returned untruncated (newlines flattened).
        assert_eq!(truncate_output("ok\nfine", 100), "ok fine");
    }

    #[test]
    fn parse_comment_and_empty_lines() {
        assert!(parse_test_line("").is_none());
        assert!(parse_test_line("   ").is_none());
        assert!(parse_test_line("# this is a comment").is_none());
        assert!(parse_test_line("  # indented comment").is_none());
    }

    #[test]
    fn parse_valid_test_line() {
        let case = parse_test_line("echo hello | 0 | hello").unwrap();
        assert_eq!(case.command, "echo hello");
        assert_eq!(case.expected_exit, 0);
        assert_eq!(case.expected_pattern, "hello");
    }

    #[test]
    fn parse_line_with_spaces_in_pattern() {
        let case = parse_test_line("echo 'hello world' | 0 | hello world").unwrap();
        assert_eq!(case.command, "echo 'hello world'");
        assert_eq!(case.expected_exit, 0);
        assert_eq!(case.expected_pattern, "hello world");
    }

    #[test]
    fn parse_invalid_line_missing_parts() {
        assert!(parse_test_line("just a command").is_none());
        assert!(parse_test_line("cmd | notanumber | pattern").is_none());
    }

    #[test]
    fn pattern_matches_empty() {
        assert!(pattern_matches("anything", ""));
    }

    #[test]
    fn pattern_matches_substring() {
        assert!(pattern_matches("hello world", "hello"));
        assert!(pattern_matches("hello world", "world"));
        assert!(!pattern_matches("hello world", "missing"));
    }

    #[test]
    fn pattern_matches_regex() {
        assert!(pattern_matches("hello world 42", r"world \d+"));
        assert!(pattern_matches("/usr/bin/bash", r"/"));
        assert!(!pattern_matches("hello", r"^\d+$"));
    }

    #[test]
    fn test_skill_with_echo() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("echo-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("TEST.sh"),
            "# Echo test\necho hello | 0 | hello\n",
        )
        .unwrap();

        let result = test_skill(&skill_dir, "echo-skill", false).unwrap();
        assert_eq!(result.tests_run, 1);
        assert_eq!(result.tests_passed, 1);
        assert!(result.failures.is_empty());
    }

    #[test]
    fn test_skill_without_test_file() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("no-tests");
        fs::create_dir_all(&skill_dir).unwrap();

        let result = test_skill(&skill_dir, "no-tests", false).unwrap();
        assert_eq!(result.tests_run, 0);
        assert_eq!(result.tests_passed, 0);
        assert!(result.failures.is_empty());
    }

    #[test]
    fn test_skill_with_failing_test() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("fail-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("TEST.sh"), "echo hello | 1 | goodbye\n").unwrap();

        let result = test_skill(&skill_dir, "fail-skill", false).unwrap();
        assert_eq!(result.tests_run, 1);
        assert_eq!(result.tests_passed, 0);
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].expected_exit, 1);
        assert_eq!(result.failures[0].actual_exit, 0);
    }

    #[test]
    fn test_skill_exit_code_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("exit-mismatch");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("TEST.sh"),
            format!("{} | 0 | \n", failing_command()),
        )
        .unwrap();

        let result = test_skill(&skill_dir, "exit-mismatch", false).unwrap();
        assert_eq!(result.tests_run, 1);
        assert_eq!(result.tests_passed, 0);
        assert_eq!(result.failures[0].actual_exit, 1);
    }

    #[cfg(windows)]
    fn failing_command() -> &'static str {
        "exit /B 1"
    }

    #[cfg(not(windows))]
    fn failing_command() -> &'static str {
        "false"
    }

    #[test]
    fn test_result_aggregation() {
        let results = [
            SkillTestResult {
                skill_name: "a".to_string(),
                tests_run: 3,
                tests_passed: 3,
                failures: Vec::new(),
            },
            SkillTestResult {
                skill_name: "b".to_string(),
                tests_run: 2,
                tests_passed: 1,
                failures: vec![TestFailure {
                    command: "false".to_string(),
                    expected_exit: 0,
                    actual_exit: 1,
                    expected_pattern: String::new(),
                    actual_output: String::new(),
                }],
            },
        ];

        let total_run: usize = results.iter().map(|r| r.tests_run).sum();
        let total_passed: usize = results.iter().map(|r| r.tests_passed).sum();
        assert_eq!(total_run, 5);
        assert_eq!(total_passed, 4);
    }

    #[test]
    fn test_all_skills_finds_skills_with_tests() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");

        // Skill with TEST.sh
        let skill_a = skills_dir.join("skill-a");
        fs::create_dir_all(&skill_a).unwrap();
        fs::write(skill_a.join("TEST.sh"), "echo ok | 0 | ok\n").unwrap();

        // Skill without TEST.sh — should be skipped
        let skill_b = skills_dir.join("skill-b");
        fs::create_dir_all(&skill_b).unwrap();

        let results = test_all_skills(std::slice::from_ref(&skills_dir), false).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].skill_name, "skill-a");
        assert_eq!(results[0].tests_passed, 1);
    }

    #[test]
    fn test_truncate_output() {
        assert_eq!(truncate_output("short", 100), "short");
        let long = "a".repeat(300);
        let truncated = truncate_output(&long, 200);
        assert!(truncated.ends_with("..."));
        assert!(truncated.len() <= 204); // 200 + "..."
    }
}
