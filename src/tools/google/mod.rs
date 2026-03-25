//! Google API integration — direct HTTP access to Google Workspace APIs.
//!
//! Provides token caching (OAuth 2.0 refresh flow) and a Calendar API
//! client that routes calls from the `google_workspace` tool surface
//! without requiring the `gws` CLI binary.

pub mod auth;
pub mod calendar_client;
