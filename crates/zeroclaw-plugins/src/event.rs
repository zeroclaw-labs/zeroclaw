//! Host-owned routing for normalized events emitted by plugin channels.
//!
//! The plugin supplies message data, while the host supplies the admitted
//! instance identity and resolves the current route and authorization for each
//! submission. The dispatcher behind this boundary is responsible for entering
//! ZeroClaw's existing shared agent and SOP paths; this module deliberately
//! contains no transport listener or agent/SOP implementation.

use std::sync::Arc;

use async_trait::async_trait;
use thiserror::Error;
use zeroclaw_api::channel::ChannelMessage;

use crate::PluginCapability;
use crate::endpoint::PluginChannelEndpoint;
use crate::instance::PluginInstanceId;

/// A transient, host-stamped event submitted by one admitted plugin instance.
///
/// Construction is private so the instance identity can only come from the
/// host-owned [`PluginChannelEndpoint`]. The normalized message remains the
/// canonical payload passed into the shared channel lifecycle.
#[derive(Debug)]
pub struct PluginEventEnvelope {
    instance: PluginInstanceId,
    message: ChannelMessage,
}

impl PluginEventEnvelope {
    fn new(endpoint: &PluginChannelEndpoint, message: ChannelMessage) -> Self {
        Self {
            instance: endpoint.instance_id().clone(),
            message,
        }
    }

    /// Canonical logical instance that submitted this event.
    #[must_use]
    pub fn instance_id(&self) -> &PluginInstanceId {
        &self.instance
    }

    /// Normalized message to dispatch through the existing channel lifecycle.
    #[must_use]
    pub fn message(&self) -> &ChannelMessage {
        &self.message
    }

    /// Consume the envelope and return its normalized message.
    #[must_use]
    pub fn into_message(self) -> ChannelMessage {
        self.message
    }
}

/// A transient host-resolved view of where one event is allowed to go.
///
/// The route is bound to the exact canonical instance ID requested from the
/// live resolver. `agent` names the already-configured owning agent when an
/// agent turn is requested. `sop` is a typed marker for matching the event
/// against the shared SOP engine; a plugin cannot name a SOP or an action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedPluginEventRoute {
    instance: PluginInstanceId,
    agent: Option<Arc<str>>,
    sop: bool,
}

impl ResolvedPluginEventRoute {
    /// Route the event only to the owning agent's shared turn lifecycle.
    ///
    /// # Errors
    ///
    /// Returns [`PluginEventError::InvalidRoute`] for an empty or
    /// control-character-containing agent alias.
    pub fn agent(
        instance: &PluginInstanceId,
        agent_alias: impl Into<String>,
    ) -> Result<Self, PluginEventError> {
        Ok(Self {
            instance: instance.clone(),
            agent: Some(validate_agent_alias(agent_alias.into())?),
            sop: false,
        })
    }

    /// Route the event only to the shared SOP matcher.
    #[must_use]
    pub fn sop(instance: &PluginInstanceId) -> Self {
        Self {
            instance: instance.clone(),
            agent: None,
            sop: true,
        }
    }

    /// Route the event to both the owning agent and the shared SOP matcher.
    ///
    /// # Errors
    ///
    /// Returns [`PluginEventError::InvalidRoute`] for an empty or
    /// control-character-containing agent alias.
    pub fn sop_and_agent(
        instance: &PluginInstanceId,
        agent_alias: impl Into<String>,
    ) -> Result<Self, PluginEventError> {
        Ok(Self {
            instance: instance.clone(),
            agent: Some(validate_agent_alias(agent_alias.into())?),
            sop: true,
        })
    }

    /// Canonical instance for which this route was resolved.
    #[must_use]
    pub fn instance_id(&self) -> &PluginInstanceId {
        &self.instance
    }

    /// Owning agent selected by canonical host routing, when an agent turn is
    /// part of this route.
    #[must_use]
    pub fn agent_alias(&self) -> Option<&str> {
        self.agent.as_deref()
    }

    /// Whether the event must also enter the shared SOP matcher.
    #[must_use]
    pub fn routes_to_sop(&self) -> bool {
        self.sop
    }
}

fn validate_agent_alias(alias: String) -> Result<Arc<str>, PluginEventError> {
    if alias.is_empty() || alias.chars().any(char::is_control) {
        return Err(PluginEventError::InvalidRoute(
            "agent alias must not be empty or contain control characters".to_string(),
        ));
    }
    Ok(Arc::from(alias))
}

/// Result of resolving the current canonical route and sender authorization.
///
/// `Unknown` and `Denied` are separate fail-closed outcomes so the host can
/// observe whether a binding disappeared or a sender was rejected without
/// exposing either detail to plugin code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginEventResolution {
    /// The current canonical state authorizes this exact resolved route.
    Authorized(ResolvedPluginEventRoute),
    /// No active route owns this instance.
    Unknown,
    /// The current authorization policy rejects this event.
    Denied,
}

/// Live host resolver for plugin event ownership and sender authorization.
///
/// The closure is invoked once per submission. It should read canonical live
/// config at that point and return only a transient resolved view.
#[derive(Clone)]
pub struct PluginEventRouteResolver {
    resolve: Arc<ResolvePluginEventRoute>,
}

type ResolvePluginEventRoute = dyn Fn(&PluginInstanceId, &ChannelMessage) -> Result<PluginEventResolution, PluginEventError>
    + Send
    + Sync
    + 'static;

impl PluginEventRouteResolver {
    /// Build a live route and authorization resolver.
    #[must_use]
    pub fn new(
        resolve: impl Fn(
            &PluginInstanceId,
            &ChannelMessage,
        ) -> Result<PluginEventResolution, PluginEventError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            resolve: Arc::new(resolve),
        }
    }

    fn resolve(
        &self,
        instance: &PluginInstanceId,
        message: &ChannelMessage,
    ) -> Result<PluginEventResolution, PluginEventError> {
        (self.resolve)(instance, message)
    }
}

/// Shared runtime boundary that performs the already-resolved agent/SOP work.
///
/// Implementations must enter the existing shared channel turn and SOP event
/// paths. They must not copy either lifecycle into a plugin-specific adapter.
#[async_trait]
pub trait PluginEventDispatcher: Send + Sync {
    /// Dispatch one authorized event according to its typed resolved route.
    async fn dispatch(
        &self,
        route: ResolvedPluginEventRoute,
        event: PluginEventEnvelope,
    ) -> Result<(), PluginEventError>;
}

/// Resolves and submits plugin events through one fail-closed host boundary.
#[derive(Clone)]
pub struct PluginEventRouter {
    resolver: PluginEventRouteResolver,
    dispatcher: Arc<dyn PluginEventDispatcher>,
}

impl PluginEventRouter {
    /// Build a router from live resolution and shared dispatch services.
    #[must_use]
    pub fn new(
        resolver: PluginEventRouteResolver,
        dispatcher: Arc<dyn PluginEventDispatcher>,
    ) -> Self {
        Self {
            resolver,
            dispatcher,
        }
    }

    /// Resolve current ownership and authorization, then submit one event.
    ///
    /// # Errors
    ///
    /// Fails closed when the endpoint is not a channel capability, normalized
    /// message routing does not match the endpoint, no live route exists, live
    /// authorization denies the event, the resolver returns a route for a
    /// different instance, or the shared dispatcher fails.
    pub async fn submit(
        &self,
        endpoint: &PluginChannelEndpoint,
        message: ChannelMessage,
    ) -> Result<(), PluginEventError> {
        let instance = endpoint.instance_id();
        if instance.capability() != PluginCapability::Channel {
            return Err(PluginEventError::CapabilityMismatch);
        }
        if message.channel != endpoint.channel_type()
            || message.channel_alias.as_deref() != Some(endpoint.alias())
        {
            return Err(PluginEventError::RouteMismatch);
        }

        let route = match self.resolver.resolve(instance, &message)? {
            PluginEventResolution::Authorized(route) => route,
            PluginEventResolution::Unknown => return Err(PluginEventError::UnknownRoute),
            PluginEventResolution::Denied => return Err(PluginEventError::AccessDenied),
        };
        if route.instance_id() != instance {
            return Err(PluginEventError::RouteMismatch);
        }

        self.dispatcher
            .dispatch(route, PluginEventEnvelope::new(endpoint, message))
            .await
    }
}

/// Fail-closed outcomes from the plugin event-routing boundary.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PluginEventError {
    /// The submitted instance is not allowed to emit channel events.
    #[error("plugin capability cannot submit channel events")]
    CapabilityMismatch,
    /// The normalized event or resolved route belongs to another endpoint.
    #[error("plugin event route does not match its admitted instance")]
    RouteMismatch,
    /// No active route owns the submitted plugin instance.
    #[error("plugin event has no active host route")]
    UnknownRoute,
    /// Live host authorization rejected the submitted event.
    #[error("plugin event is not authorized")]
    AccessDenied,
    /// Canonical route state could not be materialized safely.
    #[error("invalid plugin event route: {0}")]
    InvalidRoute(String),
    /// The shared agent/SOP boundary could not accept the event.
    #[error("plugin event dispatch failed: {0}")]
    DispatchFailed(String),
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::instance::test_scope;

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct RecordedDispatch {
        instance: PluginInstanceId,
        agent: Option<String>,
        sop: bool,
        content: String,
    }

    #[derive(Default)]
    struct RecordingDispatcher {
        calls: Mutex<Vec<RecordedDispatch>>,
    }

    #[async_trait]
    impl PluginEventDispatcher for RecordingDispatcher {
        async fn dispatch(
            &self,
            route: ResolvedPluginEventRoute,
            event: PluginEventEnvelope,
        ) -> Result<(), PluginEventError> {
            self.calls
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(RecordedDispatch {
                    instance: event.instance_id().clone(),
                    agent: route.agent_alias().map(str::to_string),
                    sop: route.routes_to_sop(),
                    content: event.message().content.clone(),
                });
            Ok(())
        }
    }

    fn endpoint(package_binding: &str) -> PluginChannelEndpoint {
        let scope = test_scope(PluginCapability::Channel, package_binding, []);
        PluginChannelEndpoint::new(scope, "plugin").expect("valid endpoint")
    }

    fn message(alias: &str, sender: &str, content: &str) -> ChannelMessage {
        ChannelMessage {
            id: "event-1".to_string(),
            sender: sender.to_string(),
            reply_target: "room-1".to_string(),
            content: content.to_string(),
            channel: "plugin".to_string(),
            channel_alias: Some(alias.to_string()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn cross_instance_route_is_denied_before_dispatch() {
        let source = endpoint("source");
        let other = endpoint("other");
        let dispatcher = Arc::new(RecordingDispatcher::default());
        let resolver = PluginEventRouteResolver::new(move |_, _| {
            Ok(PluginEventResolution::Authorized(
                ResolvedPluginEventRoute::agent(other.instance_id(), "primary")?,
            ))
        });
        let router = PluginEventRouter::new(resolver, dispatcher.clone());

        let error = router
            .submit(&source, message("source", "test_user", "hello"))
            .await
            .expect_err("a route for another instance must fail closed");

        assert_eq!(error, PluginEventError::RouteMismatch);
        assert!(
            dispatcher
                .calls
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_empty()
        );
    }

    #[tokio::test]
    async fn unknown_route_is_denied_before_dispatch() {
        let source = endpoint("main");
        let dispatcher = Arc::new(RecordingDispatcher::default());
        let router = PluginEventRouter::new(
            PluginEventRouteResolver::new(|_, _| Ok(PluginEventResolution::Unknown)),
            dispatcher.clone(),
        );

        let error = router
            .submit(&source, message("main", "test_user", "hello"))
            .await
            .expect_err("an unowned instance must fail closed");

        assert_eq!(error, PluginEventError::UnknownRoute);
        assert!(
            dispatcher
                .calls
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_empty()
        );
    }

    #[tokio::test]
    async fn agent_route_enters_the_shared_dispatcher() {
        let source = endpoint("main");
        let dispatcher = Arc::new(RecordingDispatcher::default());
        let resolver = PluginEventRouteResolver::new(|instance, _| {
            Ok(PluginEventResolution::Authorized(
                ResolvedPluginEventRoute::agent(instance, "primary")?,
            ))
        });
        let router = PluginEventRouter::new(resolver, dispatcher.clone());

        router
            .submit(&source, message("main", "test_user", "hello agent"))
            .await
            .expect("authorized agent route dispatches");

        assert_eq!(
            dispatcher
                .calls
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .as_slice(),
            &[RecordedDispatch {
                instance: source.instance_id().clone(),
                agent: Some("primary".to_string()),
                sop: false,
                content: "hello agent".to_string(),
            }]
        );
    }

    #[tokio::test]
    async fn sop_route_enters_the_same_shared_dispatcher() {
        let source = endpoint("main");
        let dispatcher = Arc::new(RecordingDispatcher::default());
        let resolver = PluginEventRouteResolver::new(|instance, _| {
            Ok(PluginEventResolution::Authorized(
                ResolvedPluginEventRoute::sop(instance),
            ))
        });
        let router = PluginEventRouter::new(resolver, dispatcher.clone());

        router
            .submit(&source, message("main", "test_user", "hello sop"))
            .await
            .expect("authorized SOP route dispatches");

        assert_eq!(
            dispatcher
                .calls
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .as_slice(),
            &[RecordedDispatch {
                instance: source.instance_id().clone(),
                agent: None,
                sop: true,
                content: "hello sop".to_string(),
            }]
        );
    }

    #[tokio::test]
    async fn resolver_observes_live_route_and_authorization_changes() {
        #[derive(Clone)]
        enum LiveResolution {
            Agent(String),
            Denied,
            SopAndAgent(String),
        }

        let source = endpoint("main");
        let live = Arc::new(Mutex::new(LiveResolution::Agent("first".to_string())));
        let live_for_resolver = live.clone();
        let resolver = PluginEventRouteResolver::new(move |instance, _| {
            let current = live_for_resolver
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            match current {
                LiveResolution::Agent(agent) => Ok(PluginEventResolution::Authorized(
                    ResolvedPluginEventRoute::agent(instance, agent)?,
                )),
                LiveResolution::Denied => Ok(PluginEventResolution::Denied),
                LiveResolution::SopAndAgent(agent) => Ok(PluginEventResolution::Authorized(
                    ResolvedPluginEventRoute::sop_and_agent(instance, agent)?,
                )),
            }
        });
        let dispatcher = Arc::new(RecordingDispatcher::default());
        let router = PluginEventRouter::new(resolver, dispatcher.clone());

        router
            .submit(&source, message("main", "test_user", "first"))
            .await
            .expect("initial route dispatches");
        *live.lock().unwrap_or_else(|error| error.into_inner()) = LiveResolution::Denied;
        assert_eq!(
            router
                .submit(&source, message("main", "test_user", "denied"))
                .await,
            Err(PluginEventError::AccessDenied)
        );
        *live.lock().unwrap_or_else(|error| error.into_inner()) =
            LiveResolution::SopAndAgent("second".to_string());
        router
            .submit(&source, message("main", "test_user", "third"))
            .await
            .expect("updated route dispatches");

        let calls = dispatcher
            .calls
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert_eq!(calls.len(), 2, "denied submission never reaches dispatch");
        assert_eq!(calls[0].agent.as_deref(), Some("first"));
        assert!(!calls[0].sop);
        assert_eq!(calls[1].agent.as_deref(), Some("second"));
        assert!(calls[1].sop);
    }

    #[tokio::test]
    async fn normalized_message_cannot_select_another_route() {
        let source = endpoint("main");
        let dispatcher = Arc::new(RecordingDispatcher::default());
        let resolver = PluginEventRouteResolver::new(|instance, _| {
            Ok(PluginEventResolution::Authorized(
                ResolvedPluginEventRoute::agent(instance, "primary")?,
            ))
        });
        let router = PluginEventRouter::new(resolver, dispatcher.clone());

        let error = router
            .submit(&source, message("other", "test_user", "hello"))
            .await
            .expect_err("message alias cannot select another route");

        assert_eq!(error, PluginEventError::RouteMismatch);
        assert!(
            dispatcher
                .calls
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_empty()
        );
    }
}
