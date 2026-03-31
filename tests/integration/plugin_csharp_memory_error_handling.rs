//! Verify acceptance criterion for story US-ZCL-38:
//!
//! > Errors from host throw PluginException with descriptive messages
//!
//! Reads the C# SDK Memory.cs and PluginException.cs sources and asserts that:
//!
//! 1. PluginException exists and extends Exception with message + inner ctor
//! 2. Every error path in Memory (store, recall, forget, helper) throws
//!    PluginException with a non-empty descriptive message
//! 3. Error handling mirrors the Rust SDK's pattern (check error field, then
//!    success flag) so both SDKs surface the same failures

use std::path::Path;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn read_csharp_memory_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("sdks/csharp/src/Memory.cs");
    assert!(
        path.is_file(),
        "C# SDK Memory.cs not found at {}",
        path.display()
    );
    std::fs::read_to_string(&path).expect("failed to read C# Memory.cs")
}

fn read_csharp_plugin_exception_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("sdks/csharp/src/PluginException.cs");
    assert!(
        path.is_file(),
        "C# SDK PluginException.cs not found at {}",
        path.display()
    );
    std::fs::read_to_string(&path).expect("failed to read C# PluginException.cs")
}

fn read_rust_memory_source() -> String {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/zeroclaw-plugin-sdk/src/memory.rs");
    assert!(
        path.is_file(),
        "Rust SDK memory.rs not found at {}",
        path.display()
    );
    std::fs::read_to_string(&path).expect("failed to read Rust memory.rs")
}

fn read_csharp_serialization_tests() -> String {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("sdks/csharp/tests/MemorySerializationTests.cs");
    assert!(
        path.is_file(),
        "MemorySerializationTests.cs not found at {}",
        path.display()
    );
    std::fs::read_to_string(&path).expect("failed to read MemorySerializationTests.cs")
}

// ---------------------------------------------------------------------------
// PluginException class structure
// ---------------------------------------------------------------------------

#[test]
fn plugin_exception_extends_exception() {
    let src = read_csharp_plugin_exception_source();

    assert!(
        src.contains("class PluginException : Exception"),
        "PluginException must extend System.Exception"
    );
}

#[test]
fn plugin_exception_has_message_constructor() {
    let src = read_csharp_plugin_exception_source();

    assert!(
        src.contains("PluginException(string message) : base(message)"),
        "PluginException must have a constructor accepting a message string"
    );
}

#[test]
fn plugin_exception_has_inner_exception_constructor() {
    let src = read_csharp_plugin_exception_source();

    assert!(
        src.contains("PluginException(string message, Exception innerException)"),
        "PluginException must have a constructor accepting message + inner exception"
    );
}

// ---------------------------------------------------------------------------
// Store — throws PluginException on error field and success=false
// ---------------------------------------------------------------------------

#[test]
fn store_throws_on_error_field() {
    let src = read_csharp_memory_source();

    // The Store method must check response.Error and throw PluginException
    assert!(
        src.contains("response.Error is not null")
            && src.contains("throw new PluginException(response.Error)"),
        "Store must throw PluginException with the host error message when response.Error is set"
    );
}

#[test]
fn store_throws_on_success_false() {
    let src = read_csharp_memory_source();

    assert!(
        src.contains("throw new PluginException(\"memory store returned success=false\")"),
        "Store must throw PluginException with descriptive message when success=false"
    );
}

// ---------------------------------------------------------------------------
// Recall — throws PluginException on error field
// ---------------------------------------------------------------------------

#[test]
fn recall_throws_on_error_field() {
    let src = read_csharp_memory_source();

    // Count occurrences of the error-check pattern — Recall should also have one
    let error_checks: Vec<_> = src.match_indices("throw new PluginException(response.Error)").collect();
    assert!(
        error_checks.len() >= 2,
        "At least Store and Recall must throw PluginException(response.Error); found {} occurrences",
        error_checks.len()
    );
}

// ---------------------------------------------------------------------------
// Forget — throws PluginException on error field and success=false
// ---------------------------------------------------------------------------

#[test]
fn forget_throws_on_error_field() {
    let src = read_csharp_memory_source();

    // All three methods (Store, Recall, Forget) must check response.Error
    let error_checks: Vec<_> = src.match_indices("throw new PluginException(response.Error)").collect();
    assert!(
        error_checks.len() >= 3,
        "Store, Recall, and Forget must all throw PluginException(response.Error); found {} occurrences",
        error_checks.len()
    );
}

#[test]
fn forget_throws_on_success_false() {
    let src = read_csharp_memory_source();

    assert!(
        src.contains("throw new PluginException(\"memory forget returned success=false\")"),
        "Forget must throw PluginException with descriptive message when success=false"
    );
}

// ---------------------------------------------------------------------------
// CallHostFunction helper — throws on empty response and deserialization failure
// ---------------------------------------------------------------------------

#[test]
fn helper_throws_on_empty_host_response() {
    let src = read_csharp_memory_source();

    assert!(
        src.contains("throw new PluginException(\"host function returned empty response\")"),
        "CallHostFunction must throw PluginException when host returns empty response"
    );
}

#[test]
fn helper_throws_on_deserialization_failure() {
    let src = read_csharp_memory_source();

    assert!(
        src.contains("throw new PluginException(\"failed to deserialize host response\")"),
        "CallHostFunction must throw PluginException when response deserialization fails"
    );
}

// ---------------------------------------------------------------------------
// All error paths use PluginException (not bare Exception or other types)
// ---------------------------------------------------------------------------

#[test]
fn all_throws_use_plugin_exception() {
    let src = read_csharp_memory_source();

    // Count all `throw new` statements — they should ALL be PluginException
    let all_throws: Vec<_> = src.match_indices("throw new ").collect();
    let plugin_throws: Vec<_> = src.match_indices("throw new PluginException(").collect();

    assert!(
        !all_throws.is_empty(),
        "Memory.cs must contain throw statements for error handling"
    );
    assert_eq!(
        all_throws.len(),
        plugin_throws.len(),
        "All throw statements in Memory.cs must throw PluginException, not other exception types; \
         found {} total throws but only {} PluginException throws",
        all_throws.len(),
        plugin_throws.len()
    );
}

// ---------------------------------------------------------------------------
// Error messages are descriptive (non-empty string literals)
// ---------------------------------------------------------------------------

#[test]
fn error_messages_are_descriptive() {
    let src = read_csharp_memory_source();

    // Ensure no empty-string PluginException throws
    assert!(
        !src.contains("throw new PluginException(\"\")"),
        "PluginException must never be thrown with an empty message"
    );

    // The host-error throws use response.Error (the actual error from host)
    assert!(
        src.contains("throw new PluginException(response.Error)"),
        "Host errors must be propagated as the PluginException message"
    );

    // The fallback throws use descriptive string constants
    for expected_msg in [
        "memory store returned success=false",
        "memory forget returned success=false",
        "host function returned empty response",
        "failed to deserialize host response",
    ] {
        assert!(
            src.contains(expected_msg),
            "Memory.cs must contain descriptive error message: {expected_msg}"
        );
    }
}

// ---------------------------------------------------------------------------
// Error handling mirrors Rust SDK pattern
// ---------------------------------------------------------------------------

#[test]
fn error_handling_order_matches_rust_sdk() {
    let rust = read_rust_memory_source();
    let csharp = read_csharp_memory_source();

    // Rust: checks error field first, then success flag (for store and forget)
    // C#: must follow the same order

    // Rust store: error check before success check
    let rust_store_error = rust.find("response.error").unwrap();
    let rust_store_success = rust.find("response.success").unwrap();
    assert!(
        rust_store_error < rust_store_success,
        "Rust SDK checks error before success in store"
    );

    // C# store: error check before success check
    let csharp_store_error = csharp.find("response.Error is not null").unwrap();
    let csharp_store_success = csharp.find("response.Success").unwrap();
    assert!(
        csharp_store_error < csharp_store_success,
        "C# SDK must check Error before Success in Store (matching Rust SDK order)"
    );
}

#[test]
fn csharp_success_false_messages_match_rust_sdk() {
    let rust = read_rust_memory_source();
    let csharp = read_csharp_memory_source();

    // Both SDKs should use the same descriptive messages for success=false cases
    assert!(
        rust.contains("memory store returned success=false"),
        "Rust SDK must have store success=false message"
    );
    assert!(
        csharp.contains("memory store returned success=false"),
        "C# SDK must use same store success=false message as Rust"
    );

    assert!(
        rust.contains("memory forget returned success=false"),
        "Rust SDK must have forget success=false message"
    );
    assert!(
        csharp.contains("memory forget returned success=false"),
        "C# SDK must use same forget success=false message as Rust"
    );
}

// ---------------------------------------------------------------------------
// C# unit tests cover error deserialization paths
// ---------------------------------------------------------------------------

#[test]
fn csharp_tests_cover_error_deserialization() {
    let tests = read_csharp_serialization_tests();

    // Tests must cover deserializing error responses for all three operations
    assert!(
        tests.contains("StoreResponse_Deserializes_Error"),
        "C# tests must validate StoreResponse error deserialization"
    );
    assert!(
        tests.contains("RecallResponse_Deserializes_Error"),
        "C# tests must validate RecallResponse error deserialization"
    );
    assert!(
        tests.contains("ForgetResponse_Deserializes_Error"),
        "C# tests must validate ForgetResponse error deserialization"
    );
}

#[test]
fn csharp_tests_cover_plugin_exception_class() {
    let tests = read_csharp_serialization_tests();

    assert!(
        tests.contains("PluginException_PreservesMessage"),
        "C# tests must validate PluginException preserves error messages"
    );
    assert!(
        tests.contains("PluginException_PreservesInnerException"),
        "C# tests must validate PluginException inner exception chaining"
    );
}
