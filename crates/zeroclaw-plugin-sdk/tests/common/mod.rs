use std::process::Command;

pub fn wasm32_wasip2_installed() -> bool {
    Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .any(|line| line.trim() == "wasm32-wasip2")
        })
        .unwrap_or(false)
}

/// Builds the given example crate under `examples/<name>` for
/// `wasm32-wasip2` and returns the path to the compiled component.
pub fn build_example(example_dir: &std::path::Path, crate_file_stem: &str) -> std::path::PathBuf {
    let status = Command::new("cargo")
        .args(["build", "--target", "wasm32-wasip2"])
        .current_dir(example_dir)
        .status()
        .expect("failed to invoke cargo build for example");
    assert!(
        status.success(),
        "example failed to build for wasm32-wasip2"
    );

    example_dir
        .join(format!("target/wasm32-wasip2/debug/{crate_file_stem}.wasm"))
        .canonicalize()
        .expect("wasm artifact not found after build")
}
