use std::{fs, path::Path};

#[test]
fn webview_capabilities_do_not_grant_plugin_or_remote_access() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("capabilities");
    let mut capability_count = 0;

    for entry in fs::read_dir(directory).expect("read Tauri capabilities") {
        let path = entry.expect("read capability entry").path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        capability_count += 1;

        let contents = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let capability: serde_json::Value = serde_json::from_str(&contents)
            .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));

        assert!(
            capability.get("remote").is_none(),
            "{} grants remote content access to native IPC",
            path.display()
        );

        let permissions = capability["permissions"]
            .as_array()
            .unwrap_or_else(|| panic!("{} must contain permissions", path.display()));

        for permission in permissions {
            let identifier = permission
                .as_str()
                .or_else(|| {
                    permission
                        .get("identifier")
                        .and_then(serde_json::Value::as_str)
                })
                .unwrap_or_else(|| panic!("invalid permission in {}", path.display()));

            if let Some((namespace, _)) = identifier.split_once(':') {
                assert_eq!(
                    namespace,
                    "core",
                    "{} grants webview plugin permission `{identifier}`",
                    path.display()
                );
            }
        }
    }

    assert!(capability_count > 0, "no Tauri capability documents found");
}
