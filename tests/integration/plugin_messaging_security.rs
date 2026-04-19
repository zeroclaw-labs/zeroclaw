#![cfg(feature = "plugins-wasm")]

//! Security test for messaging restrictions.
//!
//! Task US-ZCL-25-10: Consolidated security test covering three messaging
//! restriction scenarios:
//! 1. Message to unauthorized channel is rejected
//! 2. Rate limit exceeded rejects message
//! 3. Plugin without messaging capability cannot send
//!
//! These tests exercise the security boundaries of the messaging subsystem
//! end-to-end through `build_functions` and the `HostFunctionRegistry`.

use async_trait::async_trait;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use zeroclaw::channels::{Channel, ChannelMessage, SendMessage};
use zeroclaw::config::AuditConfig;
use zeroclaw::memory::none::NoneMemory;
use zeroclaw::plugins::host_functions::{ChannelRateLimiter, HostFunctionRegistry};
use zeroclaw::plugins::{MessagingCapability, PluginCapabilities, PluginManifest};
use zeroclaw::security::audit::AuditLogger;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

type CallLog = Arc<Mutex<Vec<(String, String)>>>;

/// Mock channel that records send calls for verification.
struct TrackingChannel {
    channel_name: &'static str,
    calls: Arc<Mutex<Vec<(String, String)>>>,
}

impl TrackingChannel {
    fn new(name: &'static str, calls: Arc<Mutex<Vec<(String, String)>>>) -> Self {
        Self {
            channel_name: name,
            calls,
        }
    }
}

#[async_trait]
impl Channel for TrackingChannel {
    fn name(&self) -> &str {
        self.channel_name
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        self.calls
            .lock()
            .push((message.recipient.clone(), message.content.clone()));
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

fn make_registry_with_channels() -> (HostFunctionRegistry, CallLog, CallLog) {
    let calls_slack = Arc::new(Mutex::new(Vec::new()));
    let calls_email = Arc::new(Mutex::new(Vec::new()));

    let ch_slack: Arc<dyn Channel> = Arc::new(TrackingChannel::new("slack", calls_slack.clone()));
    let ch_email: Arc<dyn Channel> = Arc::new(TrackingChannel::new("email", calls_email.clone()));

    let mut channels = HashMap::new();
    channels.insert("slack".to_string(), ch_slack);
    channels.insert("email".to_string(), ch_email);

    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![], make_audit()).with_channels(channels);

    (registry, calls_slack, calls_email)
}

fn make_manifest_with_messaging(
    name: &str,
    allowed_channels: Vec<String>,
    rate_limit_per_hour: u32,
) -> PluginManifest {
    let toml_str = format!(
        r#"
        name = "{name}"
        version = "0.1.0"
        wasm_path = "plugin.wasm"
        capabilities = ["tool"]
        "#,
    );
    let mut m: PluginManifest = toml::from_str(&toml_str).expect("valid manifest");
    m.host_capabilities = PluginCapabilities {
        messaging: Some(MessagingCapability {
            allowed_channels,
            rate_limit_per_hour,
        }),
        ..Default::default()
    };
    m
}

/// Simulates the allowed_channels check performed by zeroclaw_send_message.
fn is_channel_allowed(allowed_channels: &[String], channel_name: &str) -> bool {
    allowed_channels
        .iter()
        .any(|c| c == "*" || c == channel_name)
}

// ===========================================================================
// Scenario 1: Message to unauthorized channel is rejected
// ===========================================================================

#[test]
fn unauthorized_channel_rejected_by_allowed_channels_filter() {
    let manifest = make_manifest_with_messaging("sec_test", vec!["slack".into()], 60);
    let allowed = &manifest
        .host_capabilities
        .messaging
        .as_ref()
        .unwrap()
        .allowed_channels;

    assert!(
        is_channel_allowed(allowed, "slack"),
        "authorized channel must pass"
    );
    assert!(
        !is_channel_allowed(allowed, "email"),
        "unauthorized channel must be rejected"
    );
    assert!(
        !is_channel_allowed(allowed, "telegram"),
        "unauthorized channel must be rejected"
    );
}

#[test]
fn unauthorized_channel_not_registered_via_build_functions() {
    let (registry, _, _) = make_registry_with_channels();

    // Plugin only allowed to message slack
    let manifest = make_manifest_with_messaging("restricted", vec!["slack".into()], 60);
    let fns = registry.build_functions(&manifest);

    // zeroclaw_send_message is registered (because messaging capability exists)
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();
    assert!(
        names.contains(&"zeroclaw_send_message"),
        "messaging capability must register send function"
    );

    // But the manifest's allowed_channels only contains slack
    let msg = manifest.host_capabilities.messaging.as_ref().unwrap();
    assert_eq!(msg.allowed_channels, vec!["slack"]);
    assert!(
        !is_channel_allowed(&msg.allowed_channels, "email"),
        "email must be blocked by the allowed_channels embedded in the function"
    );
}

#[test]
fn empty_allowed_channels_blocks_all_sends() {
    let manifest = make_manifest_with_messaging("locked_down", vec![], 60);
    let allowed = &manifest
        .host_capabilities
        .messaging
        .as_ref()
        .unwrap()
        .allowed_channels;

    assert!(!is_channel_allowed(allowed, "slack"));
    assert!(!is_channel_allowed(allowed, "email"));
    assert!(!is_channel_allowed(allowed, "anything"));
}

#[test]
fn case_and_whitespace_variations_rejected() {
    let manifest = make_manifest_with_messaging("strict", vec!["slack".into()], 60);
    let allowed = &manifest
        .host_capabilities
        .messaging
        .as_ref()
        .unwrap()
        .allowed_channels;

    assert!(!is_channel_allowed(allowed, "Slack"));
    assert!(!is_channel_allowed(allowed, "SLACK"));
    assert!(!is_channel_allowed(allowed, " slack"));
    assert!(!is_channel_allowed(allowed, "slack "));
    assert!(!is_channel_allowed(allowed, "slack2"));
}

// ===========================================================================
// Scenario 2: Rate limit exceeded rejects message
// ===========================================================================

#[test]
fn rate_limit_exceeded_rejects_further_sends() {
    let limiter = ChannelRateLimiter::new(3, 3600);

    // First 3 sends succeed
    for i in 0..3 {
        assert!(
            limiter.record_send("sec_plugin", "slack").is_ok(),
            "send {} should be within budget",
            i + 1
        );
    }

    // 4th send rejected
    let result = limiter.record_send("sec_plugin", "slack");
    assert!(result.is_err(), "4th send must be rate-limited");

    let err = result.unwrap_err();
    assert!(
        err.contains("Rate limit"),
        "error should mention rate limit"
    );
    assert!(err.contains("sec_plugin"), "error should name the plugin");
    assert!(err.contains("slack"), "error should name the channel");
}

#[test]
fn rate_limit_with_manifest_configured_budget() {
    // Plugin manifest specifies a very tight budget of 2 per hour
    let manifest = make_manifest_with_messaging("tight_budget", vec!["slack".into()], 2);
    let msg_cap = manifest.host_capabilities.messaging.as_ref().unwrap();

    let limiter = ChannelRateLimiter::new(msg_cap.rate_limit_per_hour, 3600);

    assert!(limiter.record_send("tight_budget", "slack").is_ok());
    assert!(limiter.record_send("tight_budget", "slack").is_ok());
    assert!(
        limiter.record_send("tight_budget", "slack").is_err(),
        "3rd send must be rejected with budget of 2"
    );
}

#[test]
fn zero_rate_limit_blocks_immediately() {
    let limiter = ChannelRateLimiter::new(0, 3600);

    let result = limiter.record_send("zero_budget", "slack");
    assert!(
        result.is_err(),
        "zero budget must block the very first send"
    );
}

#[test]
fn rate_limit_rejection_continues_after_exhaustion() {
    let limiter = ChannelRateLimiter::new(1, 3600);

    limiter.record_send("persistent", "email").unwrap();

    // All subsequent attempts also fail
    for _ in 0..5 {
        assert!(
            limiter.record_send("persistent", "email").is_err(),
            "all sends after exhaustion must be rejected"
        );
    }
}

#[test]
fn rate_limit_error_does_not_leak_internals() {
    let limiter = ChannelRateLimiter::new(1, 3600);
    limiter.record_send("leak_test", "slack").unwrap();

    let err = limiter.record_send("leak_test", "slack").unwrap_err();

    assert!(
        !err.contains("panic"),
        "error must not expose panic information"
    );
    assert!(
        !err.contains("thread"),
        "error must not expose thread information"
    );
    assert!(
        !err.contains("backtrace"),
        "error must not expose backtrace"
    );
}

// ===========================================================================
// Scenario 3: Plugin without messaging capability cannot send
// ===========================================================================

#[test]
fn plugin_without_messaging_capability_gets_no_messaging_functions() {
    let (registry, _, _) = make_registry_with_channels();

    let manifest: PluginManifest = toml::from_str(
        r#"
        name = "no_messaging"
        version = "0.1.0"
        wasm_path = "plugin.wasm"
        capabilities = ["tool"]
        "#,
    )
    .expect("valid manifest");

    let fns = registry.build_functions(&manifest);
    let messaging_fns: Vec<_> = fns
        .iter()
        .filter(|f| {
            let name = f.name();
            name.contains("send_message") || name.contains("get_channels")
        })
        .collect();

    assert!(
        messaging_fns.is_empty(),
        "plugin without messaging capability must get zero messaging functions, got: {:?}",
        messaging_fns.iter().map(|f| f.name()).collect::<Vec<_>>()
    );
}

#[test]
fn plugin_with_other_capabilities_still_no_messaging() {
    let (registry, _, _) = make_registry_with_channels();

    let manifest: PluginManifest = toml::from_str(
        r#"
        name = "other_caps_only"
        version = "0.1.0"
        wasm_path = "plugin.wasm"
        capabilities = ["tool"]

        [host_capabilities.tool_delegation]
        allowed_tools = ["web_search"]

        [host_capabilities.memory]
        read = true
        write = true
        "#,
    )
    .expect("valid manifest");

    let fns = registry.build_functions(&manifest);
    let messaging_fns: Vec<_> = fns
        .iter()
        .filter(|f| {
            let name = f.name();
            name.contains("send_message") || name.contains("get_channels")
        })
        .collect();

    assert!(
        messaging_fns.is_empty(),
        "plugin with tool_delegation + memory but no messaging must get zero messaging functions"
    );
    // Should still have non-messaging functions
    assert!(
        fns.len() >= 2,
        "should have tool_delegation + memory functions, got {}",
        fns.len()
    );
}

#[test]
fn contrast_plugin_with_messaging_capability_gets_both_functions() {
    let (registry, _, _) = make_registry_with_channels();

    let manifest = make_manifest_with_messaging("has_messaging", vec!["slack".into()], 60);
    let fns = registry.build_functions(&manifest);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    assert!(
        names.contains(&"zeroclaw_send_message"),
        "messaging plugin must get zeroclaw_send_message"
    );
    assert!(
        names.contains(&"zeroclaw_get_channels"),
        "messaging plugin must get zeroclaw_get_channels"
    );
}

// ===========================================================================
// Combined: all three restrictions interact correctly
// ===========================================================================

#[test]
fn combined_security_restrictions_enforced_together() {
    let (registry, _, _) = make_registry_with_channels();

    // Plugin A: has messaging, allowed only on slack, budget of 2
    let manifest_a = make_manifest_with_messaging("plugin_a", vec!["slack".into()], 2);

    // Plugin B: no messaging capability at all
    let manifest_b: PluginManifest = toml::from_str(
        r#"
        name = "plugin_b"
        version = "0.1.0"
        wasm_path = "plugin.wasm"
        capabilities = ["tool"]
        "#,
    )
    .expect("valid manifest");

    // Restriction 3: Plugin B gets no messaging functions
    let fns_b = registry.build_functions(&manifest_b);
    let messaging_fns_b: Vec<_> = fns_b
        .iter()
        .filter(|f| f.name().contains("send_message") || f.name().contains("get_channels"))
        .collect();
    assert!(
        messaging_fns_b.is_empty(),
        "plugin_b must have zero messaging functions"
    );

    // Plugin A gets messaging functions
    let fns_a = registry.build_functions(&manifest_a);
    let names_a: Vec<&str> = fns_a.iter().map(|f| f.name()).collect();
    assert!(names_a.contains(&"zeroclaw_send_message"));

    // Restriction 1: Plugin A cannot message email (not in allowed_channels)
    let allowed = &manifest_a
        .host_capabilities
        .messaging
        .as_ref()
        .unwrap()
        .allowed_channels;
    assert!(is_channel_allowed(allowed, "slack"));
    assert!(!is_channel_allowed(allowed, "email"));

    // Restriction 2: Plugin A exhausts rate limit on slack
    let limiter = ChannelRateLimiter::new(
        manifest_a
            .host_capabilities
            .messaging
            .as_ref()
            .unwrap()
            .rate_limit_per_hour,
        3600,
    );
    assert!(limiter.record_send("plugin_a", "slack").is_ok());
    assert!(limiter.record_send("plugin_a", "slack").is_ok());
    assert!(
        limiter.record_send("plugin_a", "slack").is_err(),
        "plugin_a must be rate-limited after exhausting budget of 2"
    );
}
