//! Channel implementations for messaging platform integrations.
//!
//! v1 channel set (post Phase 1.5 strip):
//!   - telegram (decision #2)
//!   - slack    (decision #2)
//!   - matrix   (decision #2)
//!   - mattermost (in-house wrapper; planner decision #2)
//!   - whatsapp (Cloud API only; whatsapp-web Selenium scraper REMOVED)
//!   - signal   (M4 — wraps `signal-cli` subprocess; in-process Rust Signal
//!              SDKs are AGPL-3.0 and banned in deny.toml)
//!   - cli      (always-on; for engineer interactive mode)
//!
//! Phase 1.5 stripped 26 channels at the source level: discord, irc, email,
//! gmail-push, voice-call, voice-wake, whatsapp-web (Selenium), twitter, reddit,
//! bluesky, nostr, line, wechat, wecom, qq, dingtalk, lark, feishu, clawdtalk,
//! nextcloud, linq, wati, imessage, mochat, notion, acp-server.
//! Phase 1.4 stripped: webhook (user-rejected on security grounds).
//!
//! Phase 1.5 also stripped voice-adjacent support modules: transcription, tts.

pub mod orchestrator;
pub mod util;
pub mod cli;
pub mod link_enricher;

// v1 channel set — explicit registration only (WS-04). No distributed-slice.
#[cfg(feature = "channel-matrix")]
pub mod matrix;
#[cfg(feature = "channel-mattermost")]
pub mod mattermost;
#[cfg(feature = "channel-signal")]
pub mod signal;
#[cfg(feature = "channel-slack")]
pub mod slack;
#[cfg(feature = "channel-telegram")]
pub mod telegram;
#[cfg(feature = "channel-whatsapp-cloud")]
pub mod whatsapp;
