// Host-side WIT `logging` implementation for all three component-model plugin
// worlds (`tool-plugin`, `memory-plugin`, `channel-plugin`).
//
// [`PluginLoggingHost`] is the `Store<T>` data type for all three worlds.
// It is a ZST — logging routes to global tracing infrastructure already
// initialized by the daemon; no per-instance state is needed.

use std::time::Instant;

use serde_json::json;
use wasmtime::component::{HasSelf, ResourceTable};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use zeroclaw_log::{Action, Event, EventOutcome, info_span, record};

use super::bindings;

/// Store-data type for all three component plugin worlds.
#[derive(Default)]
pub struct PluginLoggingHost {
    wasi: WasiCtx,
    table: ResourceTable,
}

impl WasiView for PluginLoggingHost {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

// ── types::Host (empty marker trait) ─────────────────────────────────────────

impl bindings::tool::zeroclaw::plugin::types::Host for PluginLoggingHost {}
impl bindings::memory::zeroclaw::plugin::types::Host for PluginLoggingHost {}
impl bindings::channel::zeroclaw::plugin::types::Host for PluginLoggingHost {}

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

/// Generate `logging::Host for PluginLoggingHost` for one bindgen world.
///
/// All three worlds produce identical-but-distinct Rust types from the same
/// WIT; the macro eliminates the otherwise triple-repeated match bodies.
macro_rules! impl_logging_host {
    ($world:ident) => {
        impl bindings::$world::zeroclaw::plugin::logging::Host for PluginLoggingHost {
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

// ── Linker wiring helpers ─────────────────────────────────────────────────────

/// Wire all host interfaces for the `tool-plugin` world into `linker`.
pub fn add_to_linker_tool(
    linker: &mut wasmtime::component::Linker<PluginLoggingHost>,
) -> anyhow::Result<()> {
    // Use feature flags to allow developers to link in wit bindings that aren't stabilized yet.
    let mut options = crate::component::v0::bindings::tool::LinkOptions::default();
    #[cfg(feature = "plugins-wit-v0")]
    {
        options.plugins_wit_v0(true);
    }
    bindings::tool::ToolPlugin::add_to_linker::<PluginLoggingHost, HasSelf<PluginLoggingHost>>(
        linker,
        &options,
        |x| x,
    )
    .map_err(crate::error::PluginError::from)?;
    Ok(())
}

/// Wire all host interfaces for the `memory-plugin` world into `linker`.
pub fn add_to_linker_memory(
    linker: &mut wasmtime::component::Linker<PluginLoggingHost>,
) -> anyhow::Result<()> {
    // Use feature flags to allow developers to link in wit bindings that aren't stabilized yet.
    let mut options = crate::component::v0::bindings::memory::LinkOptions::default();
    #[cfg(feature = "plugins-wit-v0")]
    {
        options.plugins_wit_v0(true);
    }
    bindings::memory::MemoryPlugin::add_to_linker::<PluginLoggingHost, HasSelf<PluginLoggingHost>>(
        linker,
        &options,
        |x| x,
    )
    .map_err(crate::error::PluginError::from)?;
    Ok(())
}

/// Wire all host interfaces for the `channel-plugin` world into `linker`.
pub fn add_to_linker_channel(
    linker: &mut wasmtime::component::Linker<PluginLoggingHost>,
) -> anyhow::Result<()> {
    // Use feature flags to allow developers to link in wit bindings that aren't stabilized yet.
    let mut options = crate::component::v0::bindings::channel::LinkOptions::default();
    #[cfg(feature = "plugins-wit-v0")]
    {
        options.plugins_wit_v0(true);
    }
    bindings::channel::ChannelPlugin::add_to_linker::<
        PluginLoggingHost,
        HasSelf<PluginLoggingHost>,
    >(linker,
        &options, |x| x)
    .map_err(crate::error::PluginError::from)?;
    Ok(())
}

// ── Span and call wrapper ─────────────────────────────────────────────────────

/// Async call wrapper
///
/// Enters a tracing span, emits start/complete trace records with timing,
/// then returns the result of `f.await`.
pub async fn wrap_plugin_call<F, T>(
    plugin_name: &str,
    plugin_version: &str,
    op_name: &str,
    f: F,
) -> T
where
    F: std::future::Future<Output = T>,
{
    // Enter a span for the entire plugin call if the log level is Info, Debug or Trace. This
    // will attach the plugin_name and plugin_version fields to all logs emitted by the plugin
    // during this call.
    let span = info_span!(
        "plugin_call",
        plugin_name = %plugin_name,
        plugin_version = %plugin_version,
    );
    let _guard = span.enter();

    // When tracing, also record the start and end of the call along with its duration.
    record!(
        TRACE,
        Event::new(module_path!(), Action::Invoke)
            .with_attrs(json!({ "plugin": plugin_name, "op": op_name })),
        "plugin call start",
    );
    let start = Instant::now();
    let result = f.await;
    let duration_ms = start.elapsed().as_millis() as u64;
    record!(
        TRACE,
        Event::new(module_path!(), Action::Complete)
            .with_duration(duration_ms)
            .with_attrs(json!({ "plugin": plugin_name, "op": op_name })),
        "plugin call complete",
    );
    result
}

/// Sync call wrapper for use inside `spawn_blocking`.
///
/// Enters a tracing span, emits start/complete trace records with timing,
/// then returns the result of `f()`.
pub fn wrap_plugin_call_sync<F, T>(
    plugin_name: &str,
    plugin_version: &str,
    op_name: &str,
    f: F,
) -> T
where
    F: FnOnce() -> T,
{
    // Enter a span for the entire plugin call if the log level is Info, Debug or Trace. This
    // will attach the plugin_name and plugin_version fields to all logs emitted by the plugin
    // during this call.
    let span = info_span!(
        "plugin_call",
        plugin_name = %plugin_name,
        plugin_version = %plugin_version,
    );
    let _guard = span.enter();

    // When tracing, also record the start and end of the call along with its duration.
    record!(
        TRACE,
        Event::new(module_path!(), Action::Invoke)
            .with_attrs(json!({ "plugin": plugin_name, "op": op_name })),
        "plugin call start",
    );
    let start = Instant::now();
    let result = f();
    let duration_ms = start.elapsed().as_millis() as u64;
    record!(
        TRACE,
        Event::new(module_path!(), Action::Complete)
            .with_duration(duration_ms)
            .with_attrs(json!({ "plugin": plugin_name, "op": op_name })),
        "plugin call complete",
    );
    result
}
