//! Verify that the zeroclaw-plugin-sdk memory module wraps store, recall, and
//! forget host functions.
//!
//! Acceptance criterion for US-ZCL-27:
//! > memory module wraps store recall forget host functions

use std::path::Path;

const SDK_DIR: &str = "crates/zeroclaw-plugin-sdk";

fn sdk_memory_source() -> String {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join(SDK_DIR);
    let memory_rs = base.join("src/memory.rs");
    assert!(
        memory_rs.is_file(),
        "zeroclaw-plugin-sdk/src/memory.rs is missing — cannot verify memory wrappers"
    );
    std::fs::read_to_string(&memory_rs).expect("failed to read memory.rs")
}

// ---------------------------------------------------------------------------
// Wrapper function existence
// ---------------------------------------------------------------------------

#[test]
fn memory_module_has_store_function() {
    let src = sdk_memory_source();
    assert!(
        src.contains("fn store") || src.contains("fn memory_store"),
        "memory module must expose a store wrapper function"
    );
}

#[test]
fn memory_module_has_recall_function() {
    let src = sdk_memory_source();
    assert!(
        src.contains("fn recall") || src.contains("fn memory_recall"),
        "memory module must expose a recall wrapper function"
    );
}

#[test]
fn memory_module_has_forget_function() {
    let src = sdk_memory_source();
    assert!(
        src.contains("fn forget") || src.contains("fn memory_forget"),
        "memory module must expose a forget wrapper function"
    );
}

// ---------------------------------------------------------------------------
// Host function imports — the module must declare the extern host functions
// ---------------------------------------------------------------------------

#[test]
fn memory_module_imports_zeroclaw_memory_store() {
    let src = sdk_memory_source();
    assert!(
        src.contains("zeroclaw_memory_store"),
        "memory module must reference the zeroclaw_memory_store host function"
    );
}

#[test]
fn memory_module_imports_zeroclaw_memory_recall() {
    let src = sdk_memory_source();
    assert!(
        src.contains("zeroclaw_memory_recall"),
        "memory module must reference the zeroclaw_memory_recall host function"
    );
}

#[test]
fn memory_module_imports_zeroclaw_memory_forget() {
    let src = sdk_memory_source();
    assert!(
        src.contains("zeroclaw_memory_forget"),
        "memory module must reference the zeroclaw_memory_forget host function"
    );
}

// ---------------------------------------------------------------------------
// The wrappers should use typed request/response structs (JSON ABI)
// ---------------------------------------------------------------------------

#[test]
fn memory_module_uses_store_request_struct() {
    let src = sdk_memory_source();
    assert!(
        src.contains("MemoryStoreRequest") || (src.contains("key") && src.contains("value")),
        "store wrapper should serialize a typed request with key and value fields"
    );
}

#[test]
fn memory_module_uses_recall_request_struct() {
    let src = sdk_memory_source();
    assert!(
        src.contains("MemoryRecallRequest") || src.contains("query"),
        "recall wrapper should serialize a typed request with a query field"
    );
}

#[test]
fn memory_module_uses_forget_request_struct() {
    let src = sdk_memory_source();
    assert!(
        src.contains("MemoryForgetRequest") || (src.contains("key") && src.contains("forget")),
        "forget wrapper should serialize a typed request with a key field"
    );
}

// ---------------------------------------------------------------------------
// Public API — all three wrappers should be pub
// ---------------------------------------------------------------------------

#[test]
fn memory_store_is_public() {
    let src = sdk_memory_source();
    assert!(
        src.contains("pub fn store") || src.contains("pub fn memory_store"),
        "store wrapper must be pub so plugin authors can call it"
    );
}

#[test]
fn memory_recall_is_public() {
    let src = sdk_memory_source();
    assert!(
        src.contains("pub fn recall") || src.contains("pub fn memory_recall"),
        "recall wrapper must be pub so plugin authors can call it"
    );
}

#[test]
fn memory_forget_is_public() {
    let src = sdk_memory_source();
    assert!(
        src.contains("pub fn forget") || src.contains("pub fn memory_forget"),
        "forget wrapper must be pub so plugin authors can call it"
    );
}
