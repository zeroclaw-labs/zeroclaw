#[test]
fn root_schema_export_feature_graph_is_gated() {
    let manifest: toml::Value = toml::from_str(include_str!("../../Cargo.toml"))
        .expect("root Cargo.toml must parse as TOML");

    let root = manifest
        .as_table()
        .expect("root Cargo.toml must parse into a table");
    let dependencies = root
        .get("dependencies")
        .and_then(toml::Value::as_table)
        .expect("root Cargo.toml must define a dependencies table");
    let schemars = dependencies
        .get("schemars")
        .and_then(toml::Value::as_table)
        .expect("root dependencies.schemars must be a table");

    assert_eq!(
        schemars.get("optional").and_then(toml::Value::as_bool),
        Some(true),
        "root dependencies.schemars must be optional"
    );

    let features = root
        .get("features")
        .and_then(toml::Value::as_table)
        .expect("root Cargo.toml must define a features table");
    let schema_export = features
        .get("schema-export")
        .and_then(toml::Value::as_array)
        .expect("root features.schema-export must be an array");
    assert!(
        schema_export
            .iter()
            .any(|value| value.as_str() == Some("dep:schemars")),
        "root features.schema-export must enable dep:schemars; got {schema_export:?}"
    );
    assert!(
        schema_export
            .iter()
            .any(|value| value.as_str() == Some("zeroclaw-config/schema-export")),
        "root features.schema-export must forward zeroclaw-config/schema-export; \
         got {schema_export:?}"
    );

    let default_features = features
        .get("default")
        .and_then(toml::Value::as_array)
        .expect("root features.default must be an array");
    assert!(
        default_features
            .iter()
            .any(|value| value.as_str() == Some("schema-export")),
        "root features.default must include schema-export; got {default_features:?}"
    );

    let workspace = root
        .get("workspace")
        .and_then(toml::Value::as_table)
        .expect("root Cargo.toml must define a workspace table");
    let workspace_dependencies = workspace
        .get("dependencies")
        .and_then(toml::Value::as_table)
        .expect("workspace must define a dependencies table");
    let zeroclaw_config = workspace_dependencies
        .get("zeroclaw-config")
        .and_then(toml::Value::as_table)
        .expect("workspace.dependencies must define zeroclaw-config as a table");
    assert_eq!(
        zeroclaw_config
            .get("default-features")
            .and_then(toml::Value::as_bool),
        Some(false),
        "workspace.dependencies.zeroclaw-config must set default-features=false"
    );
}
