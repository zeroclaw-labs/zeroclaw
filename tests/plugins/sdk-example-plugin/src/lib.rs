//! SDK Example Plugin — "smart greeter"
//!
//! Demonstrates a meaningful workflow using three SDK modules:
//!
//! 1. **context::session** — reads the current channel and conversation ID
//! 2. **memory::recall** — checks if we've greeted this conversation before
//! 3. **memory::store** — remembers the greeting so we don't repeat it
//! 4. **tools::tool_call** — delegates to a tool to look up a fun fact
//!
//! When invoked, the plugin greets the user with a personalised message
//! that includes session context and, for first-time conversations, fetches
//! a fun fact via tool delegation.

use extism_pdk::*;
use serde::{Deserialize, Serialize};
use zeroclaw_plugin_sdk::{context, memory, tools};

#[derive(Debug, Serialize, Deserialize)]
struct GreeterInput {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct GreeterOutput {
    greeting: String,
    channel: String,
    conversation_id: String,
    first_visit: bool,
}

#[plugin_fn]
pub fn tool_greet(input: String) -> FnResult<String> {
    let params: GreeterInput = serde_json::from_str(&input).unwrap_or(GreeterInput {
        name: "friend".to_string(),
    });

    let name = if params.name.is_empty() {
        "friend".to_string()
    } else {
        params.name
    };

    // 1. Get session context
    let session = context::session()?;

    // 2. Check if we've seen this conversation before
    let memory_key = format!("greeted:{}", session.conversation_id);
    let previous = memory::recall(&memory_key).unwrap_or_default();
    let first_visit = previous.is_empty();

    // 3. Build the greeting
    let mut greeting = format!(
        "Hello, {}! You're on the {} channel (conversation {}).",
        name, session.channel_name, session.conversation_id
    );

    if first_visit {
        // 4. For first visits, fetch a fun fact via tool delegation
        let fact = tools::tool_call(
            "fun_fact",
            serde_json::json!({ "topic": "greeting" }),
        )
        .unwrap_or_else(|_| "Waving as a greeting dates back to ancient times!".to_string());

        greeting.push_str(&format!(" Welcome! Here's a fun fact: {}", fact));

        // Remember this conversation
        let _ = memory::store(&memory_key, &name);
    } else {
        greeting.push_str(" Welcome back!");
    }

    let output = GreeterOutput {
        greeting,
        channel: session.channel_name,
        conversation_id: session.conversation_id,
        first_visit,
    };

    Ok(serde_json::to_string(&output)?)
}
