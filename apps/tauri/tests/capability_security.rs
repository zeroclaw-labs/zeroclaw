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

#[test]
fn macos_privacy_manifest_declares_agent_integration_usage() {
    let info_plist = include_str!("../Info.plist");

    for required_key in [
        "NSAppleEventsUsageDescription",
        "NSRemindersFullAccessUsageDescription",
    ] {
        assert!(
            info_plist.contains(&format!("<key>{required_key}</key>")),
            "Tauri Info.plist must declare {required_key} for agent integrations"
        );
    }
}

#[test]
fn macos_signing_allows_agent_apple_events() {
    let entitlements = include_str!("../Entitlements.plist");
    assert!(
        entitlements.contains("<key>com.apple.security.automation.apple-events</key>")
            && entitlements.contains("<true/>"),
        "Tauri signing must allow Apple Events so the hardened runtime can request Automation access"
    );

    let config: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json"))
        .expect("Tauri config must be valid JSON");
    assert_eq!(
        config["bundle"]["macOS"]["entitlements"], "./Entitlements.plist",
        "Tauri bundling must apply the Apple Events entitlements file"
    );
}
