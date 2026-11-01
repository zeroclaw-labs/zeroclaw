//! Cross-crate proof that a configured channel plugin reaches a real WASM
//! component with its exact host-owned logical alias.
//!
//! The per-crate tests cover admission planning and the channel adapter
//! separately. This one runs the whole activation path an operator actually
//! exercises: a `[channels.plugin.<alias>]` declaration plus an installed
//! package, in, and a live `Channel` backed by a compiled component, out.

#![cfg(feature = "plugins-wasm-cranelift")]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use zeroclaw_api::channel::{Channel, SendMessage};
use zeroclaw_api::webhook::{
    PluginWebhookOutcome, PluginWebhookOwner, PluginWebhookRegistryLease, PluginWebhookRequest,
    WebhookCancellation,
};
use zeroclaw_config::multi_agent::{PeerGroupConfig, PeerUsername};
use zeroclaw_config::providers::ChannelRef;
use zeroclaw_config::schema::{PluginChannelConfig, PluginEntryConfig};
use zeroclaw_infra::plugin_webhook::PluginWebhookIngress;
use zeroclaw_plugins::PluginCapability;
use zeroclaw_plugins::host::PluginHost;
use zeroclaw_plugins::instance::PluginInstanceScope;

#[path = "support/plugin_channel_fixture.rs"]
mod plugin_channel_fixture;
use plugin_channel_fixture::{
    ALIAS, PACKAGE, REPLY, ROUTE, SECRET, SECRET_HEADER, activation_config, install_fixture_package,
};

/// A webhook carrying the fixture's shared secret.
fn fixture_webhook(method: &str, query: &str, body: &[u8]) -> PluginWebhookRequest {
    PluginWebhookRequest::new(
        ROUTE,
        method,
        query,
        vec![(SECRET_HEADER.to_string(), SECRET.to_string())],
        body.to_vec(),
    )
    .expect("fixture request is within ingress bounds")
}

/// The configured `operations` fixture channel, admitted for the peer
/// `tester`, whose webhook route is published into `ingress`.
async fn operations_channel(
    plugins: &TempDir,
    ingress: &PluginWebhookIngress,
) -> (Arc<dyn Channel>, PluginWebhookRegistryLease) {
    let mut config = activation_config(plugins, ALIAS, "5");
    config.peer_groups.insert(
        format!("plugin-{ALIAS}"),
        PeerGroupConfig {
            channel: ChannelRef::new(format!("plugin.{ALIAS}")),
            external_peers: vec![PeerUsername::new("tester")],
            ..PeerGroupConfig::default()
        },
    );
    config
        .validate()
        .expect("the activation declaration is valid operator config");

    let webhook_generation = ingress.registry().start_generation();
    let channels = zeroclaw_runtime::plugin_runtime::configured_plugin_channels_with_webhooks(
        Arc::new(config),
        None,
        Some(&webhook_generation),
    )
    .await;
    assert_eq!(channels.len(), 1, "the configured fixture must construct");
    (Arc::clone(&channels[0]), webhook_generation)
}

#[tokio::test]
async fn configured_channel_reaches_real_guest_and_shared_listener_contract() {
    let plugins = install_fixture_package();
    let ingress = PluginWebhookIngress::new(300, 16);
    let (channel, _webhook_generation) = operations_channel(&plugins, &ingress).await;

    assert_eq!(channel.name(), "plugin");
    assert_eq!(
        channel.alias(),
        ALIAS,
        "the channel must carry the operator's alias, not the package name"
    );
    assert_eq!(channel.self_handle().as_deref(), Some("@fixture"));
    assert!(channel.health_check().await);
    // The strict guest accepts a send only when its content is the current
    // `{credential_epoch}:{api_token}` revision resolved at point of use.
    channel
        .send(&SendMessage::new(REPLY, "room"))
        .await
        .expect("the real guest accepts an outbound message");

    // The adapter owns its poll loop and must keep running until its receiver
    // goes away, which is the contract the shared supervisor relies on.
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let listener_channel = Arc::clone(&channel);
    let listener = zeroclaw_spawn::spawn!(async move { listener_channel.listen(tx).await });
    let route = ingress
        .registry()
        .get(ROUTE)
        .expect("validated guest route is published atomically");
    assert_eq!(
        route.owner(),
        &PluginWebhookOwner::new(PACKAGE, ALIAS),
        "the route is owned by the package and the operator's alias"
    );
    drop(route);
    assert_eq!(
        ingress.routes().routes(),
        [(ROUTE.to_string(), PluginWebhookOwner::new(PACKAGE, ALIAS))],
        "the route listing names the same owner"
    );
    assert_eq!(
        ingress
            .dispatch(
                fixture_webhook(
                    "POST",
                    "",
                    br#"{"id":"runtime-1","sender":"tester","reply_target":"room","content":"from webhook"}"#,
                ),
                &WebhookCancellation::new(),
            )
            .await,
        PluginWebhookOutcome::Ack
    );
    let message = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("webhook reaches shared channel receiver")
        .expect("listener remains connected");
    assert_eq!(message.id, "runtime-1");
    assert_eq!(message.content, "from webhook");
    assert_eq!(message.channel, "plugin");
    assert_eq!(message.channel_alias.as_deref(), Some(ALIAS));
    assert_eq!(
        ingress
            .dispatch(
                fixture_webhook("GET", "challenge=runtime-echo", b""),
                &WebhookCancellation::new(),
            )
            .await,
        PluginWebhookOutcome::Reply("challenge=runtime-echo".to_string())
    );
    assert!(
        rx.try_recv().is_err(),
        "challenge must not reach the agent queue"
    );
    assert!(
        !listener.is_finished(),
        "the real plugin listener must retain its polling lifecycle"
    );
    drop(rx);
    tokio::time::timeout(Duration::from_secs(5), listener)
        .await
        .expect("listener exits after its receiver closes")
        .expect("listener task joins cleanly")
        .expect("listener returns successfully");
}

#[tokio::test]
async fn a_repeated_message_id_is_delivered_once_through_the_core_ingress() {
    let plugins = install_fixture_package();
    let ingress = PluginWebhookIngress::new(300, 16);
    let (channel, _webhook_generation) = operations_channel(&plugins, &ingress).await;

    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let listener_channel = Arc::clone(&channel);
    let listener = zeroclaw_spawn::spawn!(async move { listener_channel.listen(tx).await });
    let body = br#"{"id":"dedup-1","sender":"tester","reply_target":"room","content":"once"}"#;
    assert_eq!(
        ingress
            .dispatch(
                fixture_webhook("POST", "", body),
                &WebhookCancellation::new()
            )
            .await,
        PluginWebhookOutcome::Ack
    );
    let message = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("the first delivery reaches the channel receiver")
        .expect("listener remains connected");
    assert_eq!(message.id, "dedup-1");

    // The worker skips a committed message ID before it acknowledges, so
    // nothing can be in flight once the second Ack returns.
    assert_eq!(
        ingress
            .dispatch(
                fixture_webhook("POST", "", body),
                &WebhookCancellation::new()
            )
            .await,
        PluginWebhookOutcome::Ack
    );
    assert!(
        rx.try_recv().is_err(),
        "a repeated message ID must not reach the agent queue again"
    );

    drop(rx);
    tokio::time::timeout(Duration::from_secs(5), listener)
        .await
        .expect("listener exits after its receiver closes")
        .expect("listener task joins cleanly")
        .expect("listener returns successfully");
}

/// The guest refuses any config other than `{"retry_count":5}`, so a wrong
/// operator value must surface as a construction failure that the loader
/// reports and skips — not as a half-configured live channel.
#[tokio::test]
async fn a_channel_whose_guest_rejects_its_config_is_not_activated() {
    let plugins = install_fixture_package();
    let config = activation_config(&plugins, ALIAS, "9");

    let channels =
        zeroclaw_runtime::plugin_runtime::configured_plugin_channels(Arc::new(config), None).await;

    assert!(
        channels.is_empty(),
        "a guest that refuses its configuration must not be registered"
    );
}

#[tokio::test]
async fn duplicate_guest_routes_reject_every_claimant_before_registry_mutation() {
    let plugins = install_fixture_package();
    let mut config = activation_config(&plugins, ALIAS, "5");
    config.plugins.max_active_instances = 2;
    config.channels.plugin.insert(
        "backup".to_string(),
        PluginChannelConfig {
            package: PACKAGE.to_string(),
            enabled: true,
            ..PluginChannelConfig::default()
        },
    );
    config
        .agents
        .get_mut("operator")
        .expect("operator agent")
        .channels
        .push(ChannelRef::new("plugin.backup"));

    let host = PluginHost::from_plugins_dir(plugins.path()).expect("admit fixture package");
    let manifest = host
        .manifest(PACKAGE)
        .expect("fixture manifest is admitted");
    let scope = PluginInstanceScope::from_manifest(
        manifest,
        PluginCapability::Channel,
        "backup",
        manifest.permissions.iter().copied(),
    )
    .expect("admit backup channel scope");
    config.plugins.entries.push(PluginEntryConfig {
        name: scope
            .id()
            .config_entry_key()
            .expect("derive backup config key"),
        config: HashMap::from([
            ("retry_count".to_string(), "5".to_string()),
            ("credential_epoch".to_string(), "v1".to_string()),
            ("api_token".to_string(), "backup-secret".to_string()),
        ]),
        ..PluginEntryConfig::default()
    });

    let ingress = PluginWebhookIngress::new(300, 16);
    let webhook_generation = ingress.registry().start_generation();
    let channels = zeroclaw_runtime::plugin_runtime::configured_plugin_channels_with_webhooks(
        Arc::new(config),
        None,
        Some(&webhook_generation),
    )
    .await;

    assert!(
        channels.is_empty(),
        "both instances advertise the same fixture route and must both be rejected"
    );
    assert!(
        ingress.registry().get(ROUTE).is_none(),
        "claim resolution must finish before one partial winner mutates the registry"
    );
}

/// An installed, enabled, correctly configured package still must not activate
/// when no enabled agent routes to its alias.
#[tokio::test]
async fn a_channel_without_an_enabled_owner_is_not_activated() {
    let plugins = install_fixture_package();
    let mut config = activation_config(&plugins, ALIAS, "5");
    config.agents.get_mut("operator").unwrap().enabled = false;

    let channels =
        zeroclaw_runtime::plugin_runtime::configured_plugin_channels(Arc::new(config), None).await;

    assert!(
        channels.is_empty(),
        "an orphaned declaration must stay inert"
    );
}
