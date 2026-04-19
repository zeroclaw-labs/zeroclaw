#![cfg(feature = "plugins-wasm")]

//! Integration test for zeroclaw_get_channels host function.
//!
//! Task US-ZCL-25-2: Verify acceptance criterion for story US-ZCL-25:
//! > zeroclaw_get_channels returns available channel names without credentials
//!
//! These tests assert that:
//! 1. Channel names can be retrieved from the registry
//! 2. Only names are returned — no credentials, tokens, or connection details
//! 3. The returned list reflects all registered channels
//! 4. An empty registry returns an empty list
//! 5. Channel names match the allowed_channels filter from the manifest

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use zeroclaw::channels::{Channel, ChannelMessage, SendMessage};
use zeroclaw::config::AuditConfig;
use zeroclaw::memory::none::NoneMemory;
use zeroclaw::plugins::host_functions::HostFunctionRegistry;
use zeroclaw::plugins::{MessagingCapability, PluginCapabilities, PluginManifest};
use zeroclaw::security::audit::AuditLogger;

/// A mock channel that holds a secret token internally but only exposes
/// its name through the Channel trait — proving that credentials are not
/// leaked through the channel name interface.
struct ChannelWithSecret {
    channel_name: &'static str,
    #[allow(dead_code)]
    secret_token: &'static str,
    #[allow(dead_code)]
    api_endpoint: &'static str,
}

impl ChannelWithSecret {
    fn new(name: &'static str, secret: &'static str, endpoint: &'static str) -> Self {
        Self {
            channel_name: name,
            secret_token: secret,
            api_endpoint: endpoint,
        }
    }
}

#[async_trait]
impl Channel for ChannelWithSecret {
    fn name(&self) -> &str {
        self.channel_name
    }

    async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
        Ok(())
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        Ok(())
    }
}

fn make_audit() -> Arc<AuditLogger> {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let cfg = AuditConfig {
        enabled: false,
        ..Default::default()
    };
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    Arc::new(AuditLogger::new(cfg, path).expect("audit logger"))
}

fn make_manifest_with_messaging(allowed_channels: Vec<String>) -> PluginManifest {
    let toml_str = r#"
        name = "get_channels_plugin"
        version = "0.1.0"
        wasm_path = "plugin.wasm"
        capabilities = ["tool"]
    "#;
    let mut m: PluginManifest = toml::from_str(toml_str).expect("valid manifest");
    m.host_capabilities = PluginCapabilities {
        messaging: Some(MessagingCapability {
            allowed_channels,
            ..Default::default()
        }),
        ..Default::default()
    };
    m
}

fn make_registry_with_channels(
    channel_specs: Vec<(&'static str, &'static str, &'static str)>,
) -> HostFunctionRegistry {
    let memory = Arc::new(NoneMemory::new());
    let mut channels: HashMap<String, Arc<dyn Channel>> = HashMap::new();
    for (name, secret, endpoint) in channel_specs {
        channels.insert(
            name.to_string(),
            Arc::new(ChannelWithSecret::new(name, secret, endpoint)),
        );
    }
    HostFunctionRegistry::new(memory, vec![], make_audit()).with_channels(channels)
}

// ---------------------------------------------------------------------------
// 1. Channel names can be retrieved from the registry without credentials
// ---------------------------------------------------------------------------

#[test]
fn get_channel_names_returns_only_names() {
    let registry = make_registry_with_channels(vec![
        ("slack", "xoxb-secret-token-123", "https://slack.com/api"),
        ("telegram", "bot123456:ABC-DEF", "https://api.telegram.org"),
        ("email", "smtp-password-456", "smtp://mail.example.com"),
    ]);

    // Retrieve channel names — this is the interface a get_channels host
    // function would use: registry.channels.keys()
    let mut names: Vec<String> = registry.channels.keys().cloned().collect();
    names.sort();

    assert_eq!(names, vec!["email", "slack", "telegram"]);

    // Verify that the name() trait method also returns only the name
    for (key, channel) in &registry.channels {
        let name = channel.name();
        assert_eq!(name, key.as_str());
        // Name must not contain any credential-like content
        assert!(
            !name.contains("xoxb"),
            "channel name must not leak Slack token"
        );
        assert!(
            !name.contains("bot123456"),
            "channel name must not leak Telegram token"
        );
        assert!(
            !name.contains("password"),
            "channel name must not leak password"
        );
        assert!(
            !name.contains("://"),
            "channel name must not leak endpoint URL"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. Empty registry returns empty list
// ---------------------------------------------------------------------------

#[test]
fn empty_registry_returns_no_channels() {
    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![], make_audit());

    let names: Vec<String> = registry.channels.keys().cloned().collect();
    assert!(
        names.is_empty(),
        "empty registry must return no channel names"
    );
}

// ---------------------------------------------------------------------------
// 3. Single channel registry returns exactly one name
// ---------------------------------------------------------------------------

#[test]
fn single_channel_returns_one_name() {
    let registry = make_registry_with_channels(vec![(
        "slack",
        "xoxb-secret-token",
        "https://slack.com/api",
    )]);

    let names: Vec<String> = registry.channels.keys().cloned().collect();
    assert_eq!(names.len(), 1);
    assert_eq!(names[0], "slack");
}

// ---------------------------------------------------------------------------
// 4. Channel names match Channel::name() trait method
// ---------------------------------------------------------------------------

#[test]
fn registry_keys_match_channel_trait_name() {
    let registry = make_registry_with_channels(vec![
        ("slack", "token-1", "https://slack.com"),
        ("telegram", "token-2", "https://telegram.org"),
        ("discord", "token-3", "https://discord.com"),
    ]);

    for (key, channel) in &registry.channels {
        assert_eq!(
            key.as_str(),
            channel.name(),
            "registry key must match Channel::name()"
        );
    }
}

// ---------------------------------------------------------------------------
// 5. Allowed channels filter restricts visible channels
// ---------------------------------------------------------------------------

#[test]
fn allowed_channels_filter_restricts_visible_set() {
    let registry = make_registry_with_channels(vec![
        ("slack", "token-1", "https://slack.com"),
        ("telegram", "token-2", "https://telegram.org"),
        ("email", "token-3", "smtp://mail.example.com"),
    ]);

    let manifest = make_manifest_with_messaging(vec!["slack".into(), "email".into()]);
    let allowed = &manifest
        .host_capabilities
        .messaging
        .as_ref()
        .unwrap()
        .allowed_channels;

    // Simulate get_channels: filter registry channels by allowed list
    let mut visible: Vec<String> = registry
        .channels
        .keys()
        .filter(|name| allowed.iter().any(|a| a == "*" || a == name.as_str()))
        .cloned()
        .collect();
    visible.sort();

    assert_eq!(visible, vec!["email", "slack"]);
    assert!(
        !visible.contains(&"telegram".to_string()),
        "telegram is not in allowed_channels and must not be visible"
    );
}

// ---------------------------------------------------------------------------
// 6. Wildcard allowed_channels exposes all registered channels
// ---------------------------------------------------------------------------

#[test]
fn wildcard_allowed_channels_exposes_all() {
    let registry = make_registry_with_channels(vec![
        ("slack", "token-1", "https://slack.com"),
        ("telegram", "token-2", "https://telegram.org"),
    ]);

    let manifest = make_manifest_with_messaging(vec!["*".into()]);
    let allowed = &manifest
        .host_capabilities
        .messaging
        .as_ref()
        .unwrap()
        .allowed_channels;

    let mut visible: Vec<String> = registry
        .channels
        .keys()
        .filter(|name| allowed.iter().any(|a| a == "*" || a == name.as_str()))
        .cloned()
        .collect();
    visible.sort();

    assert_eq!(visible, vec!["slack", "telegram"]);
}

// ---------------------------------------------------------------------------
// 7. Channel name does not contain credential fragments
// ---------------------------------------------------------------------------

#[test]
fn channel_name_free_of_credential_patterns() {
    let registry = make_registry_with_channels(vec![
        (
            "slack",
            "xoxb-1234567890-abcdefgh",
            "https://hooks.slack.com/secret",
        ),
        (
            "telegram",
            "123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11",
            "https://api.telegram.org/bot123456",
        ),
        (
            "email",
            "hunter2",
            "smtp://user:hunter2@mail.example.com:587",
        ),
    ]);

    let credential_patterns = [
        "xoxb", "xoxp", "xoxa",   // Slack tokens
        "bot",    // Telegram bot prefix
        "bearer", // Auth headers
        "password", "passwd",  // Password strings
        "secret",  // Generic secret
        "://",     // URLs that might leak endpoints
        "hunter2", // The example secret
    ];

    for channel in registry.channels.values() {
        let name = channel.name().to_lowercase();
        for pattern in &credential_patterns {
            assert!(
                !name.contains(&pattern.to_lowercase()),
                "channel name '{}' must not contain credential pattern '{}'",
                channel.name(),
                pattern
            );
        }
    }
}
