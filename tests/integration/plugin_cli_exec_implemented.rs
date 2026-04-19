#![cfg(feature = "plugins-wasm")]

//! Test: zeroclaw_cli_exec host function implemented.
//!
//! Task US-ZCL-55-1: Verifies acceptance criterion for US-ZCL-55:
//! > zeroclaw_cli_exec host function implemented
//!
//! These tests verify that the zeroclaw_cli_exec host function is properly
//! implemented and registered when a plugin has CLI capability enabled.

use std::sync::Arc;
use zeroclaw::config::AuditConfig;
use zeroclaw::memory::none::NoneMemory;
use zeroclaw::plugins::host_functions::{CliExecRequest, CliExecResponse, HostFunctionRegistry};
use zeroclaw::plugins::{CliCapability, PluginCapabilities, PluginManifest};
use zeroclaw::security::audit::AuditLogger;

/// Build a minimal `HostFunctionRegistry` backed by stubs.
fn make_registry() -> HostFunctionRegistry {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let memory = Arc::new(NoneMemory::new());
    let audit = Arc::new(
        AuditLogger::new(
            AuditConfig {
                enabled: false,
                ..Default::default()
            },
            tmp.path().to_path_buf(),
        )
        .expect("audit logger"),
    );
    HostFunctionRegistry::new(memory, vec![], audit)
}

/// Build a minimal `PluginManifest` with the given host capabilities.
fn manifest_with_caps(caps: PluginCapabilities) -> PluginManifest {
    let toml_str = r#"
[plugin]
name = "test-plugin"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#;
    let mut m = PluginManifest::parse(toml_str).unwrap();
    m.host_capabilities = caps;
    m
}

// ---------------------------------------------------------------------------
// Core acceptance criterion: zeroclaw_cli_exec host function is implemented
// ---------------------------------------------------------------------------

/// AC: zeroclaw_cli_exec host function is registered when CLI capability is present.
#[test]
fn cli_capability_registers_zeroclaw_cli_exec_function() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        cli: Some(CliCapability {
            allowed_commands: vec!["echo".to_string()],
            ..Default::default()
        }),
        ..Default::default()
    });

    let fns = registry.build_functions(&manifest);

    assert_eq!(
        fns.len(),
        1,
        "CLI capability should produce exactly 1 function"
    );
    assert_eq!(
        fns[0].name(),
        "zeroclaw_cli_exec",
        "CLI function must be named zeroclaw_cli_exec"
    );
}

/// AC: zeroclaw_cli_exec is not registered without CLI capability.
#[test]
fn no_cli_capability_no_zeroclaw_cli_exec_function() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities::default());

    let fns = registry.build_functions(&manifest);

    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();
    assert!(
        !names.contains(&"zeroclaw_cli_exec"),
        "zeroclaw_cli_exec should not be registered without CLI capability"
    );
}

/// AC: zeroclaw_cli_exec coexists with other host functions.
#[test]
fn cli_capability_coexists_with_other_capabilities() {
    use zeroclaw::plugins::MemoryCapability;

    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        memory: Some(MemoryCapability {
            read: true,
            write: false,
        }),
        cli: Some(CliCapability {
            allowed_commands: vec!["git".to_string()],
            ..Default::default()
        }),
        ..Default::default()
    });

    let fns = registry.build_functions(&manifest);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    // Should have memory_recall + cli_exec
    assert_eq!(fns.len(), 2, "memory read + cli should produce 2 functions");
    assert!(names.contains(&"zeroclaw_memory_recall"));
    assert!(names.contains(&"zeroclaw_cli_exec"));
}

// ---------------------------------------------------------------------------
// CliExecRequest struct verification
// ---------------------------------------------------------------------------

/// AC: CliExecRequest struct is properly exported and constructible.
#[test]
fn cli_exec_request_struct_exists() {
    let request = CliExecRequest {
        command: "echo".to_string(),
        args: vec!["hello".to_string()],
        working_dir: None,
        env: None,
    };

    assert_eq!(request.command, "echo");
    assert_eq!(request.args, vec!["hello"]);
    assert!(request.working_dir.is_none());
    assert!(request.env.is_none());
}

/// AC: CliExecRequest supports working_dir option.
#[test]
fn cli_exec_request_with_working_dir() {
    let request = CliExecRequest {
        command: "ls".to_string(),
        args: vec!["-la".to_string()],
        working_dir: Some("/tmp".to_string()),
        env: None,
    };

    assert_eq!(request.working_dir, Some("/tmp".to_string()));
}

/// AC: CliExecRequest supports env option.
#[test]
fn cli_exec_request_with_env() {
    use std::collections::HashMap;

    let mut env = HashMap::new();
    env.insert("PATH".to_string(), "/usr/bin".to_string());

    let request = CliExecRequest {
        command: "env".to_string(),
        args: vec![],
        working_dir: None,
        env: Some(env.clone()),
    };

    assert_eq!(request.env, Some(env));
}

// ---------------------------------------------------------------------------
// CliExecResponse struct verification
// ---------------------------------------------------------------------------

/// AC: CliExecResponse struct is properly exported and constructible.
#[test]
fn cli_exec_response_struct_exists() {
    let response = CliExecResponse {
        stdout: "output".to_string(),
        stderr: String::new(),
        exit_code: 0,
        truncated: false,
        timed_out: false,
    };

    assert_eq!(response.stdout, "output");
    assert_eq!(response.stderr, "");
    assert_eq!(response.exit_code, 0);
    assert!(!response.truncated);
    assert!(!response.timed_out);
}

/// AC: CliExecResponse captures all output fields.
#[test]
fn cli_exec_response_captures_all_fields() {
    let response = CliExecResponse {
        stdout: "standard output".to_string(),
        stderr: "error output".to_string(),
        exit_code: 1,
        truncated: true,
        timed_out: true,
    };

    assert_eq!(response.stdout, "standard output");
    assert_eq!(response.stderr, "error output");
    assert_eq!(response.exit_code, 1);
    assert!(response.truncated);
    assert!(response.timed_out);
}

// ---------------------------------------------------------------------------
// JSON serialization (required for host function communication)
// ---------------------------------------------------------------------------

/// AC: CliExecRequest serializes to JSON.
#[test]
fn cli_exec_request_serializes_to_json() {
    let request = CliExecRequest {
        command: "git".to_string(),
        args: vec!["status".to_string()],
        working_dir: Some("/repo".to_string()),
        env: None,
    };

    let json = serde_json::to_string(&request).expect("serialization must succeed");

    assert!(json.contains("\"command\":\"git\"") || json.contains("\"command\": \"git\""));
    assert!(json.contains("\"args\""));
    assert!(json.contains("\"working_dir\""));
}

/// AC: CliExecRequest deserializes from JSON.
#[test]
fn cli_exec_request_deserializes_from_json() {
    let json = r#"{
        "command": "npm",
        "args": ["install"],
        "working_dir": null,
        "env": null
    }"#;

    let request: CliExecRequest = serde_json::from_str(json).expect("deserialization must succeed");

    assert_eq!(request.command, "npm");
    assert_eq!(request.args, vec!["install"]);
    assert!(request.working_dir.is_none());
    assert!(request.env.is_none());
}

/// AC: CliExecResponse serializes to JSON.
#[test]
fn cli_exec_response_serializes_to_json() {
    let response = CliExecResponse {
        stdout: "done".to_string(),
        stderr: String::new(),
        exit_code: 0,
        truncated: false,
        timed_out: false,
    };

    let json = serde_json::to_string(&response).expect("serialization must succeed");

    assert!(json.contains("\"stdout\""));
    assert!(json.contains("\"stderr\""));
    assert!(json.contains("\"exit_code\""));
    assert!(json.contains("\"truncated\""));
    assert!(json.contains("\"timed_out\""));
}

/// AC: CliExecResponse deserializes from JSON.
#[test]
fn cli_exec_response_deserializes_from_json() {
    let json = r#"{
        "stdout": "hello world",
        "stderr": "",
        "exit_code": 0,
        "truncated": false,
        "timed_out": false
    }"#;

    let response: CliExecResponse =
        serde_json::from_str(json).expect("deserialization must succeed");

    assert_eq!(response.stdout, "hello world");
    assert_eq!(response.stderr, "");
    assert_eq!(response.exit_code, 0);
    assert!(!response.truncated);
    assert!(!response.timed_out);
}

// ---------------------------------------------------------------------------
// Round-trip serialization
// ---------------------------------------------------------------------------

/// AC: CliExecRequest roundtrips through JSON.
#[test]
fn cli_exec_request_json_roundtrip() {
    use std::collections::HashMap;

    let mut env = HashMap::new();
    env.insert("HOME".to_string(), "/home/user".to_string());

    let original = CliExecRequest {
        command: "cargo".to_string(),
        args: vec!["build".to_string(), "--release".to_string()],
        working_dir: Some("/project".to_string()),
        env: Some(env),
    };

    let json = serde_json::to_string(&original).expect("serialization must succeed");
    let restored: CliExecRequest =
        serde_json::from_str(&json).expect("deserialization must succeed");

    assert_eq!(original.command, restored.command);
    assert_eq!(original.args, restored.args);
    assert_eq!(original.working_dir, restored.working_dir);
    assert_eq!(original.env, restored.env);
}

/// AC: CliExecResponse roundtrips through JSON.
#[test]
fn cli_exec_response_json_roundtrip() {
    let original = CliExecResponse {
        stdout: "Compiling zeroclaw...".to_string(),
        stderr: "warning: unused variable".to_string(),
        exit_code: 0,
        truncated: false,
        timed_out: false,
    };

    let json = serde_json::to_string(&original).expect("serialization must succeed");
    let restored: CliExecResponse =
        serde_json::from_str(&json).expect("deserialization must succeed");

    assert_eq!(original.stdout, restored.stdout);
    assert_eq!(original.stderr, restored.stderr);
    assert_eq!(original.exit_code, restored.exit_code);
    assert_eq!(original.truncated, restored.truncated);
    assert_eq!(original.timed_out, restored.timed_out);
}
