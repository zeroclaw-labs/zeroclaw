//! Round-trip: proves the SDK's `memory-plugin` guest bindings, plus the
//! `MemoryPlugin` trait's capability-gated default stubs, work end-to-end
//! through the real, unmodified host. `examples/memory-noop` implements
//! only the required methods and relies entirely on the trait defaults for
//! every capability-gated one, so this also confirms those defaults match
//! what the host expects when a capability flag is unset.

mod common;

use std::path::Path;

#[tokio::test]
async fn memory_noop_round_trips_through_plugin_host() {
    if !common::wasm32_wasip2_installed() {
        eprintln!("skipping: wasm32-wasip2 target not installed");
        return;
    }

    let example_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/memory-noop");
    let wasm_path = common::build_example(&example_dir, "memory_noop");

    let workdir = tempfile::tempdir().expect("tempdir");
    let plugin_dir = workdir.path().join("plugins/mem");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::copy(&wasm_path, plugin_dir.join("mem.wasm")).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        r#"
name = "mem"
version = "0.1.0"
description = "spike: memory-noop round trip"
wasm_path = "mem.wasm"
capabilities = ["memory"]
"#,
    )
    .unwrap();

    let host = zeroclaw_plugins::host::PluginHost::new(workdir.path()).expect("PluginHost::new");
    let memory = host
        .instantiate_memory_plugin("mem")
        .await
        .expect("instantiate_memory_plugin returned None");

    assert_eq!(zeroclaw_api::memory_traits::Memory::name(&*memory), "mem");

    zeroclaw_api::memory_traits::Memory::store(
        &*memory,
        "k1",
        "hello from host",
        zeroclaw_api::memory_traits::MemoryCategory::Core,
        None,
    )
    .await
    .expect("store");

    let entry = zeroclaw_api::memory_traits::Memory::get(&*memory, "k1")
        .await
        .expect("get")
        .expect("entry present");
    assert_eq!(entry.content, "hello from host");

    let count = zeroclaw_api::memory_traits::Memory::count(&*memory)
        .await
        .expect("count");
    assert_eq!(count, 1);

    // memory-noop declares no optional capabilities, so this exercises the
    // host's documented fallback for an unset `reindex` flag: the host
    // composes the trait default (`Ok(0)`) without calling into the guest.
    let reindexed = zeroclaw_api::memory_traits::Memory::reindex(&*memory)
        .await
        .expect("reindex");
    assert_eq!(reindexed, 0);
}
