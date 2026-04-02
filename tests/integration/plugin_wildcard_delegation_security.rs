//! Integration test: wildcard tool delegation requires strict/paranoid approval.
//!
//! Task US-ZCL-24-5: Verify acceptance criterion for story US-ZCL-24:
//! > Wildcard delegation requires strict/paranoid approval
//!
//! These tests assert that:
//! 1. Relaxed mode allows wildcard delegation (`allowed_tools: ["*"]`)
//! 2. Default mode allows wildcard delegation (with warning)
//! 3. Strict mode rejects wildcard delegation
//! 4. Paranoid mode rejects wildcard delegation
//! 5. Explicit tool lists are allowed at all security levels
//! 6. Cross-level contrast: same wildcard delegation allowed/rejected by level

use std::collections::HashMap;
use std::path::PathBuf;

use zeroclaw::config::schema::{Config, PluginSecurityConfig};
use zeroclaw::plugins::loader::PluginLoader;
use zeroclaw::plugins::{PluginCapabilities, PluginManifest, ToolDelegationCapability};
use zeroclaw::security::SecurityPolicy;

// ── Helpers ────────────────────────────────────────────────────────────

fn config_with_level(level: &str, allowed_plugins: Vec<String>) -> Config {
    let mut config = Config::default();
    config.plugins.security = PluginSecurityConfig {
        signature_mode: "disabled".to_string(),
        network_security_level: level.to_string(),
        trusted_publisher_keys: vec![],
        allowed_plugins,
    };
    config
}

fn manifest_with_delegation(name: &str, allowed_tools: Vec<String>) -> PluginManifest {
    PluginManifest {
        name: name.to_string(),
        version: "0.1.0".to_string(),
        description: None,
        author: None,
        wasm_path: "plugin.wasm".to_string(),
        capabilities: vec![],
        permissions: vec![],
        allowed_hosts: vec![],
        allowed_paths: HashMap::new(),
        tools: vec![],
        config: HashMap::new(),
        wasi: false,
        timeout_ms: 5000,
        signature: None,
        publisher_key: None,
        host_capabilities: PluginCapabilities {
            tool_delegation: Some(ToolDelegationCapability { allowed_tools }),
            ..Default::default()
        },
    }
}

fn security_default() -> SecurityPolicy {
    SecurityPolicy {
        workspace_dir: PathBuf::from("."),
        ..SecurityPolicy::default()
    }
}

// ── 1. Relaxed mode allows wildcard delegation ─────────────────────────

#[test]
fn relaxed_allows_wildcard_delegation() {
    let config = config_with_level("relaxed", vec![]);
    let security = security_default();
    let loader = PluginLoader::new(&config, &security);

    let m = manifest_with_delegation("delegator", vec!["*".to_string()]);
    let result = loader.validate_security_policy(&m);

    assert!(
        result.is_ok(),
        "relaxed mode should allow wildcard delegation, but got: {:?}",
        result.unwrap_err()
    );
}

// ── 2. Default mode allows wildcard delegation (with warning) ──────────

#[test]
fn default_allows_wildcard_delegation() {
    let config = config_with_level("default", vec![]);
    let security = security_default();
    let loader = PluginLoader::new(&config, &security);

    let m = manifest_with_delegation("delegator", vec!["*".to_string()]);
    let result = loader.validate_security_policy(&m);

    assert!(
        result.is_ok(),
        "default mode should allow wildcard delegation (with warning), but got: {:?}",
        result.unwrap_err()
    );
}

// ── 3. Strict mode rejects wildcard delegation ─────────────────────────

#[test]
fn strict_rejects_wildcard_delegation() {
    let config = config_with_level("strict", vec![]);
    let security = security_default();
    let loader = PluginLoader::new(&config, &security);

    let m = manifest_with_delegation("delegator", vec!["*".to_string()]);
    let result = loader.validate_security_policy(&m);

    assert!(
        result.is_err(),
        "strict mode should reject wildcard delegation"
    );

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("wildcard tool delegation"),
        "error should mention wildcard tool delegation: {err_msg}"
    );
    assert!(
        err_msg.contains("delegator"),
        "error should name the plugin: {err_msg}"
    );
}

// ── 4. Paranoid mode rejects wildcard delegation ───────────────────────

#[test]
fn paranoid_rejects_wildcard_delegation() {
    // Plugin must be allowlisted in paranoid mode to reach the delegation check
    let config = config_with_level("paranoid", vec!["delegator".to_string()]);
    let security = security_default();
    let loader = PluginLoader::new(&config, &security);

    let m = manifest_with_delegation("delegator", vec!["*".to_string()]);
    let result = loader.validate_security_policy(&m);

    assert!(
        result.is_err(),
        "paranoid mode should reject wildcard delegation"
    );

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("wildcard tool delegation"),
        "error should mention wildcard tool delegation: {err_msg}"
    );
}

// ── 5. Explicit tool lists are allowed at all security levels ──────────

#[test]
fn explicit_tools_allowed_at_all_levels() {
    let security = security_default();
    let explicit_tools = vec!["echo".to_string(), "file_read".to_string()];

    for level in &["relaxed", "default", "strict"] {
        let config = config_with_level(level, vec![]);
        let loader = PluginLoader::new(&config, &security);
        let m = manifest_with_delegation("delegator", explicit_tools.clone());
        assert!(
            loader.validate_security_policy(&m).is_ok(),
            "{level} mode should allow explicit tool delegation, but got: {:?}",
            loader
                .validate_security_policy(&manifest_with_delegation(
                    "delegator",
                    explicit_tools.clone()
                ))
                .unwrap_err()
        );
    }

    // Paranoid also allows explicit tools (when plugin is allowlisted)
    let config = config_with_level("paranoid", vec!["delegator".to_string()]);
    let loader = PluginLoader::new(&config, &security);
    let m = manifest_with_delegation("delegator", explicit_tools);
    assert!(
        loader.validate_security_policy(&m).is_ok(),
        "paranoid mode should allow explicit tool delegation when plugin is allowlisted, but got: {:?}",
        loader.validate_security_policy(&manifest_with_delegation("delegator", vec!["echo".into()])).unwrap_err()
    );
}

// ── 6. Cross-level contrast ────────────────────────────────────────────

#[test]
fn wildcard_delegation_allowed_by_relaxed_default_rejected_by_strict_paranoid() {
    let security = security_default();
    let wildcard = vec!["*".to_string()];

    // Relaxed: allowed
    let config = config_with_level("relaxed", vec![]);
    let loader = PluginLoader::new(&config, &security);
    let m = manifest_with_delegation("test-plugin", wildcard.clone());
    assert!(
        loader.validate_security_policy(&m).is_ok(),
        "relaxed should allow wildcard delegation"
    );

    // Default: allowed
    let config = config_with_level("default", vec![]);
    let loader = PluginLoader::new(&config, &security);
    let m = manifest_with_delegation("test-plugin", wildcard.clone());
    assert!(
        loader.validate_security_policy(&m).is_ok(),
        "default should allow wildcard delegation"
    );

    // Strict: rejected
    let config = config_with_level("strict", vec![]);
    let loader = PluginLoader::new(&config, &security);
    let m = manifest_with_delegation("test-plugin", wildcard.clone());
    assert!(
        loader.validate_security_policy(&m).is_err(),
        "strict should reject wildcard delegation"
    );

    // Paranoid: rejected (plugin allowlisted so we reach the delegation check)
    let config = config_with_level("paranoid", vec!["test-plugin".to_string()]);
    let loader = PluginLoader::new(&config, &security);
    let m = manifest_with_delegation("test-plugin", wildcard);
    assert!(
        loader.validate_security_policy(&m).is_err(),
        "paranoid should reject wildcard delegation"
    );
}

// ── 7. Wildcard mixed with explicit tools still rejected ───────────────

#[test]
fn wildcard_mixed_with_explicit_tools_rejected_in_strict() {
    let config = config_with_level("strict", vec![]);
    let security = security_default();
    let loader = PluginLoader::new(&config, &security);

    let m = manifest_with_delegation(
        "delegator",
        vec!["echo".to_string(), "*".to_string(), "file_read".to_string()],
    );
    let result = loader.validate_security_policy(&m);

    assert!(
        result.is_err(),
        "strict mode should reject wildcard even when mixed with explicit tools"
    );
}

// ── 8. No delegation capability passes all levels ──────────────────────

#[test]
fn no_delegation_capability_passes_all_levels() {
    let security = security_default();

    for level in &["relaxed", "default", "strict"] {
        let config = config_with_level(level, vec![]);
        let loader = PluginLoader::new(&config, &security);

        let m = PluginManifest {
            name: "plain-plugin".to_string(),
            version: "0.1.0".to_string(),
            description: None,
            author: None,
            wasm_path: "plugin.wasm".to_string(),
            capabilities: vec![],
            permissions: vec![],
            allowed_hosts: vec![],
            allowed_paths: HashMap::new(),
            tools: vec![],
            config: HashMap::new(),
            wasi: false,
            timeout_ms: 5000,
            signature: None,
            publisher_key: None,
            host_capabilities: Default::default(),
        };
        assert!(
            loader.validate_security_policy(&m).is_ok(),
            "{level} mode should pass plugins without delegation capability"
        );
    }
}
