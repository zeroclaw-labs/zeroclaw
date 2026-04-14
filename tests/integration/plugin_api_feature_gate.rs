#![cfg(feature = "plugins-wasm")]
//! Integration test: Plugin API endpoints are feature-gated behind `plugins-wasm`.
//!
//! Verifies the acceptance criterion for US-ZCL-18:
//! > Endpoints are feature-gated behind plugins-wasm
//!
//! Checks that `src/gateway/mod.rs` and `src/gateway/api_plugins.rs` use
//! `#[cfg(feature = "plugins-wasm")]` to conditionally compile the plugin
//! API module and routes.

use std::path::Path;

/// Read a source file relative to the project root.
fn read_src(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

#[test]
fn api_plugins_module_is_feature_gated() {
    let gateway_mod = read_src("src/gateway/mod.rs");

    // The `pub mod api_plugins` declaration must be preceded by a cfg gate.
    let has_gated_mod = gateway_mod.lines().collect::<Vec<_>>().windows(2).any(|w| {
        w[0].contains("#[cfg(feature = \"plugins-wasm\")]") && w[1].contains("pub mod api_plugins")
    });

    assert!(
        has_gated_mod,
        "src/gateway/mod.rs must gate `pub mod api_plugins` behind \
         #[cfg(feature = \"plugins-wasm\")]"
    );
}

#[test]
fn plugin_routes_module_is_feature_gated() {
    let api_plugins = read_src("src/gateway/api_plugins.rs");

    // The plugin_routes module inside api_plugins.rs must be behind a cfg gate.
    let has_gated_routes = api_plugins.lines().collect::<Vec<_>>().windows(2).any(|w| {
        w[0].contains("#[cfg(feature = \"plugins-wasm\")]")
            && w[1].contains("pub mod plugin_routes")
    });

    assert!(
        has_gated_routes,
        "src/gateway/api_plugins.rs must gate `pub mod plugin_routes` behind \
         #[cfg(feature = \"plugins-wasm\")]"
    );
}

#[test]
fn plugin_route_registration_is_feature_gated() {
    let gateway_mod = read_src("src/gateway/mod.rs");

    // The route registration block for /api/plugins must be behind a cfg gate.
    // We look for the cfg attribute near the route definition.
    let lines: Vec<&str> = gateway_mod.lines().collect();
    let has_gated_routes = lines.windows(5).any(|w| {
        w.iter()
            .any(|l| l.contains("#[cfg(feature = \"plugins-wasm\")]"))
            && w.iter().any(|l| l.contains("\"/api/plugins\""))
    });

    assert!(
        has_gated_routes,
        "src/gateway/mod.rs must gate the /api/plugins route registration behind \
         #[cfg(feature = \"plugins-wasm\")]"
    );
}

#[test]
fn plugins_wasm_feature_exists_in_cargo_toml() {
    let cargo_toml = read_src("Cargo.toml");

    assert!(
        cargo_toml.contains("plugins-wasm"),
        "Cargo.toml must define the 'plugins-wasm' feature"
    );
}

#[test]
fn plugins_wasm_feature_included_in_ci_all() {
    let cargo_toml = read_src("Cargo.toml");

    // Find the ci-all feature block and verify it includes plugins-wasm.
    let in_ci_all = cargo_toml
        .lines()
        .skip_while(|l| !l.starts_with("ci-all"))
        .take_while(|l| !l.starts_with(']') || l.contains("ci-all"))
        .any(|l| l.contains("plugins-wasm"));

    assert!(
        in_ci_all,
        "The 'ci-all' feature in Cargo.toml must include 'plugins-wasm'"
    );
}
