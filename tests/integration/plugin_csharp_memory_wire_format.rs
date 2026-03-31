//! Verify acceptance criterion for story US-ZCL-38:
//!
//! > JSON request/response marshalling matches the Rust SDK's wire format exactly
//!
//! Reads both the Rust SDK memory module and the C# SDK Memory class, then
//! asserts that every request/response struct uses identical field names,
//! types, and snake_case serialization so the two SDKs are wire-compatible.

use std::path::Path;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn read_rust_memory_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("crates/zeroclaw-plugin-sdk/src/memory.rs");
    assert!(
        path.is_file(),
        "Rust SDK memory.rs not found at {}",
        path.display()
    );
    std::fs::read_to_string(&path).expect("failed to read Rust memory.rs")
}

fn read_csharp_memory_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("sdks/csharp/src/Memory.cs");
    assert!(
        path.is_file(),
        "C# SDK Memory.cs not found at {}",
        path.display()
    );
    std::fs::read_to_string(&path).expect("failed to read C# Memory.cs")
}

fn read_csharp_serialization_tests() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("sdks/csharp/tests/MemorySerializationTests.cs");
    assert!(
        path.is_file(),
        "MemorySerializationTests.cs not found at {}",
        path.display()
    );
    std::fs::read_to_string(&path).expect("failed to read MemorySerializationTests.cs")
}

// ---------------------------------------------------------------------------
// C# Memory.cs uses snake_case JSON policy
// ---------------------------------------------------------------------------

#[test]
fn csharp_memory_uses_snake_case_json_policy() {
    let src = read_csharp_memory_source();

    assert!(
        src.contains("JsonNamingPolicy.SnakeCaseLower"),
        "Memory.cs must use SnakeCaseLower naming policy to match Rust serde defaults"
    );
    assert!(
        src.contains("PropertyNameCaseInsensitive = true"),
        "Memory.cs must enable case-insensitive deserialization"
    );
}

// ---------------------------------------------------------------------------
// StoreRequest — same fields in Rust and C#
// ---------------------------------------------------------------------------

#[test]
fn store_request_fields_match_rust_sdk() {
    let rust = read_rust_memory_source();
    let csharp = read_csharp_memory_source();

    // Rust: struct MemoryStoreRequest { key, value }
    assert!(rust.contains("struct MemoryStoreRequest"));
    assert!(rust.contains("key: String"));
    assert!(rust.contains("value: String"));

    // C#: class StoreRequest { Key, Value }  (serialized to snake_case)
    assert!(
        csharp.contains("class StoreRequest"),
        "C# must define StoreRequest mirroring Rust MemoryStoreRequest"
    );
    assert!(
        csharp.contains("public string Key { get; set; }"),
        "StoreRequest.Key must be a string property"
    );
    assert!(
        csharp.contains("public string Value { get; set; }"),
        "StoreRequest.Value must be a string property"
    );
}

// ---------------------------------------------------------------------------
// StoreResponse — same fields in Rust and C#
// ---------------------------------------------------------------------------

#[test]
fn store_response_fields_match_rust_sdk() {
    let rust = read_rust_memory_source();
    let csharp = read_csharp_memory_source();

    // Rust: struct MemoryStoreResponse { success: bool, error: Option<String> }
    assert!(rust.contains("struct MemoryStoreResponse"));
    assert!(rust.contains("success: bool"));

    // C#: class StoreResponse { Success, Error }
    assert!(
        csharp.contains("class StoreResponse"),
        "C# must define StoreResponse mirroring Rust MemoryStoreResponse"
    );
    assert!(
        csharp.contains("public bool Success { get; set; }"),
        "StoreResponse.Success must be a bool property"
    );
    assert!(
        csharp.contains("public string? Error { get; set; }"),
        "StoreResponse.Error must be a nullable string (mirrors Rust Option<String>)"
    );
}

// ---------------------------------------------------------------------------
// RecallRequest — same fields in Rust and C#
// ---------------------------------------------------------------------------

#[test]
fn recall_request_fields_match_rust_sdk() {
    let rust = read_rust_memory_source();
    let csharp = read_csharp_memory_source();

    // Rust: struct MemoryRecallRequest { query: String }
    assert!(rust.contains("struct MemoryRecallRequest"));
    assert!(rust.contains("query: String"));

    // C#: class RecallRequest { Query }
    assert!(
        csharp.contains("class RecallRequest"),
        "C# must define RecallRequest mirroring Rust MemoryRecallRequest"
    );
    assert!(
        csharp.contains("public string Query { get; set; }"),
        "RecallRequest.Query must be a string property"
    );
}

// ---------------------------------------------------------------------------
// RecallResponse — same fields in Rust and C#
// ---------------------------------------------------------------------------

#[test]
fn recall_response_fields_match_rust_sdk() {
    let rust = read_rust_memory_source();
    let csharp = read_csharp_memory_source();

    // Rust: struct MemoryRecallResponse { results: String, error: Option<String> }
    assert!(rust.contains("struct MemoryRecallResponse"));

    // C#: class RecallResponse { Results, Error }
    assert!(
        csharp.contains("class RecallResponse"),
        "C# must define RecallResponse mirroring Rust MemoryRecallResponse"
    );
    assert!(
        csharp.contains("public string Results { get; set; }"),
        "RecallResponse.Results must be a string property"
    );
    assert!(
        csharp.contains("public string? Error { get; set; }"),
        "RecallResponse.Error must be a nullable string (mirrors Rust Option<String>)"
    );
}

// ---------------------------------------------------------------------------
// ForgetRequest — same fields in Rust and C#
// ---------------------------------------------------------------------------

#[test]
fn forget_request_fields_match_rust_sdk() {
    let rust = read_rust_memory_source();
    let csharp = read_csharp_memory_source();

    // Rust: struct MemoryForgetRequest { key: String }
    assert!(rust.contains("struct MemoryForgetRequest"));
    assert!(rust.contains("key: String"));

    // C#: class ForgetRequest { Key }
    assert!(
        csharp.contains("class ForgetRequest"),
        "C# must define ForgetRequest mirroring Rust MemoryForgetRequest"
    );
    assert!(
        csharp.contains("public string Key { get; set; }"),
        "ForgetRequest.Key must be a string property"
    );
}

// ---------------------------------------------------------------------------
// ForgetResponse — same fields in Rust and C#
// ---------------------------------------------------------------------------

#[test]
fn forget_response_fields_match_rust_sdk() {
    let rust = read_rust_memory_source();
    let csharp = read_csharp_memory_source();

    // Rust: struct MemoryForgetResponse { success: bool, error: Option<String> }
    assert!(rust.contains("struct MemoryForgetResponse"));

    // C#: class ForgetResponse { Success, Error }
    assert!(
        csharp.contains("class ForgetResponse"),
        "C# must define ForgetResponse mirroring Rust MemoryForgetResponse"
    );
    assert!(
        csharp.contains("public bool Success { get; set; }"),
        "ForgetResponse.Success must be a bool property"
    );
    assert!(
        csharp.contains("public string? Error { get; set; }"),
        "ForgetResponse.Error must be a nullable string (mirrors Rust Option<String>)"
    );
}

// ---------------------------------------------------------------------------
// Host function names match between Rust and C#
// ---------------------------------------------------------------------------

#[test]
fn host_function_names_match_rust_sdk() {
    let rust = read_rust_memory_source();
    let csharp = read_csharp_memory_source();

    for fn_name in [
        "zeroclaw_memory_store",
        "zeroclaw_memory_recall",
        "zeroclaw_memory_forget",
    ] {
        assert!(
            rust.contains(fn_name),
            "Rust SDK must import host function {fn_name}"
        );
        assert!(
            csharp.contains(fn_name),
            "C# SDK must import host function {fn_name}"
        );
    }
}

// ---------------------------------------------------------------------------
// C# uses System.Text.Json (same as Rust uses serde_json)
// ---------------------------------------------------------------------------

#[test]
fn csharp_memory_uses_system_text_json() {
    let src = read_csharp_memory_source();

    assert!(
        src.contains("using System.Text.Json"),
        "Memory.cs must use System.Text.Json for JSON marshalling"
    );
    assert!(
        src.contains("JsonSerializer.SerializeToUtf8Bytes("),
        "Memory.cs must serialize requests to UTF-8 bytes"
    );
    assert!(
        src.contains("JsonSerializer.Deserialize<"),
        "Memory.cs must deserialize responses with typed generics"
    );
}

// ---------------------------------------------------------------------------
// Serialization test suite exists and covers wire format
// ---------------------------------------------------------------------------

#[test]
fn serialization_tests_cover_all_operations() {
    let src = read_csharp_serialization_tests();

    // Store
    assert!(
        src.contains("StoreRequest_Serializes_SnakeCase")
            || src.contains("StoreRequest_MatchesRustWireFormat"),
        "Serialization tests must cover StoreRequest wire format"
    );
    assert!(
        src.contains("StoreResponse_Deserializes_Success"),
        "Serialization tests must cover StoreResponse success path"
    );
    assert!(
        src.contains("StoreResponse_Deserializes_Error"),
        "Serialization tests must cover StoreResponse error path"
    );

    // Recall
    assert!(
        src.contains("RecallRequest_Serializes_SnakeCase")
            || src.contains("RecallRequest_MatchesRustWireFormat"),
        "Serialization tests must cover RecallRequest wire format"
    );
    assert!(
        src.contains("RecallResponse_Deserializes_WithResults"),
        "Serialization tests must cover RecallResponse success path"
    );
    assert!(
        src.contains("RecallResponse_Deserializes_Error"),
        "Serialization tests must cover RecallResponse error path"
    );

    // Forget
    assert!(
        src.contains("ForgetRequest_Serializes_SnakeCase")
            || src.contains("ForgetRequest_MatchesRustWireFormat"),
        "Serialization tests must cover ForgetRequest wire format"
    );
    assert!(
        src.contains("ForgetResponse_Deserializes_Success"),
        "Serialization tests must cover ForgetResponse success path"
    );
    assert!(
        src.contains("ForgetResponse_Deserializes_Error"),
        "Serialization tests must cover ForgetResponse error path"
    );
}

#[test]
fn serialization_tests_use_snake_case_json_options() {
    let src = read_csharp_serialization_tests();

    assert!(
        src.contains("JsonNamingPolicy.SnakeCaseLower"),
        "Serialization tests must configure SnakeCaseLower to match Rust wire format"
    );
    assert!(
        src.contains("DoesNotContain(\"\\\"Key\\\"\"")
            || src.contains("DoesNotContain(\"\\\"Value\\\"\""),
        "Serialization tests must assert PascalCase keys are absent"
    );
}
