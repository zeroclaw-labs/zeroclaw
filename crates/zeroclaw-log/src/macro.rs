//! `record!` — the sole logging surface for the workspace.

/// Emit a structured ZeroClaw log event. The single positional `Event`
/// expression carries the typed payload; the trailing literal is the
/// human-readable message.
#[macro_export]
macro_rules! record {
    ($level:ident, $event:expr, $msg:expr $(,)?) => {{
        let __zc_event: $crate::Event = $event;
        $crate::__private::tracing::event!(
            target: "zeroclaw_log_event",
            $crate::__private::tracing::Level::$level,
            zc_name = %__zc_event.name,
            zc_action = %__zc_event.action.as_str(),
            zc_outcome = %__zc_event.outcome_str(),
            zc_category = %__zc_event.category_str(),
            zc_attrs = %__zc_event.attrs_str(),
            zc_ephemeral_attrs = %__zc_event.ephemeral_attrs_str(),
            zc_has_duration = %__zc_event.has_duration(),
            zc_duration_ms = %__zc_event.duration_ms_or_zero(),
            zc_file = %file!(),
            zc_line = %line!(),
            message = %$msg,
        );
    }};
}

#[macro_export]
macro_rules! attribution_span {
    ($attributable:expr) => {{
        let __zc_thing = $attributable;
        let __zc_role = ::zeroclaw_api::attribution::Attributable::role(__zc_thing);
        let __zc_alias = ::zeroclaw_api::attribution::Attributable::alias(__zc_thing);
        $crate::__private::tracing::info_span!(
            target: "zeroclaw_log_internal_attribution",
            "zeroclaw_attribution",
            zc_role_family = %__zc_role.family_str(),
            zc_role_type = %__zc_role.composite_type().unwrap_or(""),
            zc_attribution_field = %__zc_role.attribution_field().unwrap_or(""),
            zc_composite_prefix = %__zc_role.composite_prefix().unwrap_or(""),
            zc_default_category = %__zc_role.default_category(),
            zc_alias = %__zc_alias,
        )
    }};
}

#[macro_export]
macro_rules! scope {
    ($($key:ident : $value:expr),+ $(,)? => $body:expr) => {{
        use $crate::__private::tracing::Instrument;
        ($body).instrument($crate::__private::tracing::info_span!(
            target: "zeroclaw_log_internal_scope",
            "zeroclaw_scope",
            $($key = %($value)),+
        ))
    }};
}

#[cfg(test)]
mod tests {
    use crate::{Action, Event, EventOutcome};
    use serde_json::json;

    #[test]
    fn record_compiles_minimal() {
        record!(INFO, Event::new(module_path!(), Action::Note), "hello");
    }

    #[test]
    fn record_compiles_with_attrs_and_outcome() {
        record!(
            WARN,
            Event::new(module_path!(), Action::Fail)
                .with_outcome(EventOutcome::Failure)
                .with_attrs(json!({"code": 42})),
            "failed"
        );
    }

    #[test]
    fn record_compiles_with_duration() {
        record!(
            INFO,
            Event::new(module_path!(), Action::Complete)
                .with_outcome(EventOutcome::Success)
                .with_duration(123),
            "done"
        );
    }
}
