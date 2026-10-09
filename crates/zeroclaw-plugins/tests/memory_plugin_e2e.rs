//! End-to-end fixture for the host's memory-component adapter.
//!
//! The source fixture is a workspace member and is built on demand into a
//! separate target directory so the nested Cargo invocation cannot contend
//! with the host test process's build lock.

#![cfg(feature = "plugins-wasm-cranelift")]

mod support;

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use zeroclaw_api::memory_traits::{Memory, MemoryCategory};
use zeroclaw_plugins::component::PluginLimits;
use zeroclaw_plugins::config::{PluginConfigResolver, resolve_plugin_config};
use zeroclaw_plugins::instance::PluginInstanceScope;
use zeroclaw_plugins::services::PluginHostServices;
use zeroclaw_plugins::wasm_memory::WasmMemory;
use zeroclaw_plugins::{PluginCapability, PluginManifest};

use support::{admit_fixture, state_service};

/// What every call that reaches the plugin returns once the backend's instance
/// is gone, whether a missed deadline or a call that failed inside Wasmtime
/// discarded it.
const UNAVAILABLE: &str =
    "plugin instance is unavailable: a previous call was interrupted before completion";

fn fixture() -> PathBuf {
    static FIXTURE: OnceLock<PathBuf> = OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            let fixture_dir =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/memory-fixture");
            let target_dir =
                PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("memory-plugin-fixture");
            let status = Command::new(env!("CARGO"))
                .current_dir(&fixture_dir)
                .args([
                    "build",
                    "--locked",
                    "--quiet",
                    "--package",
                    "zeroclaw-memory-plugin-fixture",
                    "--target",
                    "wasm32-wasip2",
                    "--target-dir",
                ])
                .arg(&target_dir)
                .status()
                .expect("run Cargo for the memory component fixture");
            assert!(
                status.success(),
                "memory fixture must build; install the wasm32-wasip2 target"
            );

            let wasm = target_dir.join("wasm32-wasip2/debug/zeroclaw_memory_plugin_fixture.wasm");
            assert!(wasm.is_file(), "memory fixture WASM was not produced");
            wasm
        })
        .clone()
}

fn limits() -> PluginLimits {
    limits_with(1_000_000_000, Duration::from_secs(30))
}

fn limits_with(call_fuel: u64, call_timeout: Duration) -> PluginLimits {
    PluginLimits {
        call_fuel,
        max_memory_bytes: 64 * 1024 * 1024,
        max_table_elements: 10_000,
        max_instances: 32,
        call_timeout,
    }
}

fn manifest() -> PluginManifest {
    PluginManifest {
        name: "memory-fixture".to_string(),
        version: "0.0.0".to_string(),
        description: None,
        author: None,
        wasm_path: Some("memory-fixture.wasm".to_string()),
        wasm_sha256: None,
        capabilities: vec![PluginCapability::Memory],
        provides: None,
        // The memory world imports nothing a grant would gate.
        permissions: Vec::new(),
        config_schema: None,
        signature: None,
        publisher_key: None,
        egress: Default::default(),
    }
}

async fn memory(binding: &str, limits: PluginLimits) -> WasmMemory {
    let manifest = manifest();
    let scope = PluginInstanceScope::from_manifest(
        &manifest,
        PluginCapability::Memory,
        binding,
        manifest.permissions.iter().copied(),
    )
    .expect("admit fixture scope");
    let resolver_manifest = manifest.clone();
    let resolver = PluginConfigResolver::new(move |scope| {
        resolve_plugin_config(&resolver_manifest, scope, None)
    });
    let services = PluginHostServices::new(resolver, state_service());

    let component = admit_fixture(&fixture(), &manifest);
    WasmMemory::from_wasm(scope, &component, &services, limits)
        .await
        .expect("instantiate fixture memory")
}

/// A memory plugin keeps its entries in its own instance. An error string it
/// returns is its own answer, so the instance and its entries stay. A trap
/// leaves a store Wasmtime refuses to enter again, so the host discards it,
/// and every later call to the plugin fails as it does after a missed
/// deadline, rather than reaching an instance that no longer holds what was
/// stored.
#[tokio::test]
async fn a_trap_disables_the_backend_but_a_returned_error_keeps_it() {
    let memory = memory("trap", limits()).await;
    memory
        .store("sky", "the sky is blue", MemoryCategory::Core, None)
        .await
        .expect("the fixture stores an entry");

    let returned = memory
        .store("refused", "memory:reject", MemoryCategory::Core, None)
        .await
        .expect_err("the fixture refuses this entry");
    assert_eq!(
        returned.to_string(),
        "fixture refuses this entry",
        "the plugin's own error string reaches the caller unchanged"
    );
    let kept = memory
        .get("sky")
        .await
        .expect("a returned error keeps the instance")
        .expect("the stored entry is still there");
    assert_eq!(kept.content, "the sky is blue");

    let trapped = memory
        .store("crash", "memory:trap", MemoryCategory::Core, None)
        .await
        .expect_err("a trapping store fails");
    assert!(
        trapped
            .to_string()
            .starts_with("memory.store-entry trapped: "),
        "unexpected error: {trapped:#}"
    );

    let later = memory
        .get("sky")
        .await
        .expect_err("the trapped instance is discarded");
    assert_eq!(later.to_string(), UNAVAILABLE);
    let later = memory
        .count()
        .await
        .expect_err("no later call reaches an instance");
    assert_eq!(later.to_string(), UNAVAILABLE);
    assert!(
        !memory.health_check().await,
        "a backend without an instance reads unhealthy"
    );
}

/// Running out of fuel fails the call inside Wasmtime without the guest
/// trapping on its own, and discards the instance the same way. The deadline
/// is far off, so only the fuel budget can stop the spinning store.
#[tokio::test]
async fn running_out_of_fuel_disables_the_backend_like_a_trap() {
    let memory = memory("fuel", limits_with(10_000_000, Duration::from_secs(30))).await;

    let exhausted = memory
        .store("spin", "memory:spin", MemoryCategory::Core, None)
        .await
        .expect_err("a spinning store runs out of fuel");
    assert!(
        exhausted
            .to_string()
            .starts_with("memory.store-entry trapped: "),
        "unexpected error: {exhausted:#}"
    );

    let later = memory
        .count()
        .await
        .expect_err("the instance that ran dry is discarded");
    assert_eq!(later.to_string(), UNAVAILABLE);
}

/// A missed deadline discards the instance too, and later calls see the same
/// error a trap leaves behind, so a caller handles both alike.
#[tokio::test]
async fn a_missed_deadline_leaves_the_same_error_as_a_trap() {
    let memory = memory(
        "deadline",
        // Unlimited fuel: only the wall-clock deadline can stop the spin.
        limits_with(u64::MAX, Duration::from_millis(500)),
    )
    .await;

    let missed = memory
        .store("spin", "memory:spin", MemoryCategory::Core, None)
        .await
        .expect_err("a spinning store misses its deadline");
    assert_eq!(
        missed.to_string(),
        "plugin call exceeded wall-clock deadline of 500 ms"
    );

    let later = memory
        .count()
        .await
        .expect_err("the interrupted instance is discarded");
    assert_eq!(later.to_string(), UNAVAILABLE);
}
