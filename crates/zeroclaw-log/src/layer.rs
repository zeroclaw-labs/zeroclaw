//! `tracing-subscriber` Layer that captures `record!` emissions and
//! `attribution_span!` spans, assembling alias-bound `LogEvent`s and
//! routing them to JSONL persistence, the broadcast hook, and the
//! Observer bridge.
//!
//! Two recognized span/event shapes:
//!
//! 1. `attribution_span!(thing)` — opens a span with `target =
//!    "zeroclaw_log_internal_attribution"` carrying `zc_role_family`,
//!    `zc_role_type`, `zc_attribution_field`, `zc_composite_prefix`,
//!    `zc_default_category`, and `zc_alias`. The layer stashes a
//!    `ZeroclawAttribution` snapshot in the span's extensions; no
//!    LogEvent is emitted for the span itself.
//! 2. `record!(LEVEL, Event::new(...), "msg")` — emits an event with
//!    `target = "zeroclaw_log_event"` carrying `zc_name`, `zc_action`,
//!    `zc_outcome`, `zc_category`, `zc_attrs`, `zc_has_duration`,
//!    `zc_duration_ms`, and `message`. The layer walks the span scope
//!    leaf→root, merges every attribution snapshot it finds, and
//!    writes a fully populated `LogEvent`.

use std::fmt::Write;

use serde_json::{Map as JsonMap, Value};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Record};
use tracing::{Event, Id, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

use crate::event::{
    ATTRIBUTION_FIELDS, COMPOSITE_PREFIXES, EventCategory, EventOutcome, LogEvent, Severity,
    ZeroclawAttribution,
};
use crate::writer::record_event;

const TARGET_EVENT: &str = "zeroclaw_log_event";
const TARGET_ATTRIBUTION_SPAN: &str = "zeroclaw_log_internal_attribution";
const TARGET_SCOPE_SPAN: &str = "zeroclaw_log_internal_scope";
const TARGET_SUPPRESS_PREFIX: &str = "zeroclaw_log_internal";

const F_NAME: &str = "zc_name";
const F_ACTION: &str = "zc_action";
const F_OUTCOME: &str = "zc_outcome";
const F_CATEGORY: &str = "zc_category";
const F_ATTRS: &str = "zc_attrs";
const F_HAS_DURATION: &str = "zc_has_duration";
const F_DURATION_MS: &str = "zc_duration_ms";
const F_FILE: &str = "zc_file";
const F_LINE: &str = "zc_line";
const F_MESSAGE: &str = "message";

const F_ROLE_FAMILY: &str = "zc_role_family";
const F_ROLE_TYPE: &str = "zc_role_type";
const F_ATTRIB_FIELD: &str = "zc_attribution_field";
const F_COMPOSITE_PREFIX: &str = "zc_composite_prefix";
const F_DEFAULT_CATEGORY: &str = "zc_default_category";
const F_ALIAS: &str = "zc_alias";

pub struct LogCaptureLayer;

impl<S> tracing_subscriber::Layer<S> for LogCaptureLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let target = attrs.metadata().target();
        let Some(span) = ctx.span(id) else { return };
        if target == TARGET_ATTRIBUTION_SPAN {
            let mut v = AttributionSpanCollector::default();
            attrs.record(&mut v);
            let mut attribution = ZeroclawAttribution::default();
            let default_category = v.default_category.as_deref().and_then(EventCategory::parse);
            v.apply_into(&mut attribution);
            let mut exts = span.extensions_mut();
            exts.insert(attribution);
            if let Some(cat) = default_category {
                exts.insert(SpanCategory(cat));
            }
            return;
        }
        if target == TARGET_SCOPE_SPAN {
            let mut v = ScopeSpanCollector::default();
            attrs.record(&mut v);
            v.install(span);
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let target = span.metadata().target();
        if target == TARGET_ATTRIBUTION_SPAN {
            let mut v = AttributionSpanCollector::default();
            values.record(&mut v);
            let mut attribution = ZeroclawAttribution::default();
            v.apply_into(&mut attribution);
            let mut exts = span.extensions_mut();
            if let Some(existing) = exts.get_mut::<ZeroclawAttribution>() {
                existing.merge_from(&attribution);
            } else {
                exts.insert(attribution);
            }
            return;
        }
        if target == TARGET_SCOPE_SPAN {
            let mut v = ScopeSpanCollector::default();
            values.record(&mut v);
            v.install(span);
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let target = metadata.target();

        if target.starts_with(TARGET_SUPPRESS_PREFIX) {
            return;
        }

        let severity = Severity::from_tracing_level(*metadata.level());

        // Two emission paths:
        //   1. `record!` → target == TARGET_EVENT; structured fields.
        //   2. raw `tracing::*!` outside `record!` → arbitrary fields;
        //      we treat the message as the entire payload.
        let mut visitor = EventCollector::default();
        event.record(&mut visitor);

        let action_str = visitor
            .action
            .as_deref()
            .map(str::to_string)
            .unwrap_or_else(|| metadata.name().to_string());

        let category = visitor
            .category
            .as_deref()
            .filter(|s| !s.is_empty())
            .and_then(EventCategory::parse)
            .or_else(|| {
                ctx.lookup_current()
                    .into_iter()
                    .flat_map(|span| span.scope())
                    .find_map(|span| span.extensions().get::<SpanCategory>().map(|c| c.0))
            })
            .unwrap_or(EventCategory::Internal);

        let name_for_action = visitor
            .name
            .as_deref()
            .unwrap_or(action_str.as_str())
            .to_string();

        let mut log_event = LogEvent::new(severity, &name_for_action, category);

        if target == TARGET_EVENT {
            log_event.event.action = action_str;
        }

        if let Some(outcome) = visitor.outcome.as_deref().and_then(EventOutcome::parse) {
            log_event.set_outcome(outcome);
        }

        log_event.message = Some(visitor.message.unwrap_or_default());

        if visitor.has_duration.unwrap_or(false) {
            log_event.zeroclaw.duration_ms = visitor.duration_ms;
        }

        if let Some(attrs_json) = visitor.attrs_json
            && !attrs_json.is_empty()
            && let Ok(v) = serde_json::from_str::<Value>(&attrs_json)
        {
            log_event.attributes = v;
        }
        if !visitor.extra.is_empty() {
            if log_event.attributes.is_null() {
                log_event.attributes = Value::Object(visitor.extra);
            } else if let Value::Object(map) = &mut log_event.attributes {
                for (k, v) in visitor.extra {
                    map.entry(k).or_insert(v);
                }
            }
        }

        // Attach source location for jump-to-source from log viewers.
        if visitor.file.is_some() || visitor.line.is_some() {
            let map = match &mut log_event.attributes {
                Value::Object(m) => m,
                _ => {
                    log_event.attributes = Value::Object(JsonMap::new());
                    match &mut log_event.attributes {
                        Value::Object(m) => m,
                        _ => unreachable!(),
                    }
                }
            };
            if let Some(f) = visitor.file {
                map.entry("_file".to_string()).or_insert(Value::String(f));
            }
            if let Some(l) = visitor.line {
                map.entry("_line".to_string()).or_insert(Value::from(l));
            }
        }

        // Walk span scope leaf→root, merging every attribution snapshot
        // and every ScopeExtra stash along the way. Inner spans win
        // because we merge_from() / entry().or_insert() which fills only
        // absent keys.
        if let Some(span_ref) = ctx.lookup_current() {
            let mut current = Some(span_ref);
            while let Some(span) = current {
                let exts = span.extensions();
                if let Some(parent) = exts.get::<ZeroclawAttribution>() {
                    log_event.zeroclaw.merge_from(parent);
                }
                if let Some(scope_extra) = exts.get::<ScopeExtra>() {
                    if log_event.attributes.is_null() {
                        log_event.attributes = Value::Object(scope_extra.extra.clone());
                    } else if let Value::Object(map) = &mut log_event.attributes {
                        for (k, v) in &scope_extra.extra {
                            map.entry(k.clone()).or_insert_with(|| v.clone());
                        }
                    }
                }
                drop(exts);
                current = span.parent();
            }
        }

        record_event(log_event);
    }
}

#[derive(Clone, Copy)]
struct SpanCategory(EventCategory);

#[derive(Default)]
struct EventCollector {
    name: Option<String>,
    action: Option<String>,
    outcome: Option<String>,
    category: Option<String>,
    attrs_json: Option<String>,
    has_duration: Option<bool>,
    duration_ms: Option<u64>,
    file: Option<String>,
    line: Option<u64>,
    message: Option<String>,
    extra: JsonMap<String, Value>,
}

impl Visit for EventCollector {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.put(field.name(), Value::String(value.to_string()));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        if field.name() == F_HAS_DURATION {
            self.has_duration = Some(value);
            return;
        }
        self.put(field.name(), Value::Bool(value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.put(field.name(), Value::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == F_DURATION_MS {
            self.duration_ms = Some(value);
            return;
        }
        self.put(field.name(), Value::from(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.put(
            field.name(),
            serde_json::Number::from_f64(value)
                .map(Value::Number)
                .unwrap_or(Value::Null),
        );
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        let mut buf = String::new();
        let _ = write!(&mut buf, "{value}");
        let mut current = value.source();
        while let Some(src) = current {
            let _ = write!(&mut buf, ": {src}");
            current = src.source();
        }
        self.put(field.name(), Value::String(buf));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let mut buf = String::new();
        let _ = write!(&mut buf, "{value:?}");
        if field.name() == F_MESSAGE {
            self.message = Some(strip_outer_quotes(&buf));
            return;
        }
        if field.name() == F_HAS_DURATION {
            // `%bool` on tracing comes through Display, not Debug, but
            // guard anyway.
            self.has_duration = Some(buf == "true");
            return;
        }
        self.put(field.name(), Value::String(buf));
    }
}

impl EventCollector {
    fn put(&mut self, name: &str, value: Value) {
        match name {
            F_NAME => {
                if let Value::String(s) = value {
                    self.name = Some(s);
                }
            }
            F_ACTION => {
                if let Value::String(s) = value {
                    self.action = Some(s);
                }
            }
            F_OUTCOME => {
                if let Value::String(s) = value {
                    self.outcome = Some(s);
                }
            }
            F_CATEGORY => {
                if let Value::String(s) = value {
                    self.category = Some(s);
                }
            }
            F_ATTRS => {
                if let Value::String(s) = value {
                    self.attrs_json = Some(s);
                }
            }
            F_DURATION_MS => {
                if let Value::Number(n) = &value
                    && let Some(u) = n.as_u64()
                {
                    self.duration_ms = Some(u);
                } else if let Value::String(s) = &value
                    && let Ok(u) = s.parse::<u64>()
                {
                    self.duration_ms = Some(u);
                }
            }
            F_HAS_DURATION => {
                if let Value::Bool(b) = value {
                    self.has_duration = Some(b);
                } else if let Value::String(s) = value {
                    self.has_duration = Some(s == "true");
                }
            }
            F_MESSAGE => {
                if let Value::String(s) = value {
                    self.message = Some(s);
                }
            }
            F_FILE => {
                if let Value::String(s) = value {
                    self.file = Some(s);
                }
            }
            F_LINE => {
                if let Value::Number(n) = &value
                    && let Some(u) = n.as_u64()
                {
                    self.line = Some(u);
                } else if let Value::String(s) = &value
                    && let Ok(u) = s.parse::<u64>()
                {
                    self.line = Some(u);
                }
            }
            _ => {
                self.extra.insert(name.to_string(), value);
            }
        }
    }
}

#[derive(Default)]
struct AttributionSpanCollector {
    role_family: Option<String>,
    role_type: Option<String>,
    attribution_field: Option<String>,
    composite_prefix: Option<String>,
    default_category: Option<String>,
    alias: Option<String>,
}

impl AttributionSpanCollector {
    fn apply_into(self, attr: &mut ZeroclawAttribution) {
        let Some(alias) = self.alias.as_deref().filter(|s| !s.is_empty()) else {
            return;
        };

        if let Some(prefix) = self.composite_prefix.as_deref().filter(|s| !s.is_empty()) {
            // Composite role: build `<type>.<alias>` if we have type;
            // otherwise just the alias as the bare composite value.
            let ty = self.role_type.as_deref().unwrap_or("");
            if !ty.is_empty() {
                attr.set_composite(prefix, &format!("{ty}.{alias}"));
            } else {
                attr.set_composite(prefix, alias);
            }
        } else if let Some(field) = self.attribution_field.as_deref().filter(|s| !s.is_empty()) {
            attr.set(field, alias);
        }

        if let Some(family) = self.role_family.as_deref().filter(|s| !s.is_empty()) {
            attr.set("zc_role", family);
        }
    }
}

impl Visit for AttributionSpanCollector {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.put(field.name(), value);
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let mut buf = String::new();
        let _ = write!(&mut buf, "{value:?}");
        let trimmed = strip_outer_quotes(&buf);
        self.put(field.name(), &trimmed);
    }
}

impl AttributionSpanCollector {
    fn put(&mut self, name: &str, value: &str) {
        match name {
            F_ROLE_FAMILY => self.role_family = Some(value.to_string()),
            F_ROLE_TYPE => self.role_type = Some(value.to_string()),
            F_ATTRIB_FIELD => self.attribution_field = Some(value.to_string()),
            F_COMPOSITE_PREFIX => self.composite_prefix = Some(value.to_string()),
            F_DEFAULT_CATEGORY => self.default_category = Some(value.to_string()),
            F_ALIAS => self.alias = Some(value.to_string()),
            _ => {}
        }
    }
}

/// Carries ad-hoc per-scope context (sender id, message id, turn id,
/// etc.) emitted via [`crate::scope!`]. Recognized attribution fields
/// land in `attribution`; any free-form keys land in `extra`. Both
/// stashes ride on the span's extensions and are merged onto every
/// descendant event by the layer's scope walk.
#[derive(Default)]
struct ScopeExtra {
    extra: JsonMap<String, Value>,
}

#[derive(Default)]
struct ScopeSpanCollector {
    category: Option<String>,
    attribution: ZeroclawAttribution,
    extra: JsonMap<String, Value>,
}

impl ScopeSpanCollector {
    fn install<'a>(
        self,
        span: tracing_subscriber::registry::SpanRef<
            'a,
            impl Subscriber + for<'lookup> LookupSpan<'lookup>,
        >,
    ) {
        if !self.attribution.fields.is_empty() || self.attribution.duration_ms.is_some() {
            let mut exts = span.extensions_mut();
            if let Some(existing) = exts.get_mut::<ZeroclawAttribution>() {
                existing.merge_from(&self.attribution);
            } else {
                exts.insert(self.attribution);
            }
        }
        if !self.extra.is_empty() {
            let mut exts = span.extensions_mut();
            if let Some(existing) = exts.get_mut::<ScopeExtra>() {
                for (k, v) in self.extra {
                    existing.extra.entry(k).or_insert(v);
                }
            } else {
                exts.insert(ScopeExtra { extra: self.extra });
            }
        }
        if let Some(cat) = self.category.as_deref().and_then(EventCategory::parse) {
            span.extensions_mut().insert(SpanCategory(cat));
        }
    }

    fn put(&mut self, name: &str, value: Value) {
        if name == "category" {
            if let Value::String(s) = value {
                self.category = Some(s);
            }
            return;
        }

        for prefix in COMPOSITE_PREFIXES {
            if name == *prefix
                && let Value::String(s) = &value
            {
                if s.contains('.') {
                    self.attribution.set_composite(prefix, s);
                } else {
                    self.attribution.set(format!("{prefix}_type"), s.clone());
                }
                return;
            }
        }

        if ATTRIBUTION_FIELDS.contains(&name)
            && let Value::String(s) = value
        {
            self.attribution.set(name, s);
            return;
        }

        self.extra.insert(name.to_string(), value);
    }
}

impl Visit for ScopeSpanCollector {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.put(field.name(), Value::String(value.to_string()));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.put(field.name(), Value::Bool(value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.put(field.name(), Value::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == "duration_ms" {
            self.attribution.duration_ms = Some(value);
            return;
        }
        self.put(field.name(), Value::from(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.put(
            field.name(),
            serde_json::Number::from_f64(value)
                .map(Value::Number)
                .unwrap_or(Value::Null),
        );
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let mut buf = String::new();
        let _ = write!(&mut buf, "{value:?}");
        self.put(field.name(), Value::String(strip_outer_quotes(&buf)));
    }
}

fn strip_outer_quotes(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
        return trimmed[1..trimmed.len() - 1].to_string();
    }
    trimmed.to_string()
}

#[cfg(test)]
mod e2e_tests {
    use crate as zeroclaw_log;
    use crate::{
        Action, Event, EventOutcome, subscribe_or_install, try_install_capture_subscriber,
    };
    use ::zeroclaw_api::attribution::{Attributable, ChannelKind, Role};

    /// Synthetic Attributable test fixture standing in for a real
    /// channel impl. Keeps the test free of every channel impl's
    /// transitive deps.
    struct FakeTelegramChannel {
        alias: String,
    }

    impl Attributable for FakeTelegramChannel {
        fn role(&self) -> Role {
            Role::Channel(ChannelKind::Telegram)
        }
        fn alias(&self) -> &str {
            &self.alias
        }
    }

    static TEST_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn attribution_span_populates_alias_bound_fields() {
        // Hold both the subscriber lock and the writer lock: this test
        // fires record! through the global LogCaptureLayer, which forwards
        // to writer::record_event. Without the writer lock, a concurrent
        // writer::tests run sees this test's event land in its tempdir.
        let _subscriber_guard = TEST_LOCK.lock();
        let _writer_guard = crate::writer::WRITER_TEST_LOCK.lock();
        // Hold the broadcast hook lock too: the broadcast module's own tests
        // clear/install the global hook under this same lock. Without it, a
        // parallel `clear_broadcast_hook` drops this test's event and the
        // search below times out.
        let _hook_guard = crate::broadcast::HOOK_TEST_LOCK.lock();

        try_install_capture_subscriber();
        let mut rx = subscribe_or_install();
        // Drain any pre-existing buffered events from prior tests.
        while rx.try_recv().is_ok() {}

        let thing = FakeTelegramChannel {
            alias: "clamps".into(),
        };

        {
            use zeroclaw_log::Instrument;
            async {
                zeroclaw_log::record!(
                    INFO,
                    Event::new(module_path!(), Action::Note).with_outcome(EventOutcome::Success),
                    "attribution-span e2e test"
                );
            }
            .instrument(zeroclaw_log::attribution_span!(&thing))
            .await;
        }

        // Drain captured events and find ours. `recv` is awaited inside a
        // deadline so the receiver can recover from `Lagged` errors caused
        // by other workspace tests firing `record!` into the same global
        // broadcast hook in parallel; a single Lagged would otherwise abort
        // the search prematurely.
        let mut found = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !found && std::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let step = remaining.min(std::time::Duration::from_millis(50));
            match tokio::time::timeout(step, rx.recv()).await {
                Ok(Ok(value)) => {
                    if value
                        .get("message")
                        .and_then(|v| v.as_str())
                        .map(|s| s.contains("attribution-span e2e test"))
                        .unwrap_or(false)
                    {
                        let zc = value.get("zeroclaw").expect("zeroclaw block present");
                        assert_eq!(
                            zc.get("channel").and_then(|v| v.as_str()),
                            Some("telegram.clamps"),
                            "expected channel composite, got: {zc:?}"
                        );
                        assert_eq!(
                            zc.get("channel_type").and_then(|v| v.as_str()),
                            Some("telegram"),
                        );
                        assert_eq!(
                            zc.get("channel_alias").and_then(|v| v.as_str()),
                            Some("clamps"),
                        );
                        found = true;
                    }
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
                Err(_elapsed) => {}
            }
        }
        assert!(
            found,
            "did not find the test event with attribution-span fields",
        );

        // Clean up so subsequent parallel tests aren't affected.
        crate::clear_broadcast_hook();
    }
}
