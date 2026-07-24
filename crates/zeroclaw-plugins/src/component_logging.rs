//! Host-side `logging` and `types` implementations for all three plugin worlds.

use zeroclaw_log::{Action, Event, EventOutcome, record};

use crate::component::PluginState;
use crate::component::bindings;
use crate::instance::PluginInstanceId;

fn plugin_log_attrs(
    instance: &PluginInstanceId,
    fn_name: String,
    raw_attrs: Option<String>,
) -> serde_json::Value {
    let mut attrs = serde_json::json!({
        "plugin": instance.package(),
        "plugin_capability": instance.capability(),
        "plugin_binding": instance.binding(),
        "plugin_fn": fn_name,
    });
    if let Some(raw) = raw_attrs {
        attrs["raw"] = serde_json::Value::String(raw);
    }
    attrs
}

fn do_log_record(
    instance: &PluginInstanceId,
    level_idx: u8,
    fn_name: String,
    action: Action,
    outcome: EventOutcome,
    duration_ms: Option<u64>,
    raw_attrs: Option<String>,
    msg: String,
) {
    let mut ev = Event::new(module_path!(), action).with_outcome(outcome);
    if let Some(ms) = duration_ms {
        ev = ev.with_duration(ms);
    }
    ev = ev.with_attrs(plugin_log_attrs(instance, fn_name, raw_attrs));
    match level_idx {
        0 => record!(TRACE, ev, msg),
        1 => record!(DEBUG, ev, msg),
        2 => record!(INFO, ev, msg),
        3 => record!(WARN, ev, msg),
        _ => record!(ERROR, ev, msg),
    }
}

macro_rules! impl_host {
    ($world:ident) => {
        impl bindings::$world::zeroclaw::plugin::types::Host for PluginState {}

        impl bindings::$world::zeroclaw::plugin::logging::Host for PluginState {
            async fn log_record(
                &mut self,
                level: bindings::$world::zeroclaw::plugin::logging::LogLevel,
                event: bindings::$world::zeroclaw::plugin::logging::PluginEvent,
            ) {
                if !self.charge_host_call() {
                    return;
                }
                use bindings::$world::zeroclaw::plugin::logging::{
                    LogLevel, PluginAction, PluginOutcome,
                };
                let action = match event.action {
                    PluginAction::Start => Action::Start,
                    PluginAction::Complete => Action::Complete,
                    PluginAction::Fail => Action::Fail,
                    PluginAction::Cancel => Action::Cancel,
                    PluginAction::Skip => Action::Skip,
                    PluginAction::Timeout => Action::Timeout,
                    PluginAction::Retry => Action::Retry,
                    PluginAction::Inbound => Action::Inbound,
                    PluginAction::Outbound => Action::Outbound,
                    PluginAction::Send => Action::Send,
                    PluginAction::Receive => Action::Receive,
                    PluginAction::Connect => Action::Connect,
                    PluginAction::Disconnect => Action::Disconnect,
                    PluginAction::Reconnect => Action::Reconnect,
                    PluginAction::Spawn => Action::Spawn,
                    PluginAction::Kill => Action::Kill,
                    PluginAction::Tick => Action::Tick,
                    PluginAction::Trigger => Action::Trigger,
                    PluginAction::Schedule => Action::Schedule,
                    PluginAction::Approve => Action::Approve,
                    PluginAction::Reject => Action::Reject,
                    PluginAction::Defer => Action::Defer,
                    PluginAction::Read => Action::Read,
                    PluginAction::Write => Action::Write,
                    PluginAction::Delete => Action::Delete,
                    PluginAction::ListAction => Action::List,
                    PluginAction::Query => Action::Query,
                    PluginAction::Invoke => Action::Invoke,
                    PluginAction::Dispatch => Action::Dispatch,
                    PluginAction::Resolve => Action::Resolve,
                    PluginAction::Register => Action::Register,
                    PluginAction::Unregister => Action::Unregister,
                    PluginAction::Load => Action::Load,
                    PluginAction::Save => Action::Save,
                    PluginAction::Migrate => Action::Migrate,
                    PluginAction::Validate => Action::Validate,
                    PluginAction::MemoryAudit => Action::MemoryAudit,
                    PluginAction::Note => Action::Note,
                };
                let outcome = match event.outcome {
                    Some(PluginOutcome::Success) => EventOutcome::Success,
                    Some(PluginOutcome::Failure) => EventOutcome::Failure,
                    None => EventOutcome::Unknown,
                };
                let level_idx = match level {
                    LogLevel::Trace => 0,
                    LogLevel::Debug => 1,
                    LogLevel::Info => 2,
                    LogLevel::Warn => 3,
                    LogLevel::Error => 4,
                };
                do_log_record(
                    self.scope().id(),
                    level_idx,
                    event.function_name,
                    action,
                    outcome,
                    event.duration_ms,
                    event.attrs,
                    event.message,
                );
            }
        }
    };
}

impl_host!(tool);
impl_host!(channel);
impl_host!(memory);

impl bindings::channel::zeroclaw::plugin::inbound::Host for PluginState {
    async fn inbound_poll(
        &mut self,
    ) -> Option<bindings::channel::zeroclaw::plugin::inbound::HostInboundMessage> {
        if !self.charge_host_call() {
            return None;
        }
        self.inbound().poll().map(|m| {
            bindings::channel::zeroclaw::plugin::inbound::HostInboundMessage {
                id: m.id,
                sender: m.sender,
                reply_target: m.reply_target,
                content: m.content,
                channel: m.channel,
                channel_alias: m.channel_alias,
                timestamp: m.timestamp,
                thread_ts: m.thread_ts,
                interruption_scope_id: m.interruption_scope_id,
                subject: m.subject,
            }
        })
    }

    async fn inbound_pending(&mut self) -> u32 {
        if !self.charge_host_call() {
            return 0;
        }
        self.inbound().pending()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PluginCapability;

    const LOGGING_WIT: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../wit/v0/logging.wit"
    ));

    #[test]
    fn wit_plugin_actions_cover_log_action_taxonomy() {
        let (_, after_enum) = LOGGING_WIT
            .split_once("enum plugin-action {")
            .expect("logging WIT must define plugin-action");
        let (action_body, _) = after_enum
            .split_once('}')
            .expect("plugin-action must have a closing brace");

        macro_rules! assert_actions {
            ($( $variant:ident => $wit_name:literal ),+ $(,)?) => {
                fn wit_name(action: Action) -> &'static str {
                    match action {
                        $(Action::$variant => $wit_name),+
                    }
                }

                $(
                    let name = wit_name(Action::$variant);
                    assert!(
                        action_body
                            .lines()
                            .any(|line| line.trim() == concat!($wit_name, ",")),
                        "plugin-action is missing {name}"
                    );
                )+
            };
        }

        assert_actions!(
            Start => "start",
            Complete => "complete",
            Fail => "fail",
            Cancel => "cancel",
            Skip => "skip",
            Timeout => "timeout",
            Retry => "retry",
            Inbound => "inbound",
            Outbound => "outbound",
            Send => "send",
            Receive => "receive",
            Connect => "connect",
            Disconnect => "disconnect",
            Reconnect => "reconnect",
            Spawn => "spawn",
            Kill => "kill",
            Tick => "tick",
            Trigger => "trigger",
            Schedule => "schedule",
            Approve => "approve",
            Reject => "reject",
            Defer => "defer",
            Read => "read",
            Write => "write",
            Delete => "delete",
            List => "list-action",
            Query => "query",
            Invoke => "invoke",
            Dispatch => "dispatch",
            Resolve => "resolve",
            Register => "register",
            Unregister => "unregister",
            Load => "load",
            Save => "save",
            Migrate => "migrate",
            Validate => "validate",
            MemoryAudit => "memory-audit",
            Note => "note",
        );
    }

    #[test]
    fn host_log_attributes_are_issued_from_the_instance_identity() {
        let scope = crate::instance::test_scope(PluginCapability::Channel, "support", []);
        let attrs = plugin_log_attrs(scope.id(), "poll".to_string(), Some("guest".to_string()));

        assert_eq!(attrs["plugin"], "fixture");
        assert_eq!(attrs["plugin_capability"], "channel");
        assert_eq!(attrs["plugin_binding"], "support");
        assert_eq!(attrs["plugin_fn"], "poll");
        assert_eq!(attrs["raw"], "guest");
    }
}
