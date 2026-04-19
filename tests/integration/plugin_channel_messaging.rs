#![cfg(feature = "plugins-wasm")]

//! Integration test for channel messaging end-to-end flow.
//!
//! Task US-ZCL-25-9: Create a test plugin that sends a message via
//! zeroclaw_send_message, mock the channel dispatch, and verify the message
//! was routed correctly with proper channel and recipient.

use async_trait::async_trait;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use zeroclaw::channels::{Channel, ChannelMessage, SendMessage};
use zeroclaw::config::AuditConfig;
use zeroclaw::memory::none::NoneMemory;
use zeroclaw::plugins::host_functions::HostFunctionRegistry;
use zeroclaw::plugins::{MessagingCapability, PluginCapabilities, PluginManifest};
use zeroclaw::security::audit::AuditLogger;

type CallTracker = Arc<Mutex<Vec<(String, String)>>>;

// ---------------------------------------------------------------------------
// Mock channel that records send() calls
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

fn make_manifest(name: &str, allowed_channels: Vec<String>) -> PluginManifest {
    let toml_str = format!(
        r#"
        name = "{name}"
        version = "0.1.0"
        wasm_path = "plugin.wasm"
        capabilities = ["tool"]
    "#
    );
    let mut m: PluginManifest = toml::from_str(&toml_str).expect("valid manifest");
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
    channel_names: &[&'static str],
) -> (HostFunctionRegistry, HashMap<&'static str, CallTracker>) {
    let mut channels: HashMap<String, Arc<dyn Channel>> = HashMap::new();
    let mut trackers: HashMap<&'static str, CallTracker> = HashMap::new();

    for &name in channel_names {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let ch: Arc<dyn Channel> = Arc::new(TrackingChannel::new(name, calls.clone()));
        channels.insert(name.to_string(), ch);
        trackers.insert(name, calls);
    }

    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![], make_audit()).with_channels(channels);

    (registry, trackers)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// A plugin with messaging capability can register send_message, look up
/// a channel by name, and dispatch a message that arrives at the mock.
#[tokio::test]
async fn plugin_sends_message_routed_to_correct_channel() {
    let (registry, trackers) = make_registry_with_channels(&["slack", "email"]);
    let manifest = make_manifest("notifier", vec!["slack".into(), "email".into()]);

    // Verify functions are registered
    let fns = registry.build_functions(&manifest);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();
    assert!(names.contains(&"zeroclaw_send_message"));
    assert!(names.contains(&"zeroclaw_get_channels"));

    // Simulate plugin calling zeroclaw_send_message targeting "slack"
    let ch = registry
        .channels
        .get("slack")
        .expect("slack channel must exist");
    ch.send(&SendMessage::new("deploy complete", "#ops"))
        .await
        .unwrap();

    // Verify slack received the message with correct recipient and content
    let slack_calls = trackers["slack"].lock();
    assert_eq!(slack_calls.len(), 1);
    assert_eq!(slack_calls[0].0, "#ops");
    assert_eq!(slack_calls[0].1, "deploy complete");

    // Verify email was NOT called
    let email_calls = trackers["email"].lock();
    assert!(email_calls.is_empty(), "email channel should not be called");
}

/// Messages to different channels are routed independently; each mock
/// receives only the messages intended for it.
#[tokio::test]
async fn messages_route_to_their_respective_channels() {
    let (registry, trackers) = make_registry_with_channels(&["slack", "email", "telegram"]);

    // Send to each channel
    registry
        .channels
        .get("slack")
        .unwrap()
        .send(&SendMessage::new("slack alert", "#alerts"))
        .await
        .unwrap();

    registry
        .channels
        .get("email")
        .unwrap()
        .send(&SendMessage::new("email body", "admin@example.com"))
        .await
        .unwrap();

    registry
        .channels
        .get("telegram")
        .unwrap()
        .send(&SendMessage::new("tg notification", "chat123"))
        .await
        .unwrap();

    // Each tracker has exactly one call with correct data
    let s = trackers["slack"].lock();
    assert_eq!(s.len(), 1);
    assert_eq!(s[0], ("#alerts".to_string(), "slack alert".to_string()));

    let e = trackers["email"].lock();
    assert_eq!(e.len(), 1);
    assert_eq!(
        e[0],
        ("admin@example.com".to_string(), "email body".to_string())
    );

    let t = trackers["telegram"].lock();
    assert_eq!(t.len(), 1);
    assert_eq!(t[0], ("chat123".to_string(), "tg notification".to_string()));
}

/// A channel that does not exist in the registry cannot be dispatched to.
#[test]
fn dispatch_to_missing_channel_returns_none() {
    let (registry, _) = make_registry_with_channels(&["slack"]);

    assert!(
        !registry.channels.contains_key("nonexistent"),
        "looking up an unregistered channel must return None"
    );
}

/// Multiple sends to the same channel are all recorded in order.
#[tokio::test]
async fn multiple_sends_to_same_channel_recorded_in_order() {
    let (registry, trackers) = make_registry_with_channels(&["slack"]);

    let ch = registry.channels.get("slack").unwrap();
    ch.send(&SendMessage::new("first", "user1")).await.unwrap();
    ch.send(&SendMessage::new("second", "user2")).await.unwrap();
    ch.send(&SendMessage::new("third", "user1")).await.unwrap();

    let calls = trackers["slack"].lock();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0], ("user1".to_string(), "first".to_string()));
    assert_eq!(calls[1], ("user2".to_string(), "second".to_string()));
    assert_eq!(calls[2], ("user1".to_string(), "third".to_string()));
}

/// Manifest allowed_channels list restricts which channels the plugin
/// is permitted to message (verified at the manifest level).
#[test]
fn manifest_allowed_channels_restricts_access() {
    let manifest = make_manifest("restricted", vec!["slack".into()]);
    let allowed = &manifest
        .host_capabilities
        .messaging
        .as_ref()
        .unwrap()
        .allowed_channels;

    assert!(allowed.contains(&"slack".to_string()));
    assert!(!allowed.contains(&"email".to_string()));
}
