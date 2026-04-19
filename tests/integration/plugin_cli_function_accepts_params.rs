#![cfg(feature = "plugins-wasm")]

//! Test: SDK cli_exec function accepts command/args/working_dir/env.
//!
//! Task US-ZCL-59-2: Verifies acceptance criterion for US-ZCL-59:
//! > Function accepts command/args/working_dir/env
//!
//! These tests verify that the zeroclaw-plugin-sdk cli_exec function
//! and its associated types accept all required parameters.

use std::collections::HashMap;
use zeroclaw::plugins::host_functions::{CliExecRequest, CliExecResponse};

// ---------------------------------------------------------------------------
// SDK cli_exec function parameter tests
// ---------------------------------------------------------------------------

/// AC: Function accepts `command` as a string.
#[test]
fn cli_exec_accepts_command_parameter() {
    let request = CliExecRequest {
        command: "git".to_string(),
        args: vec![],
        working_dir: None,
        env: None,
    };

    assert_eq!(request.command, "git");
}

/// AC: Function accepts `args` as a list of strings.
#[test]
fn cli_exec_accepts_args_parameter() {
    let request = CliExecRequest {
        command: "git".to_string(),
        args: vec!["status".to_string(), "--short".to_string()],
        working_dir: None,
        env: None,
    };

    assert_eq!(request.args.len(), 2);
    assert_eq!(request.args[0], "status");
    assert_eq!(request.args[1], "--short");
}

/// AC: Function accepts empty args list.
#[test]
fn cli_exec_accepts_empty_args() {
    let request = CliExecRequest {
        command: "pwd".to_string(),
        args: vec![],
        working_dir: None,
        env: None,
    };

    assert!(request.args.is_empty());
}

/// AC: Function accepts `working_dir` as optional string.
#[test]
fn cli_exec_accepts_working_dir_parameter() {
    // With working_dir
    let with_dir = CliExecRequest {
        command: "ls".to_string(),
        args: vec![],
        working_dir: Some("/home/user/project".to_string()),
        env: None,
    };

    assert_eq!(with_dir.working_dir, Some("/home/user/project".to_string()));

    // Without working_dir
    let without_dir = CliExecRequest {
        command: "ls".to_string(),
        args: vec![],
        working_dir: None,
        env: None,
    };

    assert!(without_dir.working_dir.is_none());
}

/// AC: Function accepts `env` as optional HashMap<String, String>.
#[test]
fn cli_exec_accepts_env_parameter() {
    let mut env = HashMap::new();
    env.insert("PATH".to_string(), "/usr/local/bin:/usr/bin".to_string());
    env.insert("HOME".to_string(), "/home/user".to_string());

    let with_env = CliExecRequest {
        command: "env".to_string(),
        args: vec![],
        working_dir: None,
        env: Some(env.clone()),
    };

    assert_eq!(with_env.env, Some(env));
}

/// AC: Function accepts env as None.
#[test]
fn cli_exec_accepts_no_env() {
    let request = CliExecRequest {
        command: "echo".to_string(),
        args: vec!["hello".to_string()],
        working_dir: None,
        env: None,
    };

    assert!(request.env.is_none());
}

/// AC: Function accepts all parameters together.
#[test]
fn cli_exec_accepts_all_parameters() {
    let mut env = HashMap::new();
    env.insert("DEBUG".to_string(), "1".to_string());

    let request = CliExecRequest {
        command: "cargo".to_string(),
        args: vec!["build".to_string(), "--release".to_string()],
        working_dir: Some("/workspace/zeroclaw".to_string()),
        env: Some(env.clone()),
    };

    assert_eq!(request.command, "cargo");
    assert_eq!(request.args, vec!["build", "--release"]);
    assert_eq!(request.working_dir, Some("/workspace/zeroclaw".to_string()));
    assert_eq!(request.env, Some(env));
}

// ---------------------------------------------------------------------------
// Response struct verification (confirms return type)
// ---------------------------------------------------------------------------

/// AC: Response includes all expected fields.
#[test]
fn cli_exec_response_has_expected_fields() {
    let response = CliExecResponse {
        stdout: "success".to_string(),
        stderr: "warning".to_string(),
        exit_code: 0,
        truncated: false,
        timed_out: false,
    };

    assert_eq!(response.stdout, "success");
    assert_eq!(response.stderr, "warning");
    assert_eq!(response.exit_code, 0);
    assert!(!response.truncated);
    assert!(!response.timed_out);
}

// ---------------------------------------------------------------------------
// JSON parameter serialization (required for host function communication)
// ---------------------------------------------------------------------------

/// AC: Parameters serialize correctly to JSON for host function call.
#[test]
fn cli_exec_parameters_serialize_correctly() {
    let mut env = HashMap::new();
    env.insert("KEY".to_string(), "value".to_string());

    let request = CliExecRequest {
        command: "npm".to_string(),
        args: vec!["install".to_string()],
        working_dir: Some("/app".to_string()),
        env: Some(env),
    };

    let json = serde_json::to_string(&request).expect("must serialize");

    // Verify all parameter fields are present in JSON
    assert!(
        json.contains("\"command\""),
        "JSON must contain command field"
    );
    assert!(json.contains("\"args\""), "JSON must contain args field");
    assert!(
        json.contains("\"working_dir\""),
        "JSON must contain working_dir field"
    );
    assert!(json.contains("\"env\""), "JSON must contain env field");
}

/// AC: Parameters deserialize correctly from JSON.
#[test]
fn cli_exec_parameters_deserialize_correctly() {
    let json = r#"{
        "command": "docker",
        "args": ["ps", "-a"],
        "working_dir": "/var/run",
        "env": {"DOCKER_HOST": "unix:///var/run/docker.sock"}
    }"#;

    let request: CliExecRequest = serde_json::from_str(json).expect("must deserialize");

    assert_eq!(request.command, "docker");
    assert_eq!(request.args, vec!["ps", "-a"]);
    assert_eq!(request.working_dir, Some("/var/run".to_string()));
    assert!(request.env.is_some());
    assert_eq!(
        request.env.as_ref().unwrap().get("DOCKER_HOST"),
        Some(&"unix:///var/run/docker.sock".to_string())
    );
}
