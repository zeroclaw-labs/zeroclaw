//! Hermetic contract tests for the wasmtime component-model (WIT) tool runtime.
//!
//! Each fixture is inline WebAssembly text (WAT) for a *core* module implementing
//! the canonical-ABI shape of zeroclaw's `tool-plugin` world, encoded into a
//! component at test time with `wat` + `wit-component` (reading the real
//! `wit/v0/` package). This proves the runtime end-to-end — load, metadata
//! extraction, per-call isolation, host imports, egress accounting, deny-by-
//! default, fuel/epoch deadlines — with NO prebuilt `.wasm` plugin required.
//!
//! Run with a compiler backend, e.g.:
//!   cargo test -p zeroclaw-plugins --features plugins-wasm-cranelift
//!
//! Gated on a compiler backend (cranelift/pulley): the suite calls
//! `Component::new`, which does not exist in runtime-only builds. Gating on the
//! default `plugins-wasmtime` (no backend) would make these tests fail in a
//! plain `cargo test`, so the gate requires an actual backend instead.

#![cfg(any(feature = "plugins-wasm-cranelift", feature = "plugins-wasm-pulley"))]

use std::sync::Arc;

use serde_json::json;
use wit_component::{ComponentEncoder, StringEncoding, embed_component_metadata};
use wit_parser::Resolve;
use zeroclaw_plugins::{
    DenyWasmHostHttp, RecordingWasmHostHttp, WasmError, WasmHostHttp, WasmHttpRequest,
    WasmHttpResponse, WitToolHost, WitToolRequest, WitToolRuntime, WitToolRuntimeConfig,
};

/// A counter tool: exports the full `tool-plugin` world and, on each `execute`,
/// returns `ok(tool-result)` whose output is "1" on the first call of a given
/// instance and "2" thereafter. Because the runtime builds a fresh instance per
/// call, a correct runtime always observes "1".
const COUNTER_TOOL_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (global $heap (mut i32) (i32.const 4096))
  (global $count (mut i32) (i32.const 0))
  (data (i32.const 1024) "{\22type\22:\22object\22}")
  (data (i32.const 1100) "fixture description")
  (data (i32.const 1200) "counter")
  (data (i32.const 1300) "fixture")
  (data (i32.const 1400) "0.1.0")
  (data (i32.const 3072) "1")
  (data (i32.const 3073) "2")
  (func $name (result i32)
    i32.const 16
    i32.const 1200
    i32.store
    i32.const 20
    i32.const 7
    i32.store
    i32.const 16)
  (func $description (result i32)
    i32.const 32
    i32.const 1100
    i32.store
    i32.const 36
    i32.const 19
    i32.store
    i32.const 32)
  (func $schema (result i32)
    i32.const 48
    i32.const 1024
    i32.store
    i32.const 52
    i32.const 17
    i32.store
    i32.const 48)
  (func $plugin_name (result i32)
    i32.const 64
    i32.const 1300
    i32.store
    i32.const 68
    i32.const 7
    i32.store
    i32.const 64)
  (func $plugin_version (result i32)
    i32.const 80
    i32.const 1400
    i32.store
    i32.const 84
    i32.const 5
    i32.store
    i32.const 80)
  (func $execute (param i32 i32) (result i32)
    global.get $count
    i32.const 1
    i32.add
    global.set $count
    i32.const 96
    i32.const 0
    i32.store
    i32.const 100
    i32.const 1
    i32.store
    i32.const 104
    global.get $count
    i32.const 1
    i32.eq
    if (result i32)
      i32.const 3072
    else
      i32.const 3073
    end
    i32.store
    i32.const 108
    i32.const 1
    i32.store
    i32.const 112
    i32.const 0
    i32.store
    i32.const 96)
  (func $post (param i32))
  (func $realloc (param $old i32) (param $old_align i32) (param $new_size i32) (param $new_align i32) (result i32)
    (local $ret i32)
    global.get $heap
    local.set $ret
    global.get $heap
    local.get $new_size
    i32.add
    global.set $heap
    local.get $ret)
  (func $_initialize)
  (export "zeroclaw:plugin/tool@0.1.0#name" (func $name))
  (export "cabi_post_zeroclaw:plugin/tool@0.1.0#name" (func $post))
  (export "zeroclaw:plugin/tool@0.1.0#description" (func $description))
  (export "cabi_post_zeroclaw:plugin/tool@0.1.0#description" (func $post))
  (export "zeroclaw:plugin/tool@0.1.0#parameters-schema" (func $schema))
  (export "cabi_post_zeroclaw:plugin/tool@0.1.0#parameters-schema" (func $post))
  (export "zeroclaw:plugin/tool@0.1.0#execute" (func $execute))
  (export "cabi_post_zeroclaw:plugin/tool@0.1.0#execute" (func $post))
  (export "zeroclaw:plugin/plugin-info@0.1.0#plugin-name" (func $plugin_name))
  (export "cabi_post_zeroclaw:plugin/plugin-info@0.1.0#plugin-name" (func $post))
  (export "zeroclaw:plugin/plugin-info@0.1.0#plugin-version" (func $plugin_version))
  (export "cabi_post_zeroclaw:plugin/plugin-info@0.1.0#plugin-version" (func $post))
  (export "cabi_realloc" (func $realloc))
  (export "_initialize" (func $_initialize))
)
"#;

/// An HTTP tool: identical exports to the counter, but `execute` first calls the
/// host `http-request` import (POST https://example.test/api, body "hello") and
/// then returns `ok(tool-result)` with output "1".
const HTTP_TOOL_WAT: &str = r#"
(module
  (import "zeroclaw:plugin/host@0.1.0" "http-request" (func $http_request (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)))
  (memory (export "memory") 1)
  (global $heap (mut i32) (i32.const 4096))
  (data (i32.const 128) "POST")
  (data (i32.const 160) "https://example.test/api")
  (data (i32.const 224) "{}")
  (data (i32.const 256) "hello")
  (data (i32.const 1024) "{\22type\22:\22object\22}")
  (data (i32.const 1100) "fixture description")
  (data (i32.const 1200) "http")
  (data (i32.const 1300) "fixture")
  (data (i32.const 1400) "0.1.0")
  (data (i32.const 3072) "1")
  (func $name (result i32)
    i32.const 16
    i32.const 1200
    i32.store
    i32.const 20
    i32.const 4
    i32.store
    i32.const 16)
  (func $description (result i32)
    i32.const 32
    i32.const 1100
    i32.store
    i32.const 36
    i32.const 19
    i32.store
    i32.const 32)
  (func $schema (result i32)
    i32.const 48
    i32.const 1024
    i32.store
    i32.const 52
    i32.const 17
    i32.store
    i32.const 48)
  (func $plugin_name (result i32)
    i32.const 64
    i32.const 1300
    i32.store
    i32.const 68
    i32.const 7
    i32.store
    i32.const 64)
  (func $plugin_version (result i32)
    i32.const 80
    i32.const 1400
    i32.store
    i32.const 84
    i32.const 5
    i32.store
    i32.const 80)
  (func $execute (param i32 i32) (result i32)
    i32.const 128
    i32.const 4
    i32.const 160
    i32.const 24
    i32.const 224
    i32.const 2
    i32.const 1
    i32.const 256
    i32.const 5
    i32.const 0
    i32.const 0
    i32.const 512
    call $http_request
    i32.const 96
    i32.const 0
    i32.store
    i32.const 100
    i32.const 1
    i32.store
    i32.const 104
    i32.const 3072
    i32.store
    i32.const 108
    i32.const 1
    i32.store
    i32.const 112
    i32.const 0
    i32.store
    i32.const 96)
  (func $post (param i32))
  (func $realloc (param $old i32) (param $old_align i32) (param $new_size i32) (param $new_align i32) (result i32)
    (local $ret i32)
    global.get $heap
    local.set $ret
    global.get $heap
    local.get $new_size
    i32.add
    global.set $heap
    local.get $ret)
  (func $_initialize)
  (export "zeroclaw:plugin/tool@0.1.0#name" (func $name))
  (export "cabi_post_zeroclaw:plugin/tool@0.1.0#name" (func $post))
  (export "zeroclaw:plugin/tool@0.1.0#description" (func $description))
  (export "cabi_post_zeroclaw:plugin/tool@0.1.0#description" (func $post))
  (export "zeroclaw:plugin/tool@0.1.0#parameters-schema" (func $schema))
  (export "cabi_post_zeroclaw:plugin/tool@0.1.0#parameters-schema" (func $post))
  (export "zeroclaw:plugin/tool@0.1.0#execute" (func $execute))
  (export "cabi_post_zeroclaw:plugin/tool@0.1.0#execute" (func $post))
  (export "zeroclaw:plugin/plugin-info@0.1.0#plugin-name" (func $plugin_name))
  (export "cabi_post_zeroclaw:plugin/plugin-info@0.1.0#plugin-name" (func $post))
  (export "zeroclaw:plugin/plugin-info@0.1.0#plugin-version" (func $plugin_version))
  (export "cabi_post_zeroclaw:plugin/plugin-info@0.1.0#plugin-version" (func $post))
  (export "cabi_realloc" (func $realloc))
  (export "_initialize" (func $_initialize))
)
"#;

/// HTTP fixture variant that traps (`unreachable`) right after the host call, so
/// the guest fails *after* egress has already been accounted.
fn trap_after_http_wat() -> String {
    HTTP_TOOL_WAT.replace("call $http_request", "call $http_request\n    unreachable")
}

/// Compile a fixture core module and encode it into a component against `wit/v0`.
#[allow(clippy::field_reassign_with_default)]
fn tool_component(wat_src: &str) -> Vec<u8> {
    let mut module = wat::parse_str(wat_src).expect("fixture WAT must parse");
    // `all_features` includes the `@unstable(feature = plugins-wit-v0)` items
    // when resolving the package (bindgen does the same on the host side).
    let mut resolve = Resolve::default();
    resolve.all_features = true;
    let (package, _paths) = resolve
        .push_dir("../../wit/v0")
        .expect("wit/v0 package must parse");
    let world = resolve
        .select_world(&[package], Some("tool-plugin"))
        .expect("tool-plugin world must exist");

    embed_component_metadata(&mut module, &resolve, world, StringEncoding::UTF8)
        .expect("component metadata must embed");

    let mut encoder = ComponentEncoder::default()
        .module(&module)
        .expect("fixture module must decode")
        .validate(true);
    encoder.encode().expect("component must encode")
}

#[test]
fn prepares_metadata_from_wit_tool_component() {
    let runtime = WitToolRuntime::new(WitToolRuntimeConfig::for_testing()).unwrap();
    let prepared = runtime
        .prepare("fallback", &tool_component(COUNTER_TOOL_WAT))
        .unwrap();

    assert_eq!(prepared.name(), "counter");
    assert_eq!(prepared.description(), "fixture description");
    assert_eq!(prepared.schema(), &json!({ "type": "object" }));
}

#[test]
fn malformed_component_bytes_are_rejected_as_compilation_failure() {
    let runtime = WitToolRuntime::new(WitToolRuntimeConfig::for_testing()).unwrap();
    let error = runtime
        .prepare("malformed", b"not a wasm component")
        .unwrap_err();
    assert!(
        matches!(error, WasmError::CompilationFailed(_)),
        "unexpected error: {error:?}"
    );
}

#[test]
fn core_wasm_module_bytes_are_rejected_as_compilation_failure() {
    let runtime = WitToolRuntime::new(WitToolRuntimeConfig::for_testing()).unwrap();
    let core_module = wat::parse_str("(module)").unwrap();
    let error = runtime.prepare("core-module", &core_module).unwrap_err();
    assert!(
        matches!(error, WasmError::CompilationFailed(_)),
        "unexpected error: {error:?}"
    );
}

#[test]
fn unsupported_component_without_tool_exports_is_rejected_at_instantiation() {
    let runtime = WitToolRuntime::new(WitToolRuntimeConfig::for_testing()).unwrap();
    let component_without_tool_exports = wat::parse_str("(component)").unwrap();
    let error = runtime
        .prepare("unsupported", &component_without_tool_exports)
        .unwrap_err();
    assert!(
        matches!(error, WasmError::InstantiationFailed(_)),
        "unexpected error: {error:?}"
    );
}

#[test]
fn parameters_schema_export_must_return_json_object() {
    let runtime = WitToolRuntime::new(WitToolRuntimeConfig::for_testing()).unwrap();
    let invalid_schema_wat = COUNTER_TOOL_WAT
        .replace(
            r#"(data (i32.const 1024) "{\22type\22:\22object\22}")"#,
            r#"(data (i32.const 1024) "[1]")"#,
        )
        .replace(
            "i32.const 52\n    i32.const 17\n    i32.store",
            "i32.const 52\n    i32.const 3\n    i32.store",
        );
    assert_ne!(invalid_schema_wat, COUNTER_TOOL_WAT);

    let error = runtime
        .prepare("invalid-schema", &tool_component(&invalid_schema_wat))
        .unwrap_err();
    assert!(
        matches!(error, WasmError::InvalidSchema(_)),
        "unexpected error: {error:?}"
    );
}

#[test]
fn executes_with_fresh_component_instance_per_call() {
    let runtime = WitToolRuntime::new(WitToolRuntimeConfig::for_testing()).unwrap();
    let prepared = runtime
        .prepare("counter", &tool_component(COUNTER_TOOL_WAT))
        .unwrap();
    let host = WitToolHost::deny_all();

    let first = runtime
        .execute(&prepared, host.clone(), WitToolRequest::new(r#"{"q":1}"#))
        .unwrap();
    let second = runtime
        .execute(&prepared, host, WitToolRequest::new(r#"{"q":2}"#))
        .unwrap();

    assert!(first.success);
    assert!(second.success);
    assert_eq!(first.output, "1");
    assert_eq!(
        second.output, "1",
        "a fresh instance must reset guest state"
    );
    assert!(first.error.is_none());
}

#[test]
fn http_import_delegates_to_recording_host_and_counts_request_body_only() {
    let runtime = WitToolRuntime::new(WitToolRuntimeConfig::for_testing()).unwrap();
    let prepared = runtime
        .prepare("http", &tool_component(HTTP_TOOL_WAT))
        .unwrap();
    let http = Arc::new(RecordingWasmHostHttp::ok(WasmHttpResponse {
        status: 201,
        headers_json: r#"{"content-type":"text/plain"}"#.to_string(),
        body: b"response body is not egress".to_vec(),
    }));
    let host = WitToolHost::deny_all().with_http(http.clone());

    let executed = runtime
        .execute(&prepared, host, WitToolRequest::new("{}"))
        .unwrap();

    let requests = http.requests().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].url, "https://example.test/api");
    assert_eq!(requests[0].body.as_deref(), Some(&b"hello"[..]));
    assert_eq!(executed.usage.network_egress_bytes, 5);
}

#[test]
fn http_import_counts_request_body_when_host_reports_failure_after_send() {
    let runtime = WitToolRuntime::new(WitToolRuntimeConfig::for_testing()).unwrap();
    let prepared = runtime
        .prepare("http", &tool_component(HTTP_TOOL_WAT))
        .unwrap();
    let http = Arc::new(RecordingWasmHostHttp::err(
        zeroclaw_plugins::WasmHostError::FailedAfterRequestSent("limit exceeded".to_string()),
    ));
    let host = WitToolHost::deny_all().with_http(http.clone());

    let executed = runtime
        .execute(&prepared, host, WitToolRequest::new("{}"))
        .unwrap();

    assert_eq!(http.requests().unwrap().len(), 1);
    assert_eq!(executed.usage.network_egress_bytes, 5);
}

#[test]
fn default_http_host_fails_closed_without_recording_egress() {
    let denied = DenyWasmHostHttp
        .request(WasmHttpRequest {
            method: "GET".to_string(),
            url: "https://example.test/".to_string(),
            headers_json: "{}".to_string(),
            body: Some(b"should-not-send".to_vec()),
            timeout_ms: None,
        })
        .unwrap_err();
    assert!(denied.to_string().contains("not configured"));

    let runtime = WitToolRuntime::new(WitToolRuntimeConfig::for_testing()).unwrap();
    let prepared = runtime
        .prepare("http", &tool_component(HTTP_TOOL_WAT))
        .unwrap();
    let executed = runtime
        .execute(
            &prepared,
            WitToolHost::deny_all(),
            WitToolRequest::new("{}"),
        )
        .unwrap();

    assert_eq!(executed.usage.network_egress_bytes, 0);
}

#[test]
fn execution_error_preserves_usage_when_guest_traps_after_host_egress() {
    let runtime = WitToolRuntime::new(WitToolRuntimeConfig::for_testing()).unwrap();
    let prepared = runtime
        .prepare("http", &tool_component(&trap_after_http_wat()))
        .unwrap();
    let http = Arc::new(RecordingWasmHostHttp::ok(WasmHttpResponse {
        status: 201,
        headers_json: "{}".to_string(),
        body: Vec::new(),
    }));
    let host = WitToolHost::deny_all().with_http(http.clone());

    let error = runtime
        .execute(&prepared, host, WitToolRequest::new("{}"))
        .unwrap_err();

    assert_eq!(http.requests().unwrap().len(), 1);
    match error {
        WasmError::ExecutionFailed { usage, .. } => {
            assert_eq!(usage.network_egress_bytes, 5);
        }
        other => panic!("expected execution failure with usage, got {other:?}"),
    }
}

#[test]
fn http_import_caps_guest_timeout_to_remaining_execution_deadline() {
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    struct CapturingHttp {
        timeout_ms: Mutex<Option<u32>>,
    }

    impl WasmHostHttp for CapturingHttp {
        fn request(
            &self,
            request: WasmHttpRequest,
        ) -> Result<WasmHttpResponse, zeroclaw_plugins::WasmHostError> {
            *self.timeout_ms.lock().unwrap() = request.timeout_ms;
            Ok(WasmHttpResponse {
                status: 200,
                headers_json: "{}".to_string(),
                body: Vec::new(),
            })
        }
    }

    let runtime = WitToolRuntime::new(WitToolRuntimeConfig::for_testing()).unwrap();
    let prepared = runtime
        .prepare("http", &tool_component(HTTP_TOOL_WAT))
        .unwrap();
    let http = Arc::new(CapturingHttp::default());
    let host = WitToolHost::deny_all().with_http(http.clone());

    runtime
        .execute(&prepared, host, WitToolRequest::new("{}"))
        .unwrap();

    let timeout_ms = http.timeout_ms.lock().unwrap().expect("timeout is set");
    assert!(
        timeout_ms <= 5_000,
        "host timeout should be capped to the execution deadline, got {timeout_ms}ms"
    );
}

#[test]
fn http_import_uses_wit_default_when_guest_omits_timeout_below_deadline() {
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    struct CapturingHttp {
        timeout_ms: Mutex<Option<u32>>,
    }

    impl WasmHostHttp for CapturingHttp {
        fn request(
            &self,
            request: WasmHttpRequest,
        ) -> Result<WasmHttpResponse, zeroclaw_plugins::WasmHostError> {
            *self.timeout_ms.lock().unwrap() = request.timeout_ms;
            Ok(WasmHttpResponse {
                status: 200,
                headers_json: "{}".to_string(),
                body: Vec::new(),
            })
        }
    }

    let runtime = WitToolRuntime::new(WitToolRuntimeConfig::default()).unwrap();
    let prepared = runtime
        .prepare("http", &tool_component(HTTP_TOOL_WAT))
        .unwrap();
    let http = Arc::new(CapturingHttp::default());
    let host = WitToolHost::deny_all().with_http(http.clone());

    runtime
        .execute(&prepared, host, WitToolRequest::new("{}"))
        .unwrap();

    assert_eq!(*http.timeout_ms.lock().unwrap(), Some(30_000));
}

#[test]
fn execution_fails_when_host_import_returns_after_deadline() {
    use std::time::Duration;

    struct SlowHttp;

    impl WasmHostHttp for SlowHttp {
        fn request(
            &self,
            _request: WasmHttpRequest,
        ) -> Result<WasmHttpResponse, zeroclaw_plugins::WasmHostError> {
            std::thread::sleep(Duration::from_millis(50));
            Ok(WasmHttpResponse {
                status: 200,
                headers_json: "{}".to_string(),
                body: Vec::new(),
            })
        }
    }

    let runtime = WitToolRuntime::new(WitToolRuntimeConfig {
        default_limits: zeroclaw_plugins::WitToolLimits::default()
            .with_memory_bytes(1024 * 1024)
            .with_fuel(100_000)
            .with_timeout(Duration::from_millis(20)),
    })
    .unwrap();
    let prepared = runtime
        .prepare("http", &tool_component(HTTP_TOOL_WAT))
        .unwrap();
    let host = WitToolHost::deny_all().with_http(Arc::new(SlowHttp));

    let error = runtime
        .execute(&prepared, host, WitToolRequest::new("{}"))
        .unwrap_err();

    assert!(
        error.to_string().contains("deadline"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_multiple_linear_memories_that_exceed_aggregate_memory_budget() {
    let runtime = WitToolRuntime::new(WitToolRuntimeConfig {
        default_limits: zeroclaw_plugins::WitToolLimits::default()
            .with_memory_bytes(64 * 1024)
            .with_fuel(100_000)
            .with_timeout(std::time::Duration::from_secs(5)),
    })
    .unwrap();
    let multi_memory = COUNTER_TOOL_WAT.replace(
        "(memory (export \"memory\") 1)",
        "(memory (export \"memory\") 1)\n  (memory 1)",
    );

    let error = runtime
        .prepare("counter", &tool_component(&multi_memory))
        .unwrap_err();
    // Must fail specifically at instantiation via the ResourceLimiter, not from
    // some unrelated error, so the memory-budget guarantee is what's tested.
    assert!(
        matches!(error, WasmError::InstantiationFailed(_)),
        "expected instantiation failure from the memory limiter, got {error:?}"
    );
}

/// A tool that calls the host `secret-exists` import for "API_KEY" and returns
/// "1" if it exists, "0" otherwise. Proves the existence-only secret boundary:
/// the guest receives a bool, never a value.
const SECRET_TOOL_WAT: &str = r#"
(module
  (import "zeroclaw:plugin/host@0.1.0" "secret-exists" (func $secret_exists (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (global $heap (mut i32) (i32.const 4096))
  (data (i32.const 128) "API_KEY")
  (data (i32.const 1024) "{\22type\22:\22object\22}")
  (data (i32.const 1100) "fixture description")
  (data (i32.const 1200) "secret")
  (data (i32.const 1300) "fixture")
  (data (i32.const 1400) "0.1.0")
  (data (i32.const 3072) "1")
  (data (i32.const 3073) "0")
  (func $name (result i32)
    i32.const 16 i32.const 1200 i32.store
    i32.const 20 i32.const 6 i32.store
    i32.const 16)
  (func $description (result i32)
    i32.const 32 i32.const 1100 i32.store
    i32.const 36 i32.const 19 i32.store
    i32.const 32)
  (func $schema (result i32)
    i32.const 48 i32.const 1024 i32.store
    i32.const 52 i32.const 17 i32.store
    i32.const 48)
  (func $plugin_name (result i32)
    i32.const 64 i32.const 1300 i32.store
    i32.const 68 i32.const 7 i32.store
    i32.const 64)
  (func $plugin_version (result i32)
    i32.const 80 i32.const 1400 i32.store
    i32.const 84 i32.const 5 i32.store
    i32.const 80)
  (func $execute (param i32 i32) (result i32) (local $ex i32)
    i32.const 128
    i32.const 7
    call $secret_exists
    local.set $ex
    i32.const 96 i32.const 0 i32.store
    i32.const 100 i32.const 1 i32.store
    i32.const 104
    local.get $ex
    if (result i32)
      i32.const 3072
    else
      i32.const 3073
    end
    i32.store
    i32.const 108 i32.const 1 i32.store
    i32.const 112 i32.const 0 i32.store
    i32.const 96)
  (func $post (param i32))
  (func $realloc (param $old i32) (param $old_align i32) (param $new_size i32) (param $new_align i32) (result i32)
    (local $ret i32)
    global.get $heap
    local.set $ret
    global.get $heap
    local.get $new_size
    i32.add
    global.set $heap
    local.get $ret)
  (func $_initialize)
  (export "zeroclaw:plugin/tool@0.1.0#name" (func $name))
  (export "cabi_post_zeroclaw:plugin/tool@0.1.0#name" (func $post))
  (export "zeroclaw:plugin/tool@0.1.0#description" (func $description))
  (export "cabi_post_zeroclaw:plugin/tool@0.1.0#description" (func $post))
  (export "zeroclaw:plugin/tool@0.1.0#parameters-schema" (func $schema))
  (export "cabi_post_zeroclaw:plugin/tool@0.1.0#parameters-schema" (func $post))
  (export "zeroclaw:plugin/tool@0.1.0#execute" (func $execute))
  (export "cabi_post_zeroclaw:plugin/tool@0.1.0#execute" (func $post))
  (export "zeroclaw:plugin/plugin-info@0.1.0#plugin-name" (func $plugin_name))
  (export "cabi_post_zeroclaw:plugin/plugin-info@0.1.0#plugin-name" (func $post))
  (export "zeroclaw:plugin/plugin-info@0.1.0#plugin-version" (func $plugin_version))
  (export "cabi_post_zeroclaw:plugin/plugin-info@0.1.0#plugin-version" (func $post))
  (export "cabi_realloc" (func $realloc))
  (export "_initialize" (func $_initialize))
)
"#;

/// A tool whose `execute` spins forever, to exercise the CPU sandbox bounds.
const COMPUTE_SPIN_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (global $heap (mut i32) (i32.const 4096))
  (data (i32.const 1024) "{\22type\22:\22object\22}")
  (data (i32.const 1100) "fixture description")
  (data (i32.const 1200) "spin")
  (data (i32.const 1300) "fixture")
  (data (i32.const 1400) "0.1.0")
  (func $name (result i32)
    i32.const 16 i32.const 1200 i32.store
    i32.const 20 i32.const 4 i32.store
    i32.const 16)
  (func $description (result i32)
    i32.const 32 i32.const 1100 i32.store
    i32.const 36 i32.const 19 i32.store
    i32.const 32)
  (func $schema (result i32)
    i32.const 48 i32.const 1024 i32.store
    i32.const 52 i32.const 17 i32.store
    i32.const 48)
  (func $plugin_name (result i32)
    i32.const 64 i32.const 1300 i32.store
    i32.const 68 i32.const 7 i32.store
    i32.const 64)
  (func $plugin_version (result i32)
    i32.const 80 i32.const 1400 i32.store
    i32.const 84 i32.const 5 i32.store
    i32.const 80)
  (func $execute (param i32 i32) (result i32)
    (loop $l br $l)
    unreachable)
  (func $post (param i32))
  (func $realloc (param $old i32) (param $old_align i32) (param $new_size i32) (param $new_align i32) (result i32)
    (local $ret i32)
    global.get $heap
    local.set $ret
    global.get $heap
    local.get $new_size
    i32.add
    global.set $heap
    local.get $ret)
  (func $_initialize)
  (export "zeroclaw:plugin/tool@0.1.0#name" (func $name))
  (export "cabi_post_zeroclaw:plugin/tool@0.1.0#name" (func $post))
  (export "zeroclaw:plugin/tool@0.1.0#description" (func $description))
  (export "cabi_post_zeroclaw:plugin/tool@0.1.0#description" (func $post))
  (export "zeroclaw:plugin/tool@0.1.0#parameters-schema" (func $schema))
  (export "cabi_post_zeroclaw:plugin/tool@0.1.0#parameters-schema" (func $post))
  (export "zeroclaw:plugin/tool@0.1.0#execute" (func $execute))
  (export "cabi_post_zeroclaw:plugin/tool@0.1.0#execute" (func $post))
  (export "zeroclaw:plugin/plugin-info@0.1.0#plugin-name" (func $plugin_name))
  (export "cabi_post_zeroclaw:plugin/plugin-info@0.1.0#plugin-name" (func $post))
  (export "zeroclaw:plugin/plugin-info@0.1.0#plugin-version" (func $plugin_version))
  (export "cabi_post_zeroclaw:plugin/plugin-info@0.1.0#plugin-version" (func $post))
  (export "cabi_realloc" (func $realloc))
  (export "_initialize" (func $_initialize))
)
"#;

#[test]
fn secret_exists_denied_by_default_returns_false_to_guest() {
    let runtime = WitToolRuntime::new(WitToolRuntimeConfig::for_testing()).unwrap();
    let prepared = runtime
        .prepare("secret", &tool_component(SECRET_TOOL_WAT))
        .unwrap();
    let executed = runtime
        .execute(
            &prepared,
            WitToolHost::deny_all(),
            WitToolRequest::new("{}"),
        )
        .unwrap();
    assert!(executed.success);
    assert_eq!(
        executed.output, "0",
        "deny-by-default: secret must read absent"
    );
}

#[test]
fn secret_exists_reaches_granted_host_but_only_as_a_bool() {
    struct GrantSecret;
    impl zeroclaw_plugins::WasmHostSecrets for GrantSecret {
        fn exists(&self, name: &str) -> bool {
            name == "API_KEY"
        }
    }

    let runtime = WitToolRuntime::new(WitToolRuntimeConfig::for_testing()).unwrap();
    let prepared = runtime
        .prepare("secret", &tool_component(SECRET_TOOL_WAT))
        .unwrap();
    let host = WitToolHost::deny_all().with_secrets(Arc::new(GrantSecret));
    let executed = runtime
        .execute(&prepared, host, WitToolRequest::new("{}"))
        .unwrap();
    assert!(executed.success);
    assert_eq!(
        executed.output, "1",
        "granted: secret exists is visible as a bool"
    );
}

#[test]
fn runaway_compute_is_bounded_by_fuel() {
    // Tiny fuel, generous timeout: the spin loop must exhaust fuel and trap.
    let runtime = WitToolRuntime::new(WitToolRuntimeConfig {
        default_limits: zeroclaw_plugins::WitToolLimits::default()
            .with_memory_bytes(1024 * 1024)
            .with_fuel(50_000)
            .with_timeout(std::time::Duration::from_secs(30)),
    })
    .unwrap();
    let prepared = runtime
        .prepare("spin", &tool_component(COMPUTE_SPIN_WAT))
        .unwrap();
    let error = runtime
        .execute(
            &prepared,
            WitToolHost::deny_all(),
            WitToolRequest::new("{}"),
        )
        .unwrap_err();
    // Fuel exhaustion (not the deadline) is what stopped it.
    assert!(
        matches!(error, WasmError::ExecutionFailed { .. }),
        "got {error:?}"
    );
    assert!(
        !error.to_string().contains("deadline"),
        "expected a fuel trap, not a deadline: {error}"
    );
}

#[test]
fn runaway_compute_is_bounded_by_epoch_deadline() {
    // Generous fuel, short timeout: the epoch ticker must trap the spin loop.
    let runtime = WitToolRuntime::new(WitToolRuntimeConfig {
        default_limits: zeroclaw_plugins::WitToolLimits::default()
            .with_memory_bytes(1024 * 1024)
            .with_fuel(50_000_000_000)
            .with_timeout(std::time::Duration::from_millis(10)),
    })
    .unwrap();
    let prepared = runtime
        .prepare("spin", &tool_component(COMPUTE_SPIN_WAT))
        .unwrap();
    let error = runtime
        .execute(
            &prepared,
            WitToolHost::deny_all(),
            WitToolRequest::new("{}"),
        )
        .unwrap_err();
    assert!(
        error.to_string().contains("deadline"),
        "expected an epoch deadline trap: {error}"
    );
}

/// Proof that the *public* agent entry point works: a real component on disk,
/// loaded via `WitTool::from_wasm` and driven through the async `Tool` trait —
/// the same path `all_tools_with_runtime` uses. No host wiring (M3) or wasm
/// toolchain required; the component bytes come from the inline-WAT fixtures.
mod tool_trait_bridge {
    use super::*;
    use std::io::Write;
    use zeroclaw_api::tool::Tool;
    use zeroclaw_plugins::WitTool;

    fn write_component(bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().expect("temp file");
        file.write_all(bytes).expect("write component");
        file.flush().expect("flush component");
        file
    }

    #[tokio::test]
    async fn loads_metadata_and_executes_through_tool_trait() {
        let file = write_component(&tool_component(COUNTER_TOOL_WAT));
        let tool = WitTool::from_wasm(
            file.path().to_path_buf(),
            vec![],
            "fallback-name".to_string(),
            "fallback-desc".to_string(),
        );

        // Metadata is read from the component's WIT exports, not the fallback.
        assert_eq!(tool.name(), "counter");
        assert_eq!(tool.description(), "fixture description");
        assert_eq!(tool.parameters_schema(), json!({ "type": "object" }));

        let result = tool.execute(json!({ "q": 1 })).await.unwrap();
        assert!(result.success);
        assert_eq!(result.output, "1");
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn granted_http_capability_reaches_recording_host_through_tool() {
        let file = write_component(&tool_component(HTTP_TOOL_WAT));
        let http = Arc::new(RecordingWasmHostHttp::ok(WasmHttpResponse {
            status: 200,
            headers_json: "{}".to_string(),
            body: Vec::new(),
        }));
        let tool = WitTool::from_wasm(
            file.path().to_path_buf(),
            vec![],
            "http".to_string(),
            String::new(),
        )
        .with_host(WitToolHost::deny_all().with_http(http.clone()));

        let result = tool.execute(json!({})).await.unwrap();
        assert!(result.success);

        let requests = http.requests().unwrap();
        assert_eq!(
            requests.len(),
            1,
            "the granted host saw the guest's request"
        );
        assert_eq!(requests[0].url, "https://example.test/api");
    }

    #[tokio::test]
    async fn missing_component_file_falls_back_and_fails_at_execute() {
        let tool = WitTool::from_wasm(
            std::path::PathBuf::from("/nonexistent/plugin.wasm"),
            vec![],
            "broken".to_string(),
            "broken plugin".to_string(),
        );
        // Metadata falls back to manifest-supplied values when the file is gone.
        assert_eq!(tool.name(), "broken");
        assert_eq!(tool.description(), "broken plugin");

        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("failed to load"));
    }
}
