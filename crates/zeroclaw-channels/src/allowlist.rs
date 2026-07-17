//! Shared sender-allowlist matching used by every chat channel.
//!
//! The matcher primitives live in [`zeroclaw_config::allowlist`] so the plugin
//! runtime can enforce the same authorization without introducing a dependency
//! from `zeroclaw-runtime` back to this channel implementation crate. Existing
//! channel call sites continue to use this module through the re-export.

pub use zeroclaw_config::allowlist::{Match, email_match, is_user_allowed, is_user_allowed_by};
