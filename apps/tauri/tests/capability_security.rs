use std::path::Path;

use tauri_utils::acl::build::parse_capabilities;

#[test]
fn webview_capabilities_do_not_grant_plugin_or_remote_access() {
    let pattern = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("capabilities")
        .join("**")
        .join("*");
    let capabilities = parse_capabilities(
        pattern
            .to_str()
            .expect("Tauri capability path must be valid UTF-8"),
    )
    .unwrap_or_else(|error| panic!("parse {}: {error}", pattern.display()));

    assert!(!capabilities.is_empty(), "no Tauri capabilities found");

    for (identifier, capability) in capabilities {
        assert!(
            capability.remote.is_none(),
            "capability `{identifier}` grants remote content access to native IPC"
        );

        for permission in capability.permissions {
            let permission_identifier = permission.identifier().get();
            assert!(
                permission_identifier.starts_with("core:"),
                "capability `{identifier}` grants non-core permission `{permission_identifier}`"
            );
        }
    }
}
