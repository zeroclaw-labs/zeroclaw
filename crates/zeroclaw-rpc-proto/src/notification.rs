//! Server-to-client notification names.
//!
//! Notifications are JSON-RPC requests without an `id`. The daemon pushes
//! them on the connection that owns the subscription or session.

/// Turn progress for a session started with `session/prompt`. Payload:
/// [`crate::types::SessionUpdateEvent`].
pub const SESSION_UPDATE: &str = "session/update";

/// One frame from the daemon event bus, delivered to a `logs/subscribe`
/// subscriber. Payload: an untyped event object (see `zeroclaw-log`).
pub const LOGS_EVENT: &str = "logs/event";

/// Every notification with the name of its payload type, or `None` when the
/// payload is an untyped JSON object.
pub const ALL: &[(&str, Option<&str>)] = &[
    (SESSION_UPDATE, Some("SessionUpdateEvent")),
    (LOGS_EVENT, None),
];

#[cfg(test)]
mod tests {
    use super::ALL;
    use std::collections::BTreeSet;

    #[test]
    fn notification_names_are_unique() {
        let names: BTreeSet<_> = ALL.iter().map(|(n, _)| *n).collect();
        assert_eq!(names.len(), ALL.len());
    }
}
