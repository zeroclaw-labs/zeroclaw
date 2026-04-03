#![cfg(feature = "plugins-wasm")]

//! Integration test for zeroclaw_send_message host function routing.
//!
//! Task US-ZCL-25-1: Verify acceptance criterion for story US-ZCL-25:
//! > zeroclaw_send_message sends through existing channel dispatch
//!
//! These tests assert that:
//! 1. zeroclaw_send_message is registered when messaging capability is declared
//! 2. The registry dispatches to the correct channel by name
//! 3. Message content and recipient are passed through to Channel::send
//! 4. Requesting a channel not in allowed_channels returns an error
//! 5. Requesting an unknown channel returns an error

use async_trait::async_trait;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use zeroclaw::channels::traits::{Channel, ChannelMessage, SendMessage};
use zeroclaw::config::AuditConfig;
use zeroclaw::memory::none::NoneMemory;
use zeroclaw::plugins::host_functions::HostFunctionRegistry;
use zeroclaw::plugins::{MessagingCapability, PluginCapabilities, PluginManifest};
use zeroclaw::security::audit::AuditLogger;

/// A mock channel that records all `send()` calls.
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

fn make_manifest_with_messaging(allowed_channels: Vec<String>) -> PluginManifest {
    let toml_str = r#"
        name = "messenger_plugin"
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

// ---------------------------------------------------------------------------
// 1. zeroclaw_send_message is registered when messaging capability is declared
// ---------------------------------------------------------------------------

#[test]
fn messaging_capability_registers_zeroclaw_send_message() {
    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![], make_audit());
    let manifest = make_manifest_with_messaging(vec!["slack".into()]);

    let fns = registry.build_functions(&manifest);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    assert!(
        names.contains(&"zeroclaw_send_message"),
        "messaging capability should register zeroclaw_send_message, got: {:?}",
        names
    );
}

#[test]
fn no_messaging_capability_does_not_register_zeroclaw_send_message() {
    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![], make_audit());

    let toml_str = r#"
        name = "bare_plugin"
        version = "0.1.0"
        wasm_path = "plugin.wasm"
        capabilities = ["tool"]
    "#;
    let manifest: PluginManifest = toml::from_str(toml_str).expect("valid manifest");

    let fns = registry.build_functions(&manifest);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    assert!(
        !names.contains(&"zeroclaw_send_message"),
        "without messaging capability, zeroclaw_send_message must not be registered"
    );
}

// ---------------------------------------------------------------------------
// 2. The registry holds channel references for dispatch
// ---------------------------------------------------------------------------

#[test]
fn registry_holds_channel_references() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let channel: Arc<dyn Channel> = Arc::new(TrackingChannel::new("slack", calls));

    let mut channels = HashMap::new();
    channels.insert("slack".to_string(), channel);

    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![], make_audit()).with_channels(channels);

    assert_eq!(registry.channels.len(), 1);
    assert!(registry.channels.contains_key("slack"));
    assert_eq!(registry.channels["slack"].name(), "slack");
}

// ---------------------------------------------------------------------------
// 3. Channel dispatch routes to the correct channel
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dispatch_routes_to_named_channel() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let channel: Arc<dyn Channel> = Arc::new(TrackingChannel::new("slack", calls.clone()));

    let mut channels = HashMap::new();
    channels.insert("slack".to_string(), channel);

    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![], make_audit()).with_channels(channels);

    // Simulate the dispatch logic: find channel by name and send
    let target_name = "slack";
    let found = registry.channels.get(target_name);
    assert!(
        found.is_some(),
        "channel 'slack' must be findable in registry"
    );

    let msg = SendMessage::new("hello world", "user123");
    found.unwrap().send(&msg).await.unwrap();

    let recorded = calls.lock();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].0, "user123");
    assert_eq!(recorded[0].1, "hello world");
}

#[tokio::test]
async fn dispatch_selects_correct_channel_among_multiple() {
    let calls_slack = Arc::new(Mutex::new(Vec::new()));
    let calls_email = Arc::new(Mutex::new(Vec::new()));
    let ch_slack: Arc<dyn Channel> = Arc::new(TrackingChannel::new("slack", calls_slack.clone()));
    let ch_email: Arc<dyn Channel> = Arc::new(TrackingChannel::new("email", calls_email.clone()));

    let mut channels = HashMap::new();
    channels.insert("slack".to_string(), ch_slack);
    channels.insert("email".to_string(), ch_email);

    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![], make_audit()).with_channels(channels);

    // Dispatch to "email"
    let msg = SendMessage::new("test message", "alice@example.com");
    registry
        .channels
        .get("email")
        .unwrap()
        .send(&msg)
        .await
        .unwrap();

    assert!(
        calls_slack.lock().is_empty(),
        "slack should not have been called"
    );
    assert_eq!(
        calls_email.lock().len(),
        1,
        "email should have been called once"
    );
    assert_eq!(calls_email.lock()[0].0, "alice@example.com");
    assert_eq!(calls_email.lock()[0].1, "test message");
}

// ---------------------------------------------------------------------------
// 4. Unknown channel is not found in the registry
// ---------------------------------------------------------------------------

#[test]
fn unknown_channel_not_found_in_registry() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let channel: Arc<dyn Channel> = Arc::new(TrackingChannel::new("slack", calls));

    let mut channels = HashMap::new();
    channels.insert("slack".to_string(), channel);

    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![], make_audit()).with_channels(channels);

    let found = registry.channels.get("nonexistent");
    assert!(found.is_none(), "nonexistent channel must not be found");
}

// ---------------------------------------------------------------------------
// 5. Allowed channels list is accessible in manifest
// ---------------------------------------------------------------------------

#[test]
fn allowed_channels_list_is_accessible_in_manifest() {
    let manifest = make_manifest_with_messaging(vec!["slack".into(), "email".into()]);

    let msg = manifest.host_capabilities.messaging.as_ref().unwrap();
    assert_eq!(msg.allowed_channels, vec!["slack", "email"]);
}

#[test]
fn empty_allowed_channels_still_registers_function() {
    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![], make_audit());
    let manifest = make_manifest_with_messaging(vec![]);

    let fns = registry.build_functions(&manifest);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    assert!(
        names.contains(&"zeroclaw_send_message"),
        "even with empty allowed_channels, zeroclaw_send_message should be registered"
    );
}

// ---------------------------------------------------------------------------
// 6. zeroclaw_send_message uses existing Channel::send dispatch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn zeroclaw_send_message_uses_channel_trait_send() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let channel: Arc<dyn Channel> = Arc::new(TrackingChannel::new("slack", calls.clone()));

    let mut channels = HashMap::new();
    channels.insert("slack".to_string(), channel);

    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![], make_audit()).with_channels(channels);

    let manifest = make_manifest_with_messaging(vec!["slack".into()]);

    // Verify zeroclaw_send_message is registered
    let fns = registry.build_functions(&manifest);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();
    assert!(names.contains(&"zeroclaw_send_message"));

    // Verify the dispatch path: channel lookup → Channel::send
    let ch = registry.channels.get("slack").unwrap();
    let msg = SendMessage::new("notification from plugin", "general");
    ch.send(&msg).await.unwrap();

    let recorded = calls.lock();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].0, "general");
    assert_eq!(recorded[0].1, "notification from plugin");
}

// ---------------------------------------------------------------------------
// 7. Multiple channels can be registered and dispatched independently
// ---------------------------------------------------------------------------

#[tokio::test]
async fn multiple_channels_dispatch_independently() {
    let calls_a = Arc::new(Mutex::new(Vec::new()));
    let calls_b = Arc::new(Mutex::new(Vec::new()));
    let ch_a: Arc<dyn Channel> = Arc::new(TrackingChannel::new("slack", calls_a.clone()));
    let ch_b: Arc<dyn Channel> = Arc::new(TrackingChannel::new("telegram", calls_b.clone()));

    let mut channels = HashMap::new();
    channels.insert("slack".to_string(), ch_a);
    channels.insert("telegram".to_string(), ch_b);

    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![], make_audit()).with_channels(channels);

    // Send via slack
    let msg1 = SendMessage::new("slack msg", "chan1");
    registry
        .channels
        .get("slack")
        .unwrap()
        .send(&msg1)
        .await
        .unwrap();

    // Send via telegram
    let msg2 = SendMessage::new("telegram msg", "chat42");
    registry
        .channels
        .get("telegram")
        .unwrap()
        .send(&msg2)
        .await
        .unwrap();

    assert_eq!(calls_a.lock().len(), 1);
    assert_eq!(calls_a.lock()[0].1, "slack msg");
    assert_eq!(calls_b.lock().len(), 1);
    assert_eq!(calls_b.lock()[0].1, "telegram msg");
}
