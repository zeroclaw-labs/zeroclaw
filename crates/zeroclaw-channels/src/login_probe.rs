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

use zeroclaw_config::schema::Config;

/// Result of a persisted-login probe for one channel alias.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistedLogin {
    /// The channel found its persisted login/session signal on disk; it
    /// will resume the linked session instead of asking for a QR scan.
    Present,
    /// The channel supports persisted logins but none is stored; the next
    /// channel start begins a fresh QR pairing.
    Absent,
    /// The channel type has no channel-owned persisted-login probe, or the
    /// channel feature is not compiled into this binary.
    Unsupported,
}

/// Probe the persisted login state for a channel alias.
///
/// `compiled_key` uses the same per-alias key space as
/// [`crate::listing::is_channel_type_compiled`] (`"wechat"`,
/// `"whatsapp-web"`, ...), so callers that already distinguish the two
/// WhatsApp backends keep a single dispatch value for both questions.
pub fn persisted_login(compiled_key: &str, config: &Config, alias: &str) -> PersistedLogin {
    // Read at use-time in the feature-gated arms below; the binding keeps
    // the signature stable when no QR-pairing channel feature is compiled.
    let (_config, _alias) = (config, alias);
    match compiled_key {
        #[cfg(feature = "channel-wechat")]
        "wechat" => {
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
        "whatsapp-web" => {
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
                // The `whatsapp-web` compiled key is only selected for
                // aliases whose config carries a `session_path`; without
                // one there is nothing on disk to resume.
                None => PersistedLogin::Absent,
            }
        }
        _ => PersistedLogin::Unsupported,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_without_a_probe_report_unsupported() {
        let config = Config::default();
        assert_eq!(
            persisted_login("discord", &config, "default"),
            PersistedLogin::Unsupported
        );
        assert_eq!(
            persisted_login("whatsapp", &config, "default"),
            PersistedLogin::Unsupported,
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
            persisted_login("wechat", &config, "admin"),
            PersistedLogin::Absent
        );

        std::fs::write(
            temp.path().join("account.json"),
            r#"{"token": "tok_persisted", "account_id": "acct_1"}"#,
        )
        .unwrap();
        assert_eq!(
            persisted_login("wechat", &config, "admin"),
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
            persisted_login("whatsapp-web", &config, "admin"),
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
            persisted_login("whatsapp-web", &config, "admin"),
            PersistedLogin::Absent,
            "a persisted but unlinked device (pre-pairing) is not a login"
        );

        let mut device = CoreDevice::new();
        device.pn = Some(wacore_binary::jid::Jid::pn("15551234567"));
        DeviceStoreTrait::save(&store, &device).await.unwrap();
        assert_eq!(
            persisted_login("whatsapp-web", &config, "admin"),
            PersistedLogin::Present
        );
    }
}
