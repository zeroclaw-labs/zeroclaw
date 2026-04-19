//! Context module — wraps session, user_identity, and agent_config host functions.
//!
//! Each function calls the corresponding `context_*` host function via Extism
//! shared memory and returns a typed response struct.

use extism_pdk::*;
use serde::Deserialize;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Response types (mirror the host-side structs)
// ---------------------------------------------------------------------------

/// Snapshot of the current session context.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionContext {
    /// The channel name the current request originated from (e.g. "telegram", "slack").
    pub channel_name: String,
    /// An opaque conversation/session identifier.
    pub conversation_id: String,
    /// ISO-8601 timestamp of the current request.
    pub timestamp: String,
}

/// Information about the user who triggered the current invocation.
#[derive(Debug, Clone, Deserialize)]
pub struct UserIdentity {
    /// The user's username (e.g. "jdoe").
    pub username: String,
    /// The user's display name (e.g. "Jane Doe").
    pub display_name: String,
    /// A channel-specific identifier for the user.
    pub channel_user_id: String,
}

/// Agent personality and identity configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    /// The agent's configured display name.
    pub name: String,
    /// Personality traits (e.g. "friendly", "concise", "technical").
    pub personality_traits: Vec<String>,
    /// Arbitrary identity fields from the agent configuration.
    pub identity: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// Host function imports
// ---------------------------------------------------------------------------

#[host_fn]
extern "ExtismHost" {
    fn context_session(input: Json<()>) -> Json<SessionContext>;
    fn context_user_identity(input: Json<()>) -> Json<UserIdentity>;
    fn context_agent_config(input: Json<()>) -> Json<AgentConfig>;
}

// ---------------------------------------------------------------------------
// Public wrapper API
// ---------------------------------------------------------------------------

/// Get the current session context (channel, conversation ID, timestamp).
pub fn session() -> Result<SessionContext, Error> {
    let Json(ctx) = unsafe { context_session(Json(()))? };
    Ok(ctx)
}

/// Get the identity of the user who triggered this invocation.
pub fn user_identity() -> Result<UserIdentity, Error> {
    let Json(identity) = unsafe { context_user_identity(Json(()))? };
    Ok(identity)
}

/// Get the agent's personality and identity configuration.
pub fn agent_config() -> Result<AgentConfig, Error> {
    let Json(config) = unsafe { context_agent_config(Json(()))? };
    Ok(config)
}
