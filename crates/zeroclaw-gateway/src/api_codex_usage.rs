//! On-demand Codex usage for the Agent owned by an operator's chat socket.
//! No HTTP cache, session-id lookup, or caller-supplied route/profile/endpoint.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use zeroclaw_config::{pairing::PairingGuard, schema::Config};
use zeroclaw_providers::openai_codex::usage::{
    CodexUsage, CodexUsageError, CodexUsageWindow, codex_usage_provider_for_route,
};

pub(crate) const REQUEST: &str = "check_codex_usage";

/// Provider lookup seam: production reads the native Codex route; tests
/// inject a fake so no network or auth store is touched.
#[async_trait]
pub(crate) trait CodexUsageLookup: Send + Sync {
    async fn lookup(&self, config: &Config, route: &str) -> Result<CodexUsage, CodexUsageError>;
}

pub(crate) struct NativeCodexUsage;

#[async_trait]
impl CodexUsageLookup for NativeCodexUsage {
    async fn lookup(&self, config: &Config, route: &str) -> Result<CodexUsage, CodexUsageError> {
        codex_usage_provider_for_route(config, route)?
            .fetch_usage()
            .await
    }
}

#[derive(Debug, Serialize)]
struct WindowBody {
    /// `null` means the server did not report it; it is never a stand-in for 0.
    used_percent: Option<f64>,
    resets_at: Option<DateTime<Utc>>,
}

impl From<&CodexUsageWindow> for WindowBody {
    fn from(window: &CodexUsageWindow) -> Self {
        Self {
            used_percent: window.used_percent,
            resets_at: window.resets_at,
        }
    }
}

/// Why no usage was produced, as a stable machine reason (never raw text).
fn failure_reason(error: &CodexUsageError) -> &'static str {
    use CodexUsageError as E;
    match error {
        E::UnsupportedRoute(_) => "unsupported_route",
        E::ProfileNotFound { .. }
        | E::ProfileNotOAuth
        | E::AccessTokenMissing
        | E::AccountIdMissing
        | E::AccountIdMismatch
        | E::ResponseAccountMismatch => "not_signed_in",
        E::TokenExpired => "token_expired",
        E::AuthStoreUnreadable
        | E::ClientUnavailable
        | E::Transport
        | E::HttpStatus(_)
        | E::BodyTooLarge
        | E::MalformedResponse
        | E::AmbiguousWindows
        | E::NoRecognizedWindows => "unavailable",
    }
}

pub(crate) fn request_id(frame: &Value) -> Option<u64> {
    frame["request_id"]
        .as_u64()
        .filter(|id| (1..=(1_u64 << 53) - 1).contains(id))
}

pub(crate) fn unavailable(id: u64, reason: &'static str) -> Value {
    json!({ "type": "codex_usage", "request_id": id, "status": "unavailable", "reason": reason })
}

/// Only the transports that the in-process chat admits as the operator.
/// The cookie's administrator grant must have been checked for this frame;
/// a paired bearer must still exist, not just have passed the upgrade.
pub(crate) fn operator(paired: &PairingGuard, bearer_hash: Option<&str>, admin: bool) -> bool {
    paired.require_pairing()
        && (admin || bearer_hash.is_some_and(|hash| paired.token_hash_is_paired(hash)))
}

pub(crate) async fn check<'a>(
    id: u64,
    is_operator: impl Fn() -> bool,
    resolve: impl FnOnce() -> Option<(&'a Config, &'a str)>,
    lookup: &dyn CodexUsageLookup,
) -> Value {
    if !is_operator() {
        return unavailable(id, "not_permitted");
    }
    let Some((config, route)) = resolve() else {
        return unavailable(id, "route_unavailable");
    };
    // Don't hold up the chat indefinitely. The native lookup is read-only;
    // dropping it does not initiate a refresh, write, or retry.
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        lookup.lookup(config, route),
    )
    .await;
    if !is_operator() {
        return unavailable(id, "not_permitted");
    }
    match result {
        Ok(Ok(usage)) => json!({
            "type": "codex_usage",
            "request_id": id,
            "status": "ok",
            "route": route,
            "checked_at": Utc::now(),
            "five_hour": usage.five_hour.as_ref().map(WindowBody::from),
            "weekly": usage.weekly.as_ref().map(WindowBody::from),
        }),
        Ok(Err(error)) => unavailable(id, failure_reason(&error)),
        Err(_) => unavailable(id, "unavailable"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, sync::Mutex};
    use zeroclaw_config::pairing::PairingCodePolicy;

    struct Fake {
        routes: Mutex<Vec<String>>,
        result: Result<CodexUsage, CodexUsageError>,
    }

    #[async_trait]
    impl CodexUsageLookup for Fake {
        async fn lookup(&self, _: &Config, route: &str) -> Result<CodexUsage, CodexUsageError> {
            self.routes.lock().unwrap().push(route.to_string());
            self.result.clone()
        }
    }

    fn fake() -> Fake {
        Fake {
            routes: Mutex::default(),
            result: Ok(CodexUsage {
                five_hour: Some(CodexUsageWindow {
                    used_percent: Some(0.0),
                    resets_at: DateTime::from_timestamp(1_800_000_000, 0),
                }),
                weekly: None,
            }),
        }
    }

    #[tokio::test]
    async fn codex_usage_denials_do_not_resolve_or_look_up_an_account() {
        let fake = fake();
        let token = String::from("synthetic-operator");
        let operator_hash = PairingGuard::token_hash(&token);
        let scoped_hash = PairingGuard::token_hash("synthetic-scoped-bearer");
        let paired = PairingGuard::new(
            true,
            std::slice::from_ref(&token),
            PairingCodePolicy::default(),
        );
        let open = PairingGuard::new(false, &[token], PairingCodePolicy::default());
        assert!(operator(&paired, Some(&operator_hash), false));
        paired.revoke_token_hash(&operator_hash);
        for (guard, hash, admin) in [
            (&paired, None, false), // no account or scoped dashboard session
            (&paired, Some(scoped_hash.as_str()), false),
            (&paired, Some(operator_hash.as_str()), false), // revoked since upgrade
            (&open, None, false),
            (&open, Some(operator_hash.as_str()), false),
            (&open, None, true),
        ] {
            let frame = check(
                42,
                || operator(guard, hash, admin),
                || {
                    panic!("denied caller must not reach route resolution");
                },
                &fake,
            )
            .await;
            assert_eq!(frame, unavailable(42, "not_permitted"));
        }
        assert!(fake.routes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn codex_usage_uses_live_override_not_agent_default_and_rejects_unresolved() {
        let fake = fake();
        let config = Config::default();
        let paired = PairingGuard::new(true, &[], PairingCodePolicy::default());
        let admin = || operator(&paired, None, true);
        assert_eq!(
            check(1, admin, || None, &fake).await,
            unavailable(1, "route_unavailable")
        );
        assert!(fake.routes.lock().unwrap().is_empty());
        let frame = check(2, admin, || Some((&config, "openai.override")), &fake).await;
        assert_eq!(*fake.routes.lock().unwrap(), ["openai.override"]);
        assert_eq!(frame["request_id"], 2);
        assert_eq!(frame["route"], "openai.override");
        assert_eq!(frame["five_hour"]["used_percent"], 0.0);
        assert_eq!(frame["five_hour"]["resets_at"], "2027-01-15T08:00:00Z");
        assert!(frame["weekly"].is_null());
        assert!(frame["checked_at"].is_string());
    }

    #[tokio::test]
    async fn codex_usage_drops_data_if_access_ends_during_the_lookup() {
        let calls = Cell::new(0);
        let config = Config::default();
        let frame = check(
            1,
            || {
                calls.set(calls.get() + 1);
                calls.get() == 1
            },
            || Some((&config, "openai.test")),
            &fake(),
        )
        .await;
        assert_eq!(frame, unavailable(1, "not_permitted"));
    }

    #[tokio::test]
    async fn codex_usage_does_not_return_upstream_details() {
        let fake = Fake {
            result: Err(CodexUsageError::HttpStatus(500)),
            ..fake()
        };
        let config = Config::default();
        let frame = check(1, || true, || Some((&config, "openai.test")), &fake).await;
        assert_eq!(frame, unavailable(1, "unavailable"));
    }
}
