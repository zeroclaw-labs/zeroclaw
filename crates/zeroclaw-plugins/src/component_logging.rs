//! Host-side `logging` and `types` implementations for all three plugin worlds.
//!
//! The `log-record` host import must never block the calling executor thread:
//! the wall-clock deadline around every guest export is cooperative and can
//! only fire while the Wasmtime future yields, so a stalled stderr consumer or
//! JSONL writer inside this import would let an export overrun
//! `plugins.limits.call_timeout_ms` and skip the store-discard path. Records
//! are therefore handed to a bounded queue and written by a dedicated host
//! thread. This matches the WIT contract that `log-record` is fire-and-forget.
//!
//! The queue bound is a real memory bound, not just a record count. The WIT
//! event fields are unbounded strings that arrive as host-owned allocations
//! outside the guest's `max_memory_mb` ceiling, so a guest looping on large
//! messages against a stalled drain could otherwise retain
//! `PLUGIN_LOG_QUEUE_CAPACITY` guest-memory-sized copies on the host. A
//! record whose guest-controlled bytes exceed [`MAX_PLUGIN_LOG_RECORD_BYTES`]
//! is dropped, and queued records reserve from the aggregate
//! [`PLUGIN_LOG_QUEUE_BYTE_BUDGET`], released only after the drain side has
//! written them. Every rejected record, whatever the reason, lands in the same
//! drop counter, and the drain thread reports new drops after each written
//! record and on an idle-wake interval, so loss is surfaced even when nothing
//! accepted ever follows the rejected records. The one unreportable corner is
//! a drain thread that could not spawn at all: rejections are still counted,
//! but there is no thread left to write the report.
//!
//! Deferral must not change what an event means: each record captures the
//! host span that was current at the guest call site, and the drain thread
//! re-enters it around the deferred `record!`, so agent/channel/tool
//! attribution and the terminal label match what inline emission produced.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::time::Duration;

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

/// One guest-emitted log record, owned so it can cross to the drain thread.
struct QueuedPluginLog {
    /// Host tracing span that was current when the guest emitted the record.
    /// Re-entered around the deferred `record!` so the delivered event keeps
    /// the same agent/channel/tool attribution scope it had when logging was
    /// inline; without it every plugin event would degrade to system-level
    /// attribution on the drain thread. A fixed-size host-owned handle,
    /// deliberately excluded from [`Self::guest_bytes`].
    span: zeroclaw_log::Span,
    instance: PluginInstanceId,
    level_idx: u8,
    fn_name: String,
    action: Action,
    outcome: EventOutcome,
    duration_ms: Option<u64>,
    raw_attrs: Option<String>,
    msg: String,
}

impl QueuedPluginLog {
    /// Bytes of guest-controlled string payload this record retains on the
    /// host while queued. The host-issued instance identity is deliberately
    /// excluded: it is small, bounded, and not attacker-influenced.
    fn guest_bytes(&self) -> usize {
        self.fn_name.len() + self.raw_attrs.as_ref().map_or(0, String::len) + self.msg.len()
    }
}

/// Count bound chosen so a chatty guest can burst without loss while a wedged
/// consumer caps host memory at roughly one queue of records.
const PLUGIN_LOG_QUEUE_CAPACITY: usize = 1024;

/// Ceiling on one record's guest-controlled bytes. Generous for any
/// legitimate log line; a larger record is dropped rather than truncated so
/// the host never forwards a mangled (for example half-a-JSON-`attrs`) event.
const MAX_PLUGIN_LOG_RECORD_BYTES: usize = 64 * 1024;

/// Aggregate ceiling on guest-controlled bytes queued or being written.
/// Bounds worst-case host retention while the drain thread is stalled;
/// reserved at enqueue and released only after the record has been written.
const PLUGIN_LOG_QUEUE_BYTE_BUDGET: usize = 8 * 1024 * 1024;

/// Records dropped for any reason: full queue, oversized record, exhausted
/// byte budget, or a drain thread that never started. Incremented on the
/// enqueue side, reported from the drain side, where blocking is allowed.
static DROPPED_PLUGIN_LOGS: AtomicU64 = AtomicU64::new(0);

/// Guest-controlled bytes currently reserved by queued or in-flight records.
static QUEUED_PLUGIN_LOG_BYTES: AtomicUsize = AtomicUsize::new(0);

/// Reserve `bytes` against [`PLUGIN_LOG_QUEUE_BYTE_BUDGET`]. A compare
/// exchange loop keeps the accounting exact: concurrent enqueues can never
/// overshoot the budget, only fail and drop.
fn try_reserve_plugin_log_bytes(bytes: usize) -> bool {
    let mut current = QUEUED_PLUGIN_LOG_BYTES.load(Ordering::Relaxed);
    loop {
        let Some(next) = current.checked_add(bytes) else {
            return false;
        };
        if next > PLUGIN_LOG_QUEUE_BYTE_BUDGET {
            return false;
        }
        match QUEUED_PLUGIN_LOG_BYTES.compare_exchange_weak(
            current,
            next,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return true,
            Err(actual) => current = actual,
        }
    }
}

fn release_plugin_log_bytes(bytes: usize) {
    QUEUED_PLUGIN_LOG_BYTES.fetch_sub(bytes, Ordering::Relaxed);
}

fn plugin_log_queue() -> &'static SyncSender<QueuedPluginLog> {
    static QUEUE: OnceLock<SyncSender<QueuedPluginLog>> = OnceLock::new();
    QUEUE.get_or_init(|| {
        let (tx, rx) = sync_channel(PLUGIN_LOG_QUEUE_CAPACITY);
        // If the drain thread cannot spawn, the receiver is dropped and every
        // enqueue lands in the drop counter: logging degrades but a guest
        // export still cannot block, which is the invariant that matters.
        let _ = std::thread::Builder::new()
            .name("zc-plugin-log".to_string())
            .spawn(move || drain_plugin_logs(&rx));
        tx
    })
}

/// Enqueue without ever blocking the caller. An oversized record, an
/// exhausted byte budget, or a full queue drops the newest record; the drain
/// thread reports the accumulated drop count on its own schedule so the loss
/// is observable without re-entering the blocked path.
fn enqueue_plugin_log(log: QueuedPluginLog) {
    let bytes = log.guest_bytes();
    if bytes > MAX_PLUGIN_LOG_RECORD_BYTES || !try_reserve_plugin_log_bytes(bytes) {
        DROPPED_PLUGIN_LOGS.fetch_add(1, Ordering::Relaxed);
        return;
    }
    if plugin_log_queue().try_send(log).is_err() {
        release_plugin_log_bytes(bytes);
        DROPPED_PLUGIN_LOGS.fetch_add(1, Ordering::Relaxed);
    }
}

/// How long the drain thread sleeps on an empty queue before checking for
/// unreported drops. Bounds the delay between a rejection and its report when
/// no accepted record ever follows; one atomic load per wake keeps the idle
/// cost negligible.
const DROP_REPORT_IDLE_INTERVAL: Duration = Duration::from_secs(2);

fn drain_plugin_logs(rx: &Receiver<QueuedPluginLog>) {
    let mut reported_drops = 0_u64;
    loop {
        match rx.recv_timeout(DROP_REPORT_IDLE_INTERVAL) {
            Ok(log) => {
                // Re-enter the captured caller span so subscribers observe the
                // same attribution scope the record had at the guest call site.
                log.span.in_scope(|| do_log_record(&log));
                // Release only after the write: while the drain side is
                // stalled on this record, its memory is still retained and
                // must stay counted.
                release_plugin_log_bytes(log.guest_bytes());
            }
            // Idle wake: fall through to the drop check so rejected records
            // are reported even when nothing accepted ever follows them.
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        let dropped = DROPPED_PLUGIN_LOGS.load(Ordering::Relaxed);
        if dropped > reported_drops {
            record!(
                WARN,
                Event::new(module_path!(), Action::Skip)
                    .with_outcome(EventOutcome::Failure)
                    .with_attrs(serde_json::json!({
                        "newly_dropped": dropped - reported_drops,
                        "total_dropped": dropped,
                    })),
                "plugin log queue overflowed; newest records were dropped"
            );
            reported_drops = dropped;
        }
    }
}

fn do_log_record(log: &QueuedPluginLog) {
    let mut ev = Event::new(module_path!(), log.action).with_outcome(log.outcome);
    if let Some(ms) = log.duration_ms {
        ev = ev.with_duration(ms);
    }
    ev = ev.with_attrs(plugin_log_attrs(
        &log.instance,
        log.fn_name.clone(),
        log.raw_attrs.clone(),
    ));
    let msg = log.msg.clone();
    match log.level_idx {
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
                // Hand off instead of writing inline: see the module docs for
                // why this import must never block the executor thread.
                enqueue_plugin_log(QueuedPluginLog {
                    span: zeroclaw_log::Span::current(),
                    instance: self.scope().id().clone(),
                    level_idx,
                    fn_name: event.function_name,
                    action,
                    outcome,
                    duration_ms: event.duration_ms,
                    raw_attrs: event.attrs,
                    msg: event.message,
                });
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

    /// One test on purpose: the byte accounting is process-global state, and
    /// exercising the oversized-drop and reserve/release paths sequentially
    /// keeps the assertions free of cross-test interleaving.
    #[test]
    fn plugin_log_queue_enforces_byte_bounds() {
        let scope = crate::instance::test_scope(PluginCapability::Tool, "bounds", []);
        let record = |msg: String| QueuedPluginLog {
            span: zeroclaw_log::Span::current(),
            instance: scope.id().clone(),
            level_idx: 2,
            fn_name: "bounds::execute".to_string(),
            action: Action::Note,
            outcome: EventOutcome::Unknown,
            duration_ms: None,
            raw_attrs: None,
            msg,
        };

        // Guest-controlled sizing counts every guest string, not just `msg`.
        let mut sized = record("m".repeat(10));
        sized.raw_attrs = Some("a".repeat(20));
        assert_eq!(sized.guest_bytes(), "bounds::execute".len() + 10 + 20);

        // An oversized record is dropped before reserving any budget.
        let drops_before = DROPPED_PLUGIN_LOGS.load(Ordering::Relaxed);
        let bytes_before = QUEUED_PLUGIN_LOG_BYTES.load(Ordering::Relaxed);
        enqueue_plugin_log(record("x".repeat(MAX_PLUGIN_LOG_RECORD_BYTES + 1)));
        assert!(
            DROPPED_PLUGIN_LOGS.load(Ordering::Relaxed) > drops_before,
            "an oversized record must land in the drop counter"
        );
        assert_eq!(
            QUEUED_PLUGIN_LOG_BYTES.load(Ordering::Relaxed),
            bytes_before,
            "an oversized record must not reserve budget bytes"
        );

        // The aggregate budget is exact: a reservation holds its bytes until
        // released, and a request that would cross the ceiling fails whole.
        assert!(try_reserve_plugin_log_bytes(1024));
        assert!(
            !try_reserve_plugin_log_bytes(PLUGIN_LOG_QUEUE_BYTE_BUDGET),
            "a partially reserved budget must reject a full-budget request"
        );
        release_plugin_log_bytes(1024);
        assert!(
            try_reserve_plugin_log_bytes(PLUGIN_LOG_QUEUE_BYTE_BUDGET - bytes_before),
            "releasing must restore the reserved bytes"
        );
        release_plugin_log_bytes(PLUGIN_LOG_QUEUE_BYTE_BUDGET - bytes_before);
    }
}
