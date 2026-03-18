fn main() {
    // In development mode, try to locate the zeroclaw binary from the workspace
    // cargo build output and copy it to the sidecar binaries directory with the
    // correct target-triple suffix that Tauri expects.
    //
    // Skip this in release/production builds (detected via PROFILE env var).
    let profile = std::env::var("PROFILE").unwrap_or_default();
    if profile == "debug" {
        let target_triple = std::env::var("TARGET").unwrap_or_default();
        if !target_triple.is_empty() {
            let ext = if target_triple.contains("windows") {
                ".exe"
            } else {
                ""
            };
            let sidecar_name = format!("zeroclaw-{target_triple}{ext}");
            let binaries_dir = std::path::Path::new("binaries");

            // Look for the zeroclaw binary in the workspace target/debug directory
            let workspace_binary = std::path::Path::new("../../../target/debug")
                .join(format!("zeroclaw{ext}"));

            if workspace_binary.exists() && binaries_dir.exists() {
                let dest = binaries_dir.join(&sidecar_name);
                if !dest.exists()
                    || std::fs::metadata(&workspace_binary)
                        .and_then(|m| m.modified())
                        .ok()
                        > std::fs::metadata(&dest)
                            .and_then(|m| m.modified())
                            .ok()
                {
                    if let Err(e) = std::fs::copy(&workspace_binary, &dest) {
                        println!(
                            "cargo:warning=Failed to copy zeroclaw binary to sidecar dir: {e}"
                        );
                    } else {
                        println!(
                            "cargo:warning=Copied zeroclaw binary to {dest}",
                            dest = dest.display()
                        );
                    }
                }
            } else if !workspace_binary.exists() {
                println!(
                    "cargo:warning=ZeroClaw binary not found at {path}. Run 'cargo build' from the workspace root first.",
                    path = workspace_binary.display()
                );
            }
        }
    }

    tauri_build::build()
}
