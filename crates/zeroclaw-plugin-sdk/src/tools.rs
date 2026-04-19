//! Tools module — wraps tool_call host function.
//!
//! Serializes a typed request to JSON, calls the `zeroclaw_tool_call` host
//! function via Extism shared memory, and deserializes the JSON response.

use extism_pdk::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Request / response types (mirror the host-side structs)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct ToolCallRequest {
    tool_name: String,
    arguments: Value,
}

#[derive(Debug, Deserialize)]
struct ToolCallResponse {
    success: bool,
    #[serde(default)]
    output: String,
    #[serde(default)]
    error: Option<String>,
}

// ---------------------------------------------------------------------------
// Host function imports
// ---------------------------------------------------------------------------

#[host_fn]
extern "ExtismHost" {
    fn zeroclaw_tool_call(input: Json<ToolCallRequest>) -> Json<ToolCallResponse>;
}

// ---------------------------------------------------------------------------
// Public wrapper API
// ---------------------------------------------------------------------------

/// Call a tool by name with the given JSON arguments.
pub fn tool_call(tool_name: &str, input: serde_json::Value) -> Result<String, Error> {
    let request = ToolCallRequest {
        tool_name: tool_name.to_string(),
        arguments: input,
    };
    let Json(response) = unsafe { zeroclaw_tool_call(Json(request))? };
    if let Some(err) = response.error {
        return Err(Error::msg(err));
    }
    if !response.success {
        return Err(Error::msg("tool call returned success=false"));
    }
    Ok(response.output)
}
