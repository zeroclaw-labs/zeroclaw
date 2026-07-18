//! Bounded handoff from the plugin host into the shared channel dispatcher.
//!
//! The queue is process-local coordination state. Each request owns one exact
//! host-resolved route, one host-stamped envelope, and one acknowledgement.
//! Canonical configuration and ownership stay outside this module and are
//! resolved before the request is enqueued.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};
use zeroclaw_plugins::event::{
    PluginEventDispatcher, PluginEventEnvelope, PluginEventError, ResolvedPluginEventRoute,
};

pub(crate) const PLUGIN_EVENT_QUEUE_CAPACITY: usize = 100;

pub(crate) struct PluginEventDispatchRequest {
    pub(crate) route: ResolvedPluginEventRoute,
    pub(crate) event: PluginEventEnvelope,
    acknowledgement: oneshot::Sender<Result<(), PluginEventError>>,
}

impl PluginEventDispatchRequest {
    #[cfg(test)]
    pub(crate) fn acknowledge(self, result: Result<(), PluginEventError>) {
        let _ = self.acknowledgement.send(result);
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        ResolvedPluginEventRoute,
        PluginEventEnvelope,
        PluginEventAcknowledgement,
    ) {
        (
            self.route,
            self.event,
            PluginEventAcknowledgement(self.acknowledgement),
        )
    }
}

pub(crate) struct PluginEventAcknowledgement(oneshot::Sender<Result<(), PluginEventError>>);

impl PluginEventAcknowledgement {
    pub(crate) fn send(self, result: Result<(), PluginEventError>) {
        let _ = self.0.send(result);
    }
}

#[derive(Clone)]
struct QueuedPluginEventDispatcher {
    sender: mpsc::Sender<PluginEventDispatchRequest>,
}

#[async_trait]
impl PluginEventDispatcher for QueuedPluginEventDispatcher {
    async fn dispatch(
        &self,
        route: ResolvedPluginEventRoute,
        event: PluginEventEnvelope,
    ) -> Result<(), PluginEventError> {
        let (acknowledgement, result) = oneshot::channel();
        self.sender
            .send(PluginEventDispatchRequest {
                route,
                event,
                acknowledgement,
            })
            .await
            .map_err(|_| {
                PluginEventError::DispatchFailed(
                    "shared channel dispatcher is not available".to_string(),
                )
            })?;
        result.await.map_err(|_| {
            PluginEventError::DispatchFailed(
                "shared channel dispatcher dropped the event acknowledgement".to_string(),
            )
        })?
    }
}

pub(crate) struct PluginEventDispatchReceiver {
    receiver: mpsc::Receiver<PluginEventDispatchRequest>,
}

impl PluginEventDispatchReceiver {
    pub(crate) async fn recv(&mut self) -> Option<PluginEventDispatchRequest> {
        self.receiver.recv().await
    }
}

pub(crate) fn bounded_plugin_event_dispatch()
-> (Arc<dyn PluginEventDispatcher>, PluginEventDispatchReceiver) {
    let (sender, receiver) = mpsc::channel(PLUGIN_EVENT_QUEUE_CAPACITY);
    (
        Arc::new(QueuedPluginEventDispatcher { sender }),
        PluginEventDispatchReceiver { receiver },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::channel::ChannelMessage;
    use zeroclaw_plugins::endpoint::PluginChannelEndpoint;
    use zeroclaw_plugins::event::{
        PluginEventResolution, PluginEventRouteResolver, PluginEventRouter,
    };
    use zeroclaw_plugins::instance::PluginInstanceScope;
    use zeroclaw_plugins::{PluginCapability, PluginManifest};

    fn endpoint() -> PluginChannelEndpoint {
        let manifest = PluginManifest {
            name: "queue-fixture".to_string(),
            version: "0.1.0".to_string(),
            description: None,
            author: None,
            wasm_path: None,
            wasm_sha256: None,
            capabilities: vec![PluginCapability::Channel],
            permissions: Vec::new(),
            config_schema: None,
            signature: None,
            publisher_key: None,
        };
        let scope =
            PluginInstanceScope::from_manifest(&manifest, PluginCapability::Channel, "main", [])
                .expect("admit queue fixture");
        PluginChannelEndpoint::new(scope, "plugin").expect("bind queue fixture")
    }

    #[tokio::test]
    async fn dispatcher_waits_for_the_exact_request_acknowledgement() {
        let (dispatcher, mut receiver) = bounded_plugin_event_dispatch();
        let endpoint = endpoint();
        let resolver = PluginEventRouteResolver::new(|instance, _| {
            Ok(PluginEventResolution::Authorized(
                ResolvedPluginEventRoute::agent(instance, "operator")?,
            ))
        });
        let router = PluginEventRouter::new(resolver, dispatcher);
        let message = ChannelMessage {
            sender: "allowed".to_string(),
            channel: "plugin".to_string(),
            channel_alias: Some("main".to_string()),
            ..ChannelMessage::default()
        };

        let submit = zeroclaw_spawn::spawn!(async move { router.submit(&endpoint, message).await });
        let request = receiver.recv().await.expect("one queued request");
        assert_eq!(request.route.agent_alias(), Some("operator"));
        assert_eq!(request.event.instance_id().binding(), "main");
        request.acknowledge(Ok(()));

        submit
            .await
            .expect("submit task joins")
            .expect("acknowledgement reaches submitter");
    }
}
