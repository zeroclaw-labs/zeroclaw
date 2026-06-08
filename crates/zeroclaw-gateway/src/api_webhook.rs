//! Per-alias webhook routing helpers (#6312).
//!
//! Inbound channel webhooks historically resolved their channel instance with
//! `config.channels.<type>.values().next()`, so a multi-instance config (e.g.
//! `whatsapp.work` + `whatsapp.personal`) only ever delivered traffic to the
//! first instance. This module adds path-based routing: `/<type>/{alias}`
//! resolves to the matching instance, while the bare `/<type>` path keeps
//! working as a deprecated fallback that resolves to the first configured
//! instance and tags the response with [`DEPRECATION_HEADER`].
//!
//! Channel handlers store their instances (and any per-instance signing
//! secrets) in `AppState` as `HashMap<String, _>` keyed by alias and call
//! [`resolve`] with the optional `<alias>` captured from the request path.

use std::collections::HashMap;

use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Json, Response};

/// Response header attached when a webhook is served via a deprecated bare path
/// (`/<type>` instead of `/<type>/{alias}`). Signals operators to migrate to the
/// alias-qualified path before bare-path routing is eventually removed.
pub const DEPRECATION_HEADER: &str = "x-zeroclaw-deprecation";

/// Outcome of resolving a webhook path's optional `<alias>` against the set of
/// configured channel instances for a single channel type.
pub enum Resolved<'a, T> {
    /// An explicit `<alias>` matched a configured instance.
    Alias { key: &'a str, value: &'a T },
    /// No `<alias>` was given (bare path); resolved to the first configured
    /// instance. Callers attach [`DEPRECATION_HEADER`] to the response.
    Fallback { key: &'a str, value: &'a T },
    /// No matching instance — either an explicit alias that is not configured,
    /// or no instances configured at all.
    NotFound,
}

// Manual `Copy`/`Clone` so the bound is `T: ?Sized`-friendly and does NOT require
// `T: Copy` (a derive would add that bound). Every field is a shared reference,
// which is always `Copy` regardless of `T`.
impl<T> Copy for Resolved<'_, T> {}
impl<T> Clone for Resolved<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<'a, T> Resolved<'a, T> {
    /// The resolved `(alias_key, instance)`, or `None` when nothing matched.
    pub fn entry(self) -> Option<(&'a str, &'a T)> {
        match self {
            Resolved::Alias { key, value } | Resolved::Fallback { key, value } => {
                Some((key, value))
            }
            Resolved::NotFound => None,
        }
    }

    /// `true` when resolved via the deprecated bare path.
    pub fn is_fallback(self) -> bool {
        matches!(self, Resolved::Fallback { .. })
    }
}

/// Resolve an optional path `<alias>` against a map of channel instances.
///
/// - `Some(alias)` → exact lookup: [`Resolved::Alias`] or [`Resolved::NotFound`].
/// - `None` (bare path) → first configured instance as [`Resolved::Fallback`],
///   or [`Resolved::NotFound`] when nothing is configured.
///
/// Single-instance configs stay fully back-compatible: the map holds exactly one
/// entry, so the bare-path fallback is deterministic and behaves as before.
pub fn resolve<'a, T>(map: &'a HashMap<String, T>, alias: Option<&str>) -> Resolved<'a, T> {
    match alias {
        Some(alias) => match map.get_key_value(alias) {
            Some((key, value)) => Resolved::Alias { key, value },
            None => Resolved::NotFound,
        },
        None => match map.iter().next() {
            Some((key, value)) => Resolved::Fallback { key, value },
            None => Resolved::NotFound,
        },
    }
}

/// Plain `404` for an unresolved webhook target. Deliberately generic so it does
/// not disclose which aliases exist for the channel type.
pub fn not_found(channel_type: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "error": format!("No matching {channel_type} webhook target configured"),
        })),
    )
        .into_response()
}

/// The `(name, value)` deprecation header pair for a bare-path response.
pub fn deprecation_header(channel_type: &str) -> (HeaderName, HeaderValue) {
    let value = HeaderValue::from_str(&format!(
        "bare /{channel_type} webhook path is deprecated; use /{channel_type}/<alias>"
    ))
    .unwrap_or_else(|_| HeaderValue::from_static("bare webhook path is deprecated"));
    (HeaderName::from_static(DEPRECATION_HEADER), value)
}

/// Apply the bare-path deprecation header to `resp` when `resolved` came from the
/// deprecated fallback path; otherwise return `resp` unchanged.
pub fn tag_deprecation<T>(
    mut resp: Response,
    resolved: Resolved<'_, T>,
    channel_type: &str,
) -> Response {
    if resolved.is_fallback() {
        let (name, value) = deprecation_header(channel_type);
        resp.headers_mut().insert(name, value);
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> HashMap<String, i32> {
        HashMap::from([("work".to_string(), 1), ("personal".to_string(), 2)])
    }

    #[test]
    fn explicit_alias_hits_matching_instance() {
        let m = map();
        match resolve(&m, Some("personal")) {
            Resolved::Alias { key, value } => {
                assert_eq!(key, "personal");
                assert_eq!(*value, 2);
            }
            _ => panic!("expected Alias"),
        }
    }

    #[test]
    fn unknown_alias_is_not_found() {
        let m = map();
        assert!(matches!(resolve(&m, Some("nope")), Resolved::NotFound));
    }

    #[test]
    fn bare_path_falls_back_to_an_instance() {
        let m = map();
        let resolved = resolve(&m, None);
        assert!(resolved.is_fallback());
        let (key, _) = resolved.entry().expect("fallback entry");
        assert!(m.contains_key(key));
    }

    #[test]
    fn single_instance_bare_path_is_deterministic() {
        let m = HashMap::from([("default".to_string(), 7)]);
        let (key, value) = resolve(&m, None).entry().expect("entry");
        assert_eq!(key, "default");
        assert_eq!(*value, 7);
    }

    #[test]
    fn empty_map_is_not_found_either_way() {
        let m: HashMap<String, i32> = HashMap::new();
        assert!(matches!(resolve(&m, None), Resolved::NotFound));
        assert!(matches!(resolve(&m, Some("x")), Resolved::NotFound));
    }
}
