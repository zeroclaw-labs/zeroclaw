//! Structured channel-login lifecycle events.
//!
//! QR-pairing channels (WeChat, WhatsApp Web) historically surfaced login
//! artifacts — QR payloads, pair codes, connection state — only as terminal
//! output. Headless deployments (gateway-managed daemons, web dashboards)
//! cannot capture stdout, so operators had no way to complete pairing
//! remotely.
//!
//! [`LoginEvent::emit`] publishes the same lifecycle as structured
//! `record!` events. Each event carries an `attributes.login` object with a
//! stable machine-readable shape, so gateway SSE (`/api/events`) consumers
//! can render the QR code client-side and track connection state:
//!
//! ```json
//! {
//!   "event": { "category": "channel", "action": "note" },
//!   "attributes": {
//!     "login": {
//!       "state": "qr",
//!       "channel": "wechat.assistant",
//!       "channel_type": "wechat",
//!       "channel_alias": "assistant",
//!       "qr_payload": "https://…",
//!       "attempt": 1,
//!       "max_attempts": 3
//!     }
//!   }
//! }
//! ```
//!
//! `state` transitions: `qr` / `pair_code` → (`scanned`) → `connected`,
//! with `expired` on QR refresh, `failed` when the flow gives up, and
//! `logged_out` when a previously linked session is revoked remotely.
//!
//! ## Credential handling
//!
//! QR payloads and pair codes are short-lived device-linking credentials.
//! They ride as **ephemeral attributes** (`Event::with_ephemeral_attrs`):
//! the live broadcast copy — the bearer-authenticated SSE `/api/events`
//! stream, shown above — carries them so a dashboard can render the QR,
//! but they are never serialized into the persisted JSONL trace, so
//! `/api/logs`, log rotation/retention, and any log shipping or backup
//! never capture them at rest. Persisted events keep only the lifecycle
//! `state`, channel identity, and attempt counters.

use serde_json::Value;
use zeroclaw_log::{Action, Event, EventCategory, EventOutcome, record};

/// A point on the channel-login lifecycle. Construct the variant that
/// matches the flow state and call [`LoginEvent::emit`].
#[derive(Debug)]
pub enum LoginEvent<'a> {
    /// A fresh QR code is ready to scan.
    Qr {
        /// Raw payload to encode into a scannable QR image client-side.
        payload: &'a str,
        /// Pre-rendered QR image URL when the platform serves one.
        image_url: Option<&'a str>,
        /// Refresh counters when the flow caps QR refreshes (WeChat);
        /// `None` for flows that refresh indefinitely (WhatsApp Web).
        attempt: Option<u32>,
        max_attempts: Option<u32>,
    },
    /// A phone-number pair code is ready to type into the app.
    PairCode { code: &'a str },
    /// The QR code was scanned; waiting for in-app confirmation.
    Scanned,
    /// The QR code expired; a refresh follows while attempts remain.
    Expired { attempt: u32, max_attempts: u32 },
    /// Login confirmed — the channel is connected.
    Connected,
    /// The login flow gave up (attempts exhausted or fatal error).
    Failed { reason: &'a str },
    /// A previously linked session was revoked remotely.
    LoggedOut,
}

impl LoginEvent<'_> {
    /// Stable `attributes.login.state` discriminator for this variant.
    #[must_use]
    pub fn state(&self) -> &'static str {
        match self {
            Self::Qr { .. } => "qr",
            Self::PairCode { .. } => "pair_code",
            Self::Scanned => "scanned",
            Self::Expired { .. } => "expired",
            Self::Connected => "connected",
            Self::Failed { .. } => "failed",
            Self::LoggedOut => "logged_out",
        }
    }

    /// Build the persisted `attributes` payload: lifecycle state, channel
    /// identity, and counters only — never pairing credentials. Channel
    /// identity is derived from the caller's values at emission time —
    /// nothing is cached here.
    fn attrs(&self, channel_type: &str, channel_alias: &str) -> Value {
        let mut login = serde_json::json!({
            "state": self.state(),
            "channel": format!("{channel_type}.{channel_alias}"),
            "channel_type": channel_type,
            "channel_alias": channel_alias,
        });
        match self {
            Self::Qr {
                attempt,
                max_attempts,
                ..
            } => {
                if let Some(attempt) = attempt {
                    login["attempt"] = (*attempt).into();
                }
                if let Some(max) = max_attempts {
                    login["max_attempts"] = (*max).into();
                }
            }
            Self::Expired {
                attempt,
                max_attempts,
            } => {
                login["attempt"] = (*attempt).into();
                login["max_attempts"] = (*max_attempts).into();
            }
            Self::Failed { reason } => {
                login["reason"] = (*reason).into();
            }
            Self::PairCode { .. } | Self::Scanned | Self::Connected | Self::LoggedOut => {}
        }
        serde_json::json!({ "login": login })
    }

    /// Build the broadcast-only payload carrying the short-lived pairing
    /// credentials (QR payload / image URL, pair code). Deep-merged into
    /// `attributes.login` on the SSE copy by the log writer; never lands
    /// in the persisted JSONL trace. `None` for credential-free states.
    fn ephemeral_attrs(&self) -> Option<Value> {
        let login = match self {
            Self::Qr {
                payload, image_url, ..
            } => {
                let mut login = serde_json::json!({ "qr_payload": payload });
                if let Some(url) = image_url {
                    login["qr_image_url"] = (*url).into();
                }
                login
            }
            Self::PairCode { code } => serde_json::json!({ "pair_code": code }),
            Self::Scanned
            | Self::Expired { .. }
            | Self::Connected
            | Self::Failed { .. }
            | Self::LoggedOut => return None,
        };
        Some(serde_json::json!({ "login": login }))
    }

    /// Emit this lifecycle point as a structured log event. Live SSE
    /// consumers can drive pairing UIs remotely (credentials ride the
    /// broadcast-only ephemeral path); JSONL consumers see lifecycle
    /// state and identity without the pairing credentials.
    pub fn emit(&self, channel_type: &str, channel_alias: &str, message: &str) {
        let attrs = self.attrs(channel_type, channel_alias);
        match self {
            Self::Connected => record!(
                INFO,
                Event::new(module_path!(), Action::Connect)
                    .with_category(EventCategory::Channel)
                    .with_outcome(EventOutcome::Success)
                    .with_attrs(attrs),
                message
            ),
            Self::Failed { .. } => record!(
                ERROR,
                Event::new(module_path!(), Action::Fail)
                    .with_category(EventCategory::Channel)
                    .with_outcome(EventOutcome::Failure)
                    .with_attrs(attrs),
                message
            ),
            Self::LoggedOut => record!(
                WARN,
                Event::new(module_path!(), Action::Disconnect)
                    .with_category(EventCategory::Channel)
                    .with_attrs(attrs),
                message
            ),
            Self::Expired { .. } => record!(
                INFO,
                Event::new(module_path!(), Action::Retry)
                    .with_category(EventCategory::Channel)
                    .with_attrs(attrs),
                message
            ),
            Self::Qr { .. } | Self::PairCode { .. } | Self::Scanned => {
                let mut payload = Event::new(module_path!(), Action::Note)
                    .with_category(EventCategory::Channel)
                    .with_attrs(attrs);
                if let Some(ephemeral) = self.ephemeral_attrs() {
                    payload = payload.with_ephemeral_attrs(ephemeral);
                }
                record!(INFO, payload, message);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_strings_are_stable() {
        assert_eq!(
            LoginEvent::Qr {
                payload: "p",
                image_url: None,
                attempt: Some(1),
                max_attempts: Some(3)
            }
            .state(),
            "qr"
        );
        assert_eq!(LoginEvent::PairCode { code: "c" }.state(), "pair_code");
        assert_eq!(LoginEvent::Scanned.state(), "scanned");
        assert_eq!(
            LoginEvent::Expired {
                attempt: 1,
                max_attempts: 3
            }
            .state(),
            "expired"
        );
        assert_eq!(LoginEvent::Connected.state(), "connected");
        assert_eq!(LoginEvent::Failed { reason: "r" }.state(), "failed");
        assert_eq!(LoginEvent::LoggedOut.state(), "logged_out");
    }

    #[test]
    fn qr_attrs_carry_identity_and_counters_but_never_credentials() {
        let event = LoginEvent::Qr {
            payload: "https://example.invalid/qr",
            image_url: Some("https://example.invalid/qr.png"),
            attempt: Some(2),
            max_attempts: Some(3),
        };
        let attrs = event.attrs("wechat", "assistant");

        let login = &attrs["login"];
        assert_eq!(login["state"], "qr");
        assert_eq!(login["channel"], "wechat.assistant");
        assert_eq!(login["channel_type"], "wechat");
        assert_eq!(login["channel_alias"], "assistant");
        assert_eq!(login["attempt"], 2);
        assert_eq!(login["max_attempts"], 3);
        // Credentials must never appear on the persisted payload.
        assert!(login.get("qr_payload").is_none());
        assert!(login.get("qr_image_url").is_none());
    }

    #[test]
    fn qr_ephemeral_attrs_carry_credentials() {
        let event = LoginEvent::Qr {
            payload: "https://example.invalid/qr",
            image_url: Some("https://example.invalid/qr.png"),
            attempt: Some(2),
            max_attempts: Some(3),
        };
        let ephemeral = event.ephemeral_attrs().expect("qr has ephemeral attrs");
        assert_eq!(
            ephemeral["login"]["qr_payload"],
            "https://example.invalid/qr"
        );
        assert_eq!(
            ephemeral["login"]["qr_image_url"],
            "https://example.invalid/qr.png"
        );
    }

    #[test]
    fn qr_attrs_omit_absent_optional_fields() {
        let event = LoginEvent::Qr {
            payload: "payload",
            image_url: None,
            attempt: None,
            max_attempts: None,
        };
        let attrs = event.attrs("whatsapp", "assistant");
        assert!(attrs["login"].get("attempt").is_none());
        assert!(attrs["login"].get("max_attempts").is_none());
        let ephemeral = event.ephemeral_attrs().expect("qr has ephemeral attrs");
        assert!(ephemeral["login"].get("qr_image_url").is_none());
    }

    #[test]
    fn pair_code_rides_only_the_ephemeral_path() {
        let event = LoginEvent::PairCode { code: "ABCD-1234" };
        let attrs = event.attrs("whatsapp", "assistant");
        assert_eq!(attrs["login"]["state"], "pair_code");
        assert!(attrs["login"].get("pair_code").is_none());
        let ephemeral = event.ephemeral_attrs().expect("pair code is ephemeral");
        assert_eq!(ephemeral["login"]["pair_code"], "ABCD-1234");
    }

    #[test]
    fn credential_free_states_have_no_ephemeral_attrs() {
        for event in [
            LoginEvent::Scanned,
            LoginEvent::Expired {
                attempt: 1,
                max_attempts: 3,
            },
            LoginEvent::Connected,
            LoginEvent::Failed { reason: "r" },
            LoginEvent::LoggedOut,
        ] {
            assert!(event.ephemeral_attrs().is_none(), "{:?}", event.state());
        }
    }

    #[test]
    fn failed_attrs_carry_reason() {
        let attrs = LoginEvent::Failed {
            reason: "QR expired 3 times",
        }
        .attrs("wechat", "assistant");
        assert_eq!(attrs["login"]["state"], "failed");
        assert_eq!(attrs["login"]["reason"], "QR expired 3 times");
    }

    #[test]
    fn plain_states_carry_only_identity() {
        for event in [
            LoginEvent::Scanned,
            LoginEvent::Connected,
            LoginEvent::LoggedOut,
        ] {
            let attrs = event.attrs("wechat", "assistant");
            let login = attrs["login"].as_object().expect("login object");
            assert_eq!(login.len(), 4, "state + 3 identity keys: {login:?}");
        }
    }
}
