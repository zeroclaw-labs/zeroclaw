//! `gemini-live` — a focused Rust client for the Google Gemini Live API.
//!
//! Owns everything about talking to Gemini Live correctly: wire types, setup
//! serialization, server-message parsing (including affective tokens), the
//! WebSocket transport (proxy + TLS + close diagnostics), and the
//! reconnect/resumption session driver. Consumers drive it through the async
//! event API in [`session`].
//!
//! The layers are populated task-by-task per the kutsu implementation plan.

pub mod session;
pub mod transport;
pub mod types;
pub mod wire;
