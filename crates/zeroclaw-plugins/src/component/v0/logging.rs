// Host-side WIT `logging` implementation for all three component-model plugin
// worlds (`tool-plugin`, `memory-plugin`, `channel-plugin`).

use serde_json::json;
use zeroclaw_log::{Action, Event, EventOutcome, record};

use super::bindings;
use super::plugin_host::PluginHost;

// ── Core log dispatcher ───────────────────────────────────────────────────────

/// Inner log dispatcher invoked after world-specific type mapping.
///
/// `level_idx`: `0`=Trace, `1`=Debug, `2`=Info, `3`=Warn, `4+`=Error.
fn do_log_record(
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
    let attrs = match raw_attrs {
        Some(raw) => json!({ "plugin_fn": fn_name, "raw": raw }),
        None => json!({ "plugin_fn": fn_name }),
    };
    ev = ev.with_attrs(attrs);
    match level_idx {
        0 => record!(TRACE, ev, msg),
        1 => record!(DEBUG, ev, msg),
        2 => record!(INFO, ev, msg),
        3 => record!(WARN, ev, msg),
        _ => record!(ERROR, ev, msg),
    }
}

// ── logging::Host impls ───────────────────────────────────────────────────────

/// Generate `logging::Host for PluginHost` for one bindgen world.
///
/// All three worlds produce identical-but-distinct Rust types from the same
/// WIT; the macro eliminates the otherwise triple-repeated match bodies.
macro_rules! impl_logging_host {
    ($world:ident) => {
        impl bindings::$world::zeroclaw::plugin::logging::Host for PluginHost {
            async fn log_record(
                &mut self,
                level: bindings::$world::zeroclaw::plugin::logging::LogLevel,
                event: bindings::$world::zeroclaw::plugin::logging::PluginEvent,
            ) {
                use bindings::$world::zeroclaw::plugin::logging::LogLevel;
                use bindings::$world::zeroclaw::plugin::logging::PluginAction;
                use bindings::$world::zeroclaw::plugin::logging::PluginOutcome;

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

impl_logging_host!(tool);
impl_logging_host!(memory);
impl_logging_host!(channel);
