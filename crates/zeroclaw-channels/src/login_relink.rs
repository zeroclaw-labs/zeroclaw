//! Channel-owned relink hooks for QR-pairing channels.
//!
//! "Relink" replaces the currently linked account: QR-pairing channels
//! (WeChat, WhatsApp Web) persist their login on disk and silently resume it
//! on every start — by design, a restart never re-runs the QR flow while a
//! session exists. So issuing a new QR necessarily means clearing the
//! persisted login first; the restarted channel then finds no session and
//! begins a fresh pairing.
//!
//! Each match arm delegates to the channel module that owns the state, so
//! knowledge of what constitutes a persisted login (which files, which
//! rows) never leaks out of the channel — the gateway endpoint that exposes
//! this dispatches here and performs no file operations of its own. Paths
//! are resolved from the canonical `Config` per call; nothing is cached.
//!
//! Channels that cannot relink — webhook-token channels, bot-token channels,
//! the WhatsApp Cloud API backend, or channels whose feature is not compiled
//! into this binary — never resolve to a [`QrPairingChannel`] key
//! ([`crate::listing::qr_pairing_channel`] returns `None`), so they never
//! reach this hook and **nothing is touched**: no files are removed, no
//! state changes, the operation is an explicit no-op the caller can surface
//! verbatim.
//!
//! Relinking only clears disk state. A currently running channel keeps its
//! in-memory session until it restarts; callers own scheduling that restart
//! (the daemon reload path), which keeps this hook free of lifecycle side
//! effects and keeps restart policy where it already lives.

use crate::listing::QrPairingChannel;
use zeroclaw_config::schema::Config;

/// Result of a relink request for one channel alias.
///
/// The hook only runs for channels with a typed QR-pairing key
/// ([`QrPairingChannel`]); "this channel type cannot relink / is not
/// compiled" is expressed by [`crate::listing::qr_pairing_channel`]
/// returning `None` at resolution time, not by a variant here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelinkOutcome {
    /// A persisted login existed and its on-disk state was removed. The
    /// next channel start begins a fresh QR pairing that replaces the
    /// previously linked account.
    Cleared {
        /// Paths that were actually removed, for operator-facing reporting.
        removed: Vec<String>,
    },
    /// The channel supports relinking but held no persisted login state;
    /// nothing was removed. The next channel start already begins a fresh
    /// QR pairing.
    NothingToClear,
}

/// Clear the persisted login state for a channel alias so its next start
/// mints a fresh QR pairing.
///
/// Callers resolve their channel type key to [`QrPairingChannel`] once via
/// [`crate::listing::qr_pairing_channel`] — the same typed key
/// [`crate::login_probe::persisted_login`] dispatches on — so probe and
/// relink share one key space and no string key reaches this function. The
/// match below is exhaustive over the feature-gated variant set, so adding
/// a QR-pairing channel without a relink arm is a compile error rather
/// than a silent fallthrough.
///
/// Errors are I/O failures from removing existing files (permissions, etc.);
/// absent files are never an error.
pub fn relink(
    channel: QrPairingChannel,
    config: &Config,
    alias: &str,
) -> anyhow::Result<RelinkOutcome> {
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
            let removed = crate::wechat::WeChatChannel::clear_persisted_login(&state_dir)?;
            if removed.is_empty() {
                Ok(RelinkOutcome::NothingToClear)
            } else {
                Ok(RelinkOutcome::Cleared { removed })
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
                    let removed = crate::whatsapp_web::WhatsAppWebChannel::clear_persisted_session(
                        session_path,
                    )?;
                    if removed.is_empty() {
                        Ok(RelinkOutcome::NothingToClear)
                    } else {
                        Ok(RelinkOutcome::Cleared { removed })
                    }
                }
                // The WhatsApp Web key is only resolved for aliases whose
                // config carries a `session_path`; without one there is
                // nothing on disk to clear.
                None => Ok(RelinkOutcome::NothingToClear),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(any(feature = "channel-wechat", feature = "whatsapp-web"))]
    use super::{RelinkOutcome, relink};
    #[cfg(any(feature = "channel-wechat", feature = "whatsapp-web"))]
    use crate::listing::QrPairingChannel;
    #[cfg(any(feature = "channel-wechat", feature = "whatsapp-web"))]
    use zeroclaw_config::schema::Config;

    #[test]
    fn channels_without_a_relink_hook_resolve_to_no_qr_pairing_key() {
        // "Unsupported" is decided at key-resolution time: channel types
        // without QR-pairing sessions never reach the relink hook, so
        // nothing can be touched for them.
        assert_eq!(crate::listing::qr_pairing_channel("discord"), None);
        assert_eq!(
            crate::listing::qr_pairing_channel("whatsapp"),
            None,
            "the Cloud API backend has no on-disk session to clear"
        );
    }

    #[cfg(feature = "channel-wechat")]
    #[test]
    fn wechat_relink_clears_state_dir_files() {
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
            relink(QrPairingChannel::WeChat, &config, "admin").unwrap(),
            RelinkOutcome::NothingToClear,
            "an unpaired channel relinks as a no-op"
        );

        std::fs::write(
            temp.path().join("account.json"),
            r#"{"token": "tok_persisted"}"#,
        )
        .unwrap();
        std::fs::write(temp.path().join("sync.json"), r#"{"get_updates_buf": "c"}"#).unwrap();

        match relink(QrPairingChannel::WeChat, &config, "admin").unwrap() {
            RelinkOutcome::Cleared { removed } => assert_eq!(removed.len(), 2),
            other => panic!("expected Cleared, got {other:?}"),
        }
        assert!(!temp.path().join("account.json").exists());
        assert!(!temp.path().join("sync.json").exists());
    }

    #[cfg(feature = "whatsapp-web")]
    #[test]
    fn whatsapp_web_relink_clears_session_without_creating_it() {
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
            relink(QrPairingChannel::WhatsAppWeb, &config, "admin").unwrap(),
            RelinkOutcome::NothingToClear
        );
        assert!(
            !session_path.exists(),
            "relinking an unpaired channel must not create the session database"
        );

        std::fs::write(&session_path, b"db").unwrap();
        match relink(QrPairingChannel::WhatsAppWeb, &config, "admin").unwrap() {
            RelinkOutcome::Cleared { removed } => assert_eq!(removed.len(), 1),
            other => panic!("expected Cleared, got {other:?}"),
        }
        assert!(!session_path.exists());
    }
}
