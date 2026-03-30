//! Memory module — wraps store, recall, and forget host functions.
//!
//! Each function serializes a typed request struct to JSON, calls the
//! corresponding `zeroclaw_memory_*` host function via Extism shared memory,
//! and deserializes the JSON response.

use extism_pdk::*;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Request / response types (mirror the host-side structs)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct MemoryStoreRequest {
    key: String,
    value: String,
}

#[derive(Debug, Deserialize)]
struct MemoryStoreResponse {
    success: bool,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct MemoryRecallRequest {
    query: String,
}

#[derive(Debug, Deserialize)]
struct MemoryRecallResponse {
    #[serde(default)]
    results: String,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct MemoryForgetRequest {
    key: String,
}

#[derive(Debug, Deserialize)]
struct MemoryForgetResponse {
    success: bool,
    #[serde(default)]
    error: Option<String>,
}

// ---------------------------------------------------------------------------
// Host function imports
// ---------------------------------------------------------------------------

#[host_fn]
extern "ExtismHost" {
    fn zeroclaw_memory_store(input: Json<MemoryStoreRequest>) -> Json<MemoryStoreResponse>;
    fn zeroclaw_memory_recall(input: Json<MemoryRecallRequest>) -> Json<MemoryRecallResponse>;
    fn zeroclaw_memory_forget(input: Json<MemoryForgetRequest>) -> Json<MemoryForgetResponse>;
}

// ---------------------------------------------------------------------------
// Public wrapper API
// ---------------------------------------------------------------------------

/// Store a key-value pair in the agent's memory.
pub fn store(key: &str, value: &str) -> Result<(), Error> {
    let request = MemoryStoreRequest {
        key: key.to_string(),
        value: value.to_string(),
    };
    let Json(response) = unsafe { zeroclaw_memory_store(Json(request))? };
    if let Some(err) = response.error {
        return Err(Error::msg(err));
    }
    if !response.success {
        return Err(Error::msg("memory store returned success=false"));
    }
    Ok(())
}

/// Recall memories matching the given query string.
pub fn recall(query: &str) -> Result<String, Error> {
    let request = MemoryRecallRequest {
        query: query.to_string(),
    };
    let Json(response) = unsafe { zeroclaw_memory_recall(Json(request))? };
    if let Some(err) = response.error {
        return Err(Error::msg(err));
    }
    Ok(response.results)
}

/// Forget (delete) a memory entry by key.
pub fn forget(key: &str) -> Result<(), Error> {
    let request = MemoryForgetRequest {
        key: key.to_string(),
    };
    let Json(response) = unsafe { zeroclaw_memory_forget(Json(request))? };
    if let Some(err) = response.error {
        return Err(Error::msg(err));
    }
    if !response.success {
        return Err(Error::msg("memory forget returned success=false"));
    }
    Ok(())
}
