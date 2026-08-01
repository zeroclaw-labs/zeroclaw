//! Pins the plugin feature graph: every WASM backend feature must imply
//! `plugins-wasm`, or `cargo build --features plugins-wasm-<backend>` silently
//! produces a binary without the plugin host or `plugin` subcommand.

fn root_feature_values(feature: &str) -> Vec<String> {
    let manifest = include_str!("../../Cargo.toml");
    let prefix = format!("{feature} ");
    let line = manifest
        .lines()
        .find(|line| line.trim_start().starts_with(&prefix))
        .unwrap_or_else(|| panic!("root Cargo.toml must define the `{feature}` feature"));
    let list_start = line.find('[').expect("feature line must carry a list");
    let list_end = line.rfind(']').expect("feature line must close its list");
    line[list_start + 1..list_end]
        .split(',')
        .map(|v| v.trim().trim_matches('"').to_string())
        .filter(|v| !v.is_empty())
        .collect()
}

#[test]
fn wasm_backend_features_imply_plugins_wasm() {
    for backend in [
        "plugins-wasm-runtime-only",
        "plugins-wasm-cranelift",
        "plugins-wasm-pulley",
    ] {
        let values = root_feature_values(backend);
        assert!(
            values.iter().any(|v| v == "plugins-wasm"),
            "`{backend}` must imply `plugins-wasm` so a backend-only build \
             cannot silently drop the plugin host; got: {values:?}"
        );
    }
}
