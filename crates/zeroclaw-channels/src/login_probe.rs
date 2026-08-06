//! Channel-owned persisted-login probes for QR-pairing channels.
//!
//! Answers "does this channel alias hold a persisted login/session on
//! disk?" by delegating to the channel module that owns the state — the
//! same signal each channel's startup path uses to decide between resuming
//! an existing session and starting a fresh QR pairing. Nothing is cached
//! and nothing is written: every call resolves paths from the canonical
//! `Config` and probes read-only.
//!
//! The gateway consumes this from `/api/channels` to report
//! `readiness.authenticated`. Keeping the probe here (rather than in the
//! gateway) keeps session-state knowledge inside the owning channel, and
//! keeps it out of the login lifecycle event payloads, which stay
//! lifecycle-only.

use crate::listing::QrPairingChannel;
use zeroclaw_config::schema::Config;

/// Result of a persisted-login probe for one channel alias.
///
/// The probe only runs for channels with a typed QR-pairing key
/// ([`QrPairingChannel`]); "this channel type has no probe / is not
/// compiled" is expressed by [`crate::listing::qr_pairing_channel`]
/// returning `None` at resolution time, not by a variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistedLogin {
    /// The channel found its persisted login/session signal on disk; it
    /// will resume the linked session instead of asking for a QR scan.
    Present,
    /// The channel supports persisted logins but none is stored; the next
    /// channel start begins a fresh QR pairing.
    Absent,
}

/// Probe the persisted login state for a channel alias.
///
/// Callers resolve their channel type key to [`QrPairingChannel`] once via
/// [`crate::listing::qr_pairing_channel`] and dispatch on the typed value;
/// no string key reaches this function. The match below is exhaustive over
/// the feature-gated variant set, so adding a QR-pairing channel without a
/// probe arm is a compile error rather than a silent fallthrough.
pub fn persisted_login(channel: QrPairingChannel, config: &Config, alias: &str) -> PersistedLogin {
    // Read at use-time in the feature-gated arms below; the binding keeps
    // the signature stable when no QR-pairing channel feature is compiled.
    let (_config, _alias) = (config, alias);
    match channel {
        #[cfg(feature = "channel-wechat")]
        QrPairingChannel::WeChat => {
            let state_dir = crate::wechat::WeChatChannel::resolve_state_dir(
                _config
                    .channels
                    .wechat
                    .get(_alias)
                    .and_then(|wechat| wechat.state_dir.as_deref()),
            );
            if crate::wechat::WeChatChannel::has_persisted_login(&state_dir) {
                PersistedLogin::Present
            } else {
                PersistedLogin::Absent
            }
        }
        #[cfg(feature = "whatsapp-web")]
        QrPairingChannel::WhatsAppWeb => {
            match _config
                .channels
                .whatsapp
                .get(_alias)
                .and_then(|whatsapp| whatsapp.session_path.as_deref())
            {
                Some(session_path) => {
                    if crate::whatsapp_web::WhatsAppWebChannel::has_persisted_session(session_path)
                    {
                        PersistedLogin::Present
                    } else {
                        PersistedLogin::Absent
                    }
                }
                // The WhatsApp Web key is only resolved for aliases whose
                // config carries a `session_path`; without one there is
                // nothing on disk to resume.
                None => PersistedLogin::Absent,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(any(feature = "channel-wechat", feature = "whatsapp-web"))]
    use super::{PersistedLogin, persisted_login};
    #[cfg(any(feature = "channel-wechat", feature = "whatsapp-web"))]
    use crate::listing::QrPairingChannel;
    #[cfg(any(feature = "channel-wechat", feature = "whatsapp-web"))]
    use zeroclaw_config::schema::Config;

    #[test]
    fn channels_without_a_probe_resolve_to_no_qr_pairing_key() {
        // "Unsupported" is decided at key-resolution time: channel types
        // without channel-owned QR login state never reach the probe.
        assert_eq!(crate::listing::qr_pairing_channel("discord"), None);
        assert_eq!(
            crate::listing::qr_pairing_channel("whatsapp"),
            None,
            "the Cloud API backend has no on-disk session to probe"
        );
    }

    #[cfg(feature = "channel-wechat")]
    #[test]
    fn wechat_probe_tracks_account_json_in_configured_state_dir() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.channels.wechat.insert(
            "admin".to_string(),
            zeroclaw_config::schema::WeChatConfig {
                enabled: true,
                state_dir: Some(temp.path().to_string_lossy().into_owned()),
                ..Default::default()
            },
        );

        assert_eq!(
            persisted_login(QrPairingChannel::WeChat, &config, "admin"),
            PersistedLogin::Absent
        );

        std::fs::write(
            temp.path().join("account.json"),
            r#"{"token": "tok_persisted", "account_id": "acct_1"}"#,
        )
        .unwrap();
        assert_eq!(
            persisted_login(QrPairingChannel::WeChat, &config, "admin"),
            PersistedLogin::Present
        );
    }

    #[cfg(feature = "whatsapp-web")]
    #[tokio::test]
    async fn whatsapp_web_probe_tracks_registered_device() {
        use wacore::store::Device as CoreDevice;
        use wacore::store::traits::DeviceStore as DeviceStoreTrait;

        let temp = tempfile::tempdir().unwrap();
        let session_path = temp.path().join("session.db");
        let mut config = Config::default();
        config.channels.whatsapp.insert(
            "admin".to_string(),
            zeroclaw_config::schema::WhatsAppConfig {
                enabled: true,
                session_path: Some(session_path.to_string_lossy().into_owned()),
                ..Default::default()
            },
        );

        assert_eq!(
            persisted_login(QrPairingChannel::WhatsAppWeb, &config, "admin"),
            PersistedLogin::Absent
        );
        assert!(
            !session_path.exists(),
            "probing an unpaired channel must not create the session database"
        );

        let store = crate::whatsapp_storage::RusqliteStore::new(&session_path).unwrap();
        DeviceStoreTrait::save(&store, &CoreDevice::new())
            .await
            .unwrap();
        assert_eq!(
            persisted_login(QrPairingChannel::WhatsAppWeb, &config, "admin"),
            PersistedLogin::Absent,
            "a persisted but unlinked device (pre-pairing) is not a login"
        );

        let mut device = CoreDevice::new();
        device.pn = Some(wacore_binary::jid::Jid::pn("15551234567"));
        DeviceStoreTrait::save(&store, &device).await.unwrap();
        assert_eq!(
            persisted_login(QrPairingChannel::WhatsAppWeb, &config, "admin"),
            PersistedLogin::Present
        );
    }
}
