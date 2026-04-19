#![cfg(feature = "plugins-wasm")]

//! Test: Default limits applied when not specified in manifest.
//!
//! Task US-ZCL-56-4: Verifies acceptance criterion for US-ZCL-56:
//! > Default limits applied when not specified in manifest
//!
//! These tests verify that when a plugin manifest specifies CLI capability
//! without explicit limits, the system applies reasonable defaults for
//! timeout_ms, max_output_bytes, max_concurrent, and rate_limit_per_minute.

use zeroclaw::plugins::{
    CliCapability, DEFAULT_CLI_MAX_CONCURRENT, DEFAULT_CLI_MAX_OUTPUT_BYTES,
    DEFAULT_CLI_RATE_LIMIT_PER_MINUTE, DEFAULT_CLI_TIMEOUT_MS, PluginManifest,
};

// ---------------------------------------------------------------------------
// Core acceptance criterion: Default limits applied when not specified
// ---------------------------------------------------------------------------

/// Default timeout constant is 5 seconds (5000ms).
#[test]
fn default_timeout_constant_is_5_seconds() {
    assert_eq!(
        DEFAULT_CLI_TIMEOUT_MS, 5_000,
        "default timeout should be 5000ms (5 seconds)"
    );
}

/// Default max_output_bytes constant is 1 MiB.
#[test]
fn default_max_output_bytes_constant_is_1mib() {
    assert_eq!(
        DEFAULT_CLI_MAX_OUTPUT_BYTES, 1_048_576,
        "default max_output_bytes should be 1048576 (1 MiB)"
    );
}

/// Default max_concurrent constant is 2.
#[test]
fn default_max_concurrent_constant_is_2() {
    assert_eq!(
        DEFAULT_CLI_MAX_CONCURRENT, 2,
        "default max_concurrent should be 2"
    );
}

/// Default rate_limit_per_minute constant is 10.
#[test]
fn default_rate_limit_per_minute_constant_is_10() {
    assert_eq!(
        DEFAULT_CLI_RATE_LIMIT_PER_MINUTE, 10,
        "default rate_limit_per_minute should be 10"
    );
}

/// CliCapability::default() applies all default constants.
#[test]
fn cli_capability_default_applies_all_constants() {
    let cap = CliCapability::default();

    assert_eq!(cap.timeout_ms, DEFAULT_CLI_TIMEOUT_MS);
    assert_eq!(cap.max_output_bytes, DEFAULT_CLI_MAX_OUTPUT_BYTES);
    assert_eq!(cap.max_concurrent, DEFAULT_CLI_MAX_CONCURRENT);
    assert_eq!(cap.rate_limit_per_minute, DEFAULT_CLI_RATE_LIMIT_PER_MINUTE);
}

// ---------------------------------------------------------------------------
// Manifest parsing with defaults
// ---------------------------------------------------------------------------

/// Manifest with minimal CLI capability gets default limits.
#[test]
fn manifest_minimal_cli_gets_default_limits() {
    let toml_str = r#"
[plugin]
name = "minimal-cli"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]

[plugin.host_capabilities.cli]
allowed_commands = ["echo"]
"#;
    let manifest = PluginManifest::parse(toml_str).expect("manifest should parse");
    let cli = manifest
        .host_capabilities
        .cli
        .as_ref()
        .expect("cli capability should be present");

    // Specified field preserved
    assert_eq!(cli.allowed_commands, vec!["echo"]);

    // Defaults applied for unspecified limits
    assert_eq!(cli.timeout_ms, DEFAULT_CLI_TIMEOUT_MS);
    assert_eq!(cli.max_output_bytes, DEFAULT_CLI_MAX_OUTPUT_BYTES);
    assert_eq!(cli.max_concurrent, DEFAULT_CLI_MAX_CONCURRENT);
    assert_eq!(cli.rate_limit_per_minute, DEFAULT_CLI_RATE_LIMIT_PER_MINUTE);
}

/// Manifest with only allowed_commands gets defaults for all limits.
#[test]
fn manifest_only_commands_gets_all_defaults() {
    let toml_str = r#"
[plugin]
name = "commands-only"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]

[plugin.host_capabilities.cli]
allowed_commands = ["git", "npm"]
"#;
    let manifest = PluginManifest::parse(toml_str).expect("manifest should parse");
    let cli = manifest
        .host_capabilities
        .cli
        .as_ref()
        .expect("cli capability should be present");

    assert_eq!(cli.allowed_commands, vec!["git", "npm"]);
    assert_eq!(cli.timeout_ms, DEFAULT_CLI_TIMEOUT_MS);
    assert_eq!(cli.max_output_bytes, DEFAULT_CLI_MAX_OUTPUT_BYTES);
    assert_eq!(cli.max_concurrent, DEFAULT_CLI_MAX_CONCURRENT);
    assert_eq!(cli.rate_limit_per_minute, DEFAULT_CLI_RATE_LIMIT_PER_MINUTE);
}

/// Manifest specifying one limit gets defaults for others.
#[test]
fn manifest_partial_limits_gets_defaults_for_others() {
    let toml_str = r#"
[plugin]
name = "partial-limits"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]

[plugin.host_capabilities.cli]
allowed_commands = ["ls"]
timeout_ms = 30000
"#;
    let manifest = PluginManifest::parse(toml_str).expect("manifest should parse");
    let cli = manifest
        .host_capabilities
        .cli
        .as_ref()
        .expect("cli capability should be present");

    // Custom value preserved
    assert_eq!(cli.timeout_ms, 30_000);

    // Defaults applied for unspecified
    assert_eq!(cli.max_output_bytes, DEFAULT_CLI_MAX_OUTPUT_BYTES);
    assert_eq!(cli.max_concurrent, DEFAULT_CLI_MAX_CONCURRENT);
    assert_eq!(cli.rate_limit_per_minute, DEFAULT_CLI_RATE_LIMIT_PER_MINUTE);
}

/// Manifest specifying max_output_bytes only gets defaults for others.
#[test]
fn manifest_max_output_only_gets_defaults_for_others() {
    let toml_str = r#"
[plugin]
name = "output-only"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]

[plugin.host_capabilities.cli]
allowed_commands = ["cat"]
max_output_bytes = 2097152
"#;
    let manifest = PluginManifest::parse(toml_str).expect("manifest should parse");
    let cli = manifest
        .host_capabilities
        .cli
        .as_ref()
        .expect("cli capability should be present");

    // Custom value preserved
    assert_eq!(cli.max_output_bytes, 2_097_152);

    // Defaults applied for unspecified
    assert_eq!(cli.timeout_ms, DEFAULT_CLI_TIMEOUT_MS);
    assert_eq!(cli.max_concurrent, DEFAULT_CLI_MAX_CONCURRENT);
    assert_eq!(cli.rate_limit_per_minute, DEFAULT_CLI_RATE_LIMIT_PER_MINUTE);
}

/// Manifest specifying max_concurrent only gets defaults for others.
#[test]
fn manifest_max_concurrent_only_gets_defaults_for_others() {
    let toml_str = r#"
[plugin]
name = "concurrent-only"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]

[plugin.host_capabilities.cli]
allowed_commands = ["pwd"]
max_concurrent = 4
"#;
    let manifest = PluginManifest::parse(toml_str).expect("manifest should parse");
    let cli = manifest
        .host_capabilities
        .cli
        .as_ref()
        .expect("cli capability should be present");

    // Custom value preserved
    assert_eq!(cli.max_concurrent, 4);

    // Defaults applied for unspecified
    assert_eq!(cli.timeout_ms, DEFAULT_CLI_TIMEOUT_MS);
    assert_eq!(cli.max_output_bytes, DEFAULT_CLI_MAX_OUTPUT_BYTES);
    assert_eq!(cli.rate_limit_per_minute, DEFAULT_CLI_RATE_LIMIT_PER_MINUTE);
}

/// Manifest specifying rate_limit_per_minute only gets defaults for others.
#[test]
fn manifest_rate_limit_only_gets_defaults_for_others() {
    let toml_str = r#"
[plugin]
name = "rate-only"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]

[plugin.host_capabilities.cli]
allowed_commands = ["date"]
rate_limit_per_minute = 60
"#;
    let manifest = PluginManifest::parse(toml_str).expect("manifest should parse");
    let cli = manifest
        .host_capabilities
        .cli
        .as_ref()
        .expect("cli capability should be present");

    // Custom value preserved
    assert_eq!(cli.rate_limit_per_minute, 60);

    // Defaults applied for unspecified
    assert_eq!(cli.timeout_ms, DEFAULT_CLI_TIMEOUT_MS);
    assert_eq!(cli.max_output_bytes, DEFAULT_CLI_MAX_OUTPUT_BYTES);
    assert_eq!(cli.max_concurrent, DEFAULT_CLI_MAX_CONCURRENT);
}

/// Manifest with all limits specified uses no defaults.
#[test]
fn manifest_all_limits_specified_uses_no_defaults() {
    let toml_str = r#"
[plugin]
name = "all-custom"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]

[plugin.host_capabilities.cli]
allowed_commands = ["echo"]
timeout_ms = 60000
max_output_bytes = 4194304
max_concurrent = 8
rate_limit_per_minute = 120
"#;
    let manifest = PluginManifest::parse(toml_str).expect("manifest should parse");
    let cli = manifest
        .host_capabilities
        .cli
        .as_ref()
        .expect("cli capability should be present");

    // All custom values preserved, no defaults
    assert_eq!(cli.timeout_ms, 60_000);
    assert_eq!(cli.max_output_bytes, 4_194_304);
    assert_eq!(cli.max_concurrent, 8);
    assert_eq!(cli.rate_limit_per_minute, 120);

    // Confirm none match defaults
    assert_ne!(cli.timeout_ms, DEFAULT_CLI_TIMEOUT_MS);
    assert_ne!(cli.max_output_bytes, DEFAULT_CLI_MAX_OUTPUT_BYTES);
    assert_ne!(cli.max_concurrent, DEFAULT_CLI_MAX_CONCURRENT);
    assert_ne!(cli.rate_limit_per_minute, DEFAULT_CLI_RATE_LIMIT_PER_MINUTE);
}

// ---------------------------------------------------------------------------
// JSON deserialization with defaults
// ---------------------------------------------------------------------------

/// JSON with minimal fields gets defaults applied.
#[test]
fn json_minimal_cli_gets_defaults() {
    let json = r#"{
        "allowed_commands": ["test"]
    }"#;

    let cli: CliCapability = serde_json::from_str(json).expect("JSON should parse");

    assert_eq!(cli.allowed_commands, vec!["test"]);
    assert_eq!(cli.timeout_ms, DEFAULT_CLI_TIMEOUT_MS);
    assert_eq!(cli.max_output_bytes, DEFAULT_CLI_MAX_OUTPUT_BYTES);
    assert_eq!(cli.max_concurrent, DEFAULT_CLI_MAX_CONCURRENT);
    assert_eq!(cli.rate_limit_per_minute, DEFAULT_CLI_RATE_LIMIT_PER_MINUTE);
}

/// Empty JSON object gets all defaults.
#[test]
fn json_empty_object_gets_all_defaults() {
    let json = "{}";

    let cli: CliCapability = serde_json::from_str(json).expect("JSON should parse");

    assert!(cli.allowed_commands.is_empty());
    assert_eq!(cli.timeout_ms, DEFAULT_CLI_TIMEOUT_MS);
    assert_eq!(cli.max_output_bytes, DEFAULT_CLI_MAX_OUTPUT_BYTES);
    assert_eq!(cli.max_concurrent, DEFAULT_CLI_MAX_CONCURRENT);
    assert_eq!(cli.rate_limit_per_minute, DEFAULT_CLI_RATE_LIMIT_PER_MINUTE);
}

/// JSON with some limits gets defaults for others.
#[test]
fn json_partial_limits_gets_defaults() {
    let json = r#"{
        "allowed_commands": ["run"],
        "timeout_ms": 10000,
        "max_concurrent": 1
    }"#;

    let cli: CliCapability = serde_json::from_str(json).expect("JSON should parse");

    // Custom values
    assert_eq!(cli.timeout_ms, 10_000);
    assert_eq!(cli.max_concurrent, 1);

    // Defaults for unspecified
    assert_eq!(cli.max_output_bytes, DEFAULT_CLI_MAX_OUTPUT_BYTES);
    assert_eq!(cli.rate_limit_per_minute, DEFAULT_CLI_RATE_LIMIT_PER_MINUTE);
}

// ---------------------------------------------------------------------------
// Default values are reasonable bounds
// ---------------------------------------------------------------------------

/// Default timeout is long enough for typical commands.
#[test]
fn default_timeout_is_reasonable_for_typical_commands() {
    // 5 seconds should cover most typical CLI operations
    // like git status, ls, echo, etc.
    const { assert!(DEFAULT_CLI_TIMEOUT_MS >= 1_000) };
    const { assert!(DEFAULT_CLI_TIMEOUT_MS <= 60_000) };
}

/// Default max_output_bytes prevents memory exhaustion.
#[test]
fn default_max_output_bytes_prevents_memory_exhaustion() {
    // 1 MiB is generous for command output but won't exhaust memory
    const { assert!(DEFAULT_CLI_MAX_OUTPUT_BYTES <= 10 * 1024 * 1024) };
    const { assert!(DEFAULT_CLI_MAX_OUTPUT_BYTES >= 64 * 1024) };
}

/// Default max_concurrent prevents fork bombs.
#[test]
fn default_max_concurrent_prevents_fork_bombs() {
    // Low concurrency limit prevents runaway process spawning
    const { assert!(DEFAULT_CLI_MAX_CONCURRENT >= 1) };
    const { assert!(DEFAULT_CLI_MAX_CONCURRENT <= 10) };
}

/// Default rate_limit_per_minute prevents abuse.
#[test]
fn default_rate_limit_prevents_abuse() {
    // Rate limiting prevents rapid-fire command execution
    const { assert!(DEFAULT_CLI_RATE_LIMIT_PER_MINUTE >= 1) };
    const { assert!(DEFAULT_CLI_RATE_LIMIT_PER_MINUTE <= 100) };
}

// ---------------------------------------------------------------------------
// Defaults are consistent across CliCapability and constants
// ---------------------------------------------------------------------------

/// CliCapability::default() matches all DEFAULT_CLI_* constants.
#[test]
fn cli_capability_default_matches_constants() {
    let cap = CliCapability::default();

    assert_eq!(
        cap.timeout_ms, DEFAULT_CLI_TIMEOUT_MS,
        "default().timeout_ms should match DEFAULT_CLI_TIMEOUT_MS"
    );
    assert_eq!(
        cap.max_output_bytes, DEFAULT_CLI_MAX_OUTPUT_BYTES,
        "default().max_output_bytes should match DEFAULT_CLI_MAX_OUTPUT_BYTES"
    );
    assert_eq!(
        cap.max_concurrent, DEFAULT_CLI_MAX_CONCURRENT,
        "default().max_concurrent should match DEFAULT_CLI_MAX_CONCURRENT"
    );
    assert_eq!(
        cap.rate_limit_per_minute, DEFAULT_CLI_RATE_LIMIT_PER_MINUTE,
        "default().rate_limit_per_minute should match DEFAULT_CLI_RATE_LIMIT_PER_MINUTE"
    );
}
