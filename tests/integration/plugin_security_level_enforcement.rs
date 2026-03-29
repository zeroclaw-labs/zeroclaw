//! Integration test: validate_security_policy enforces all four security levels
//! through the PluginLoader entry point with mock Config and SecurityPolicy.
//!
//! Tests the combined behaviour of paranoid allowlisting, wildcard host
//! validation, forbidden path checks, and workspace path restrictions as
//! orchestrated by `PluginLoader::validate_security_policy`.

use std::collections::HashMap;
use std::path::PathBuf;

use zeroclaw::config::schema::{Config, PluginSecurityConfig};
use zeroclaw::plugins::loader::PluginLoader;
use zeroclaw::plugins::PluginManifest;
use zeroclaw::security::SecurityPolicy;

// ── Helpers ────────────────────────────────────────────────────────────

/// Build a Config with the given security level and allowed plugins list.
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

/// Build a minimal PluginManifest with the given name, hosts, and paths.
fn manifest(
    name: &str,
    allowed_hosts: Vec<String>,
    allowed_paths: HashMap<String, String>,
) -> PluginManifest {
    PluginManifest {
        name: name.to_string(),
        version: "0.1.0".to_string(),
        description: None,
        author: None,
        wasm_path: "plugin.wasm".to_string(),
        capabilities: vec![],
        permissions: vec![],
        allowed_hosts,
        allowed_paths,
        tools: vec![],
        config: HashMap::new(),
        wasi: false,
        timeout_ms: 5000,
        signature: None,
        publisher_key: None,
    }
}

/// Build a SecurityPolicy with the given workspace directory.
fn security_with_workspace(workspace: PathBuf) -> SecurityPolicy {
    SecurityPolicy {
        workspace_dir: workspace,
        ..SecurityPolicy::default()
    }
}

// ── Relaxed mode ───────────────────────────────────────────────────────

#[test]
fn relaxed_allows_wildcard_hosts_through_loader() {
    let config = config_with_level("relaxed", vec![]);
    let security = security_with_workspace(PathBuf::from("."));
    let loader = PluginLoader::new(&config, &security);

    let m = manifest("wildcard-plugin", vec!["*.example.com".to_string()], HashMap::new());
    let result = loader.validate_security_policy(&m);

    assert!(
        result.is_ok(),
        "relaxed mode should allow wildcard hosts, but got: {:?}",
        result.unwrap_err()
    );
}

#[test]
fn relaxed_allows_bare_star_host_through_loader() {
    let config = config_with_level("relaxed", vec![]);
    let security = security_with_workspace(PathBuf::from("."));
    let loader = PluginLoader::new(&config, &security);

    let m = manifest("star-plugin", vec!["*".to_string()], HashMap::new());
    let result = loader.validate_security_policy(&m);

    assert!(
        result.is_ok(),
        "relaxed mode should allow bare '*' host, but got: {:?}",
        result.unwrap_err()
    );
}

#[test]
fn relaxed_does_not_enforce_plugin_allowlist() {
    let config = config_with_level("relaxed", vec!["other-plugin".to_string()]);
    let security = security_with_workspace(PathBuf::from("."));
    let loader = PluginLoader::new(&config, &security);

    let m = manifest("unlisted-plugin", vec![], HashMap::new());
    let result = loader.validate_security_policy(&m);

    assert!(
        result.is_ok(),
        "relaxed mode should not enforce the plugin allowlist"
    );
}

#[test]
fn relaxed_still_rejects_forbidden_paths() {
    let config = config_with_level("relaxed", vec![]);
    let security = security_with_workspace(PathBuf::from("."));
    let loader = PluginLoader::new(&config, &security);

    let mut paths = HashMap::new();
    paths.insert("/secrets".to_string(), "/etc/shadow".to_string());
    let m = manifest("bad-path-plugin", vec![], paths);
    let result = loader.validate_security_policy(&m);

    assert!(
        result.is_err(),
        "relaxed mode should still reject forbidden paths like /etc"
    );
}

// ── Default mode ───────────────────────────────────────────────────────

#[test]
fn default_allows_wildcard_hosts_through_loader() {
    let config = config_with_level("default", vec![]);
    let security = security_with_workspace(PathBuf::from("."));
    let loader = PluginLoader::new(&config, &security);

    let m = manifest(
        "wildcard-plugin",
        vec!["*.example.com".to_string(), "*".to_string()],
        HashMap::new(),
    );
    let result = loader.validate_security_policy(&m);

    assert!(
        result.is_ok(),
        "default mode should allow wildcard hosts (with warning), but got: {:?}",
        result.unwrap_err()
    );
}

#[test]
fn default_does_not_enforce_workspace_restriction() {
    let workspace = std::env::temp_dir().join("zeroclaw_test_default_ws");
    std::fs::create_dir_all(&workspace).ok();

    let config = config_with_level("default", vec![]);
    let security = security_with_workspace(workspace.clone());
    let loader = PluginLoader::new(&config, &security);

    let mut paths = HashMap::new();
    paths.insert("/data".to_string(), "/tmp/outside".to_string());
    let m = manifest("outside-plugin", vec![], paths);
    let result = loader.validate_security_policy(&m);

    assert!(
        result.is_ok(),
        "default mode should not restrict paths to the workspace subtree, but got: {:?}",
        result.unwrap_err()
    );

    std::fs::remove_dir_all(&workspace).ok();
}

#[test]
fn default_does_not_enforce_allowlist() {
    let config = config_with_level("default", vec!["trusted-only".to_string()]);
    let security = security_with_workspace(PathBuf::from("."));
    let loader = PluginLoader::new(&config, &security);

    let m = manifest("rogue-plugin", vec![], HashMap::new());
    let result = loader.validate_security_policy(&m);

    assert!(
        result.is_ok(),
        "default mode should not enforce the plugin allowlist"
    );
}

#[test]
fn default_still_rejects_forbidden_paths() {
    let config = config_with_level("default", vec![]);
    let security = security_with_workspace(PathBuf::from("."));
    let loader = PluginLoader::new(&config, &security);

    let mut paths = HashMap::new();
    paths.insert("/key".to_string(), "/root/.secret".to_string());
    let m = manifest("root-plugin", vec![], paths);
    let result = loader.validate_security_policy(&m);

    assert!(
        result.is_err(),
        "default mode should reject forbidden paths like /root"
    );
}

// ── Strict mode ────────────────────────────────────────────────────────

#[test]
fn strict_rejects_wildcard_hosts_through_loader() {
    let config = config_with_level("strict", vec![]);
    let security = security_with_workspace(PathBuf::from("."));
    let loader = PluginLoader::new(&config, &security);

    let m = manifest("wildcard-plugin", vec!["*.example.com".to_string()], HashMap::new());
    let result = loader.validate_security_policy(&m);

    assert!(result.is_err(), "strict mode should reject wildcard hosts");

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("*.example.com"),
        "error should name the rejected host: {err_msg}"
    );
}

#[test]
fn strict_allows_explicit_hosts_through_loader() {
    let config = config_with_level("strict", vec![]);
    let security = security_with_workspace(PathBuf::from("."));
    let loader = PluginLoader::new(&config, &security);

    let m = manifest(
        "explicit-plugin",
        vec!["api.example.com".to_string()],
        HashMap::new(),
    );
    let result = loader.validate_security_policy(&m);

    assert!(
        result.is_ok(),
        "strict mode should allow explicit hosts, but got: {:?}",
        result.unwrap_err()
    );
}

#[test]
fn strict_rejects_paths_outside_workspace() {
    let workspace = std::env::temp_dir().join("zeroclaw_test_strict_enforce_ws");
    std::fs::create_dir_all(&workspace).ok();

    let config = config_with_level("strict", vec![]);
    let security = security_with_workspace(workspace.clone());
    let loader = PluginLoader::new(&config, &security);

    let mut paths = HashMap::new();
    paths.insert("/data".to_string(), "/tmp/outside".to_string());
    let m = manifest("escape-plugin", vec![], paths);
    let result = loader.validate_security_policy(&m);

    assert!(
        result.is_err(),
        "strict mode should reject paths outside the workspace"
    );

    std::fs::remove_dir_all(&workspace).ok();
}

#[test]
fn strict_allows_paths_inside_workspace() {
    let workspace = std::env::temp_dir().join("zeroclaw_test_strict_enforce_inside");
    let inner = workspace.join("data");
    std::fs::create_dir_all(&inner).ok();

    let config = config_with_level("strict", vec![]);
    let security = security_with_workspace(workspace.clone());
    let loader = PluginLoader::new(&config, &security);

    let mut paths = HashMap::new();
    paths.insert("/data".to_string(), inner.to_string_lossy().into_owned());
    let m = manifest("inner-plugin", vec![], paths);
    let result = loader.validate_security_policy(&m);

    assert!(
        result.is_ok(),
        "strict mode should allow paths inside the workspace, but got: {:?}",
        result.unwrap_err()
    );

    std::fs::remove_dir_all(&workspace).ok();
}

#[test]
fn strict_does_not_enforce_allowlist() {
    let config = config_with_level("strict", vec!["other".to_string()]);
    let security = security_with_workspace(PathBuf::from("."));
    let loader = PluginLoader::new(&config, &security);

    let m = manifest("unlisted-plugin", vec![], HashMap::new());
    let result = loader.validate_security_policy(&m);

    assert!(
        result.is_ok(),
        "strict mode should not enforce the plugin allowlist"
    );
}

#[test]
fn strict_still_rejects_forbidden_paths() {
    let workspace = std::env::temp_dir().join("zeroclaw_test_strict_forbidden");
    std::fs::create_dir_all(&workspace).ok();

    let config = config_with_level("strict", vec![]);
    let security = security_with_workspace(workspace.clone());
    let loader = PluginLoader::new(&config, &security);

    let mut paths = HashMap::new();
    paths.insert("/secrets".to_string(), "/proc/self/maps".to_string());
    let m = manifest("proc-plugin", vec![], paths);
    let result = loader.validate_security_policy(&m);

    assert!(
        result.is_err(),
        "strict mode should reject forbidden paths like /proc"
    );

    std::fs::remove_dir_all(&workspace).ok();
}

// ── Paranoid mode ──────────────────────────────────────────────────────

#[test]
fn paranoid_rejects_unlisted_plugin_through_loader() {
    let config = config_with_level("paranoid", vec!["trusted-plugin".to_string()]);
    let security = security_with_workspace(PathBuf::from("."));
    let loader = PluginLoader::new(&config, &security);

    let m = manifest("rogue-plugin", vec![], HashMap::new());
    let result = loader.validate_security_policy(&m);

    assert!(
        result.is_err(),
        "paranoid mode should reject plugins not on the allowlist"
    );

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("rogue-plugin"),
        "error should name the rejected plugin: {err_msg}"
    );
}

#[test]
fn paranoid_allows_listed_plugin_through_loader() {
    let workspace = std::env::temp_dir().join("zeroclaw_test_paranoid_allow");
    let inner = workspace.join("data");
    std::fs::create_dir_all(&inner).ok();

    let config = config_with_level("paranoid", vec!["trusted-plugin".to_string()]);
    let security = security_with_workspace(workspace.clone());
    let loader = PluginLoader::new(&config, &security);

    let mut paths = HashMap::new();
    paths.insert("/data".to_string(), inner.to_string_lossy().into_owned());
    let m = manifest("trusted-plugin", vec!["api.example.com".to_string()], paths);
    let result = loader.validate_security_policy(&m);

    assert!(
        result.is_ok(),
        "paranoid mode should allow plugins on the allowlist with valid hosts and paths, but got: {:?}",
        result.unwrap_err()
    );

    std::fs::remove_dir_all(&workspace).ok();
}

#[test]
fn paranoid_rejects_wildcard_hosts_through_loader() {
    let config = config_with_level("paranoid", vec!["my-plugin".to_string()]);
    let security = security_with_workspace(PathBuf::from("."));
    let loader = PluginLoader::new(&config, &security);

    let m = manifest("my-plugin", vec!["*.example.com".to_string()], HashMap::new());
    let result = loader.validate_security_policy(&m);

    assert!(
        result.is_err(),
        "paranoid mode should reject wildcard hosts even for allowlisted plugins"
    );
}

#[test]
fn paranoid_rejects_paths_outside_workspace() {
    let workspace = std::env::temp_dir().join("zeroclaw_test_paranoid_ws");
    std::fs::create_dir_all(&workspace).ok();

    let config = config_with_level("paranoid", vec!["esc-plugin".to_string()]);
    let security = security_with_workspace(workspace.clone());
    let loader = PluginLoader::new(&config, &security);

    let mut paths = HashMap::new();
    paths.insert("/data".to_string(), "/tmp/outside".to_string());
    let m = manifest("esc-plugin", vec![], paths);
    let result = loader.validate_security_policy(&m);

    assert!(
        result.is_err(),
        "paranoid mode should reject paths outside the workspace"
    );

    std::fs::remove_dir_all(&workspace).ok();
}

#[test]
fn paranoid_with_empty_allowlist_rejects_all() {
    let config = config_with_level("paranoid", vec![]);
    let security = security_with_workspace(PathBuf::from("."));
    let loader = PluginLoader::new(&config, &security);

    let m = manifest("any-plugin", vec![], HashMap::new());
    let result = loader.validate_security_policy(&m);

    assert!(
        result.is_err(),
        "paranoid mode with empty allowlist should reject every plugin"
    );
}

// ── Cross-level contrast tests ─────────────────────────────────────────

#[test]
fn same_wildcard_host_allowed_by_relaxed_and_default_rejected_by_strict_and_paranoid() {
    let hosts = vec!["*.example.com".to_string()];
    let security = security_with_workspace(PathBuf::from("."));

    // Relaxed: allowed
    let config = config_with_level("relaxed", vec![]);
    let loader = PluginLoader::new(&config, &security);
    let m = manifest("test-plugin", hosts.clone(), HashMap::new());
    assert!(
        loader.validate_security_policy(&m).is_ok(),
        "relaxed should allow wildcard hosts"
    );

    // Default: allowed (with warning)
    let config = config_with_level("default", vec![]);
    let loader = PluginLoader::new(&config, &security);
    let m = manifest("test-plugin", hosts.clone(), HashMap::new());
    assert!(
        loader.validate_security_policy(&m).is_ok(),
        "default should allow wildcard hosts"
    );

    // Strict: rejected
    let config = config_with_level("strict", vec![]);
    let loader = PluginLoader::new(&config, &security);
    let m = manifest("test-plugin", hosts.clone(), HashMap::new());
    assert!(
        loader.validate_security_policy(&m).is_err(),
        "strict should reject wildcard hosts"
    );

    // Paranoid: rejected (would also fail allowlist, but wildcard check is after)
    let config = config_with_level("paranoid", vec!["test-plugin".to_string()]);
    let loader = PluginLoader::new(&config, &security);
    let m = manifest("test-plugin", hosts, HashMap::new());
    assert!(
        loader.validate_security_policy(&m).is_err(),
        "paranoid should reject wildcard hosts"
    );
}

#[test]
fn outside_workspace_path_allowed_by_relaxed_and_default_rejected_by_strict_and_paranoid() {
    let workspace = std::env::temp_dir().join("zeroclaw_test_cross_level_ws");
    std::fs::create_dir_all(&workspace).ok();

    let mut paths = HashMap::new();
    paths.insert("/data".to_string(), "/tmp/somewhere_else".to_string());

    // Relaxed: allowed (no workspace restriction)
    let config = config_with_level("relaxed", vec![]);
    let security = security_with_workspace(workspace.clone());
    let loader = PluginLoader::new(&config, &security);
    let m = manifest("test-plugin", vec![], paths.clone());
    assert!(
        loader.validate_security_policy(&m).is_ok(),
        "relaxed should not restrict workspace paths"
    );

    // Default: allowed (no workspace restriction)
    let config = config_with_level("default", vec![]);
    let loader = PluginLoader::new(&config, &security);
    let m = manifest("test-plugin", vec![], paths.clone());
    assert!(
        loader.validate_security_policy(&m).is_ok(),
        "default should not restrict workspace paths"
    );

    // Strict: rejected
    let config = config_with_level("strict", vec![]);
    let loader = PluginLoader::new(&config, &security);
    let m = manifest("test-plugin", vec![], paths.clone());
    assert!(
        loader.validate_security_policy(&m).is_err(),
        "strict should reject paths outside workspace"
    );

    // Paranoid: rejected
    let config = config_with_level("paranoid", vec!["test-plugin".to_string()]);
    let loader = PluginLoader::new(&config, &security);
    let m = manifest("test-plugin", vec![], paths);
    assert!(
        loader.validate_security_policy(&m).is_err(),
        "paranoid should reject paths outside workspace"
    );

    std::fs::remove_dir_all(&workspace).ok();
}

#[test]
fn allowlist_enforced_only_in_paranoid() {
    let security = security_with_workspace(PathBuf::from("."));

    for level in &["relaxed", "default", "strict"] {
        let config = config_with_level(level, vec!["other-plugin".to_string()]);
        let loader = PluginLoader::new(&config, &security);
        let m = manifest("unlisted-plugin", vec![], HashMap::new());
        assert!(
            loader.validate_security_policy(&m).is_ok(),
            "{level} mode should not enforce the plugin allowlist"
        );
    }

    let config = config_with_level("paranoid", vec!["other-plugin".to_string()]);
    let loader = PluginLoader::new(&config, &security);
    let m = manifest("unlisted-plugin", vec![], HashMap::new());
    assert!(
        loader.validate_security_policy(&m).is_err(),
        "paranoid mode should enforce the plugin allowlist"
    );
}

// ── Config-driven level selection ──────────────────────────────────────

#[test]
fn unknown_security_level_falls_back_to_default_behaviour() {
    let config = config_with_level("banana", vec![]);
    let security = security_with_workspace(PathBuf::from("."));
    let loader = PluginLoader::new(&config, &security);

    // Default allows wildcards (with warning) — so should "banana"
    let m = manifest("test-plugin", vec!["*.example.com".to_string()], HashMap::new());
    let result = loader.validate_security_policy(&m);

    assert!(
        result.is_ok(),
        "unknown security level should fall back to default, which allows wildcards"
    );
}

#[test]
fn empty_security_level_falls_back_to_default_behaviour() {
    let config = config_with_level("", vec![]);
    let security = security_with_workspace(PathBuf::from("."));
    let loader = PluginLoader::new(&config, &security);

    let m = manifest("test-plugin", vec!["*.example.com".to_string()], HashMap::new());
    let result = loader.validate_security_policy(&m);

    assert!(
        result.is_ok(),
        "empty security level should fall back to default"
    );
}

#[test]
fn case_insensitive_level_selection() {
    let security = security_with_workspace(PathBuf::from("."));

    // "STRICT" should behave like "strict"
    let config = config_with_level("STRICT", vec![]);
    let loader = PluginLoader::new(&config, &security);
    let m = manifest("test-plugin", vec!["*.example.com".to_string()], HashMap::new());
    assert!(
        loader.validate_security_policy(&m).is_err(),
        "STRICT (uppercase) should reject wildcard hosts"
    );

    // "Paranoid" should behave like "paranoid"
    let config = config_with_level("Paranoid", vec![]);
    let loader = PluginLoader::new(&config, &security);
    let m = manifest("any-plugin", vec![], HashMap::new());
    assert!(
        loader.validate_security_policy(&m).is_err(),
        "Paranoid (mixed case) should enforce allowlist"
    );
}
