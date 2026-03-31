//! Verify that the C# SDK project targets net8.0 and is configured
//! for the wasi-experimental workload (wasi-wasm runtime identifier).

use std::path::Path;

#[test]
fn csharp_csproj_targets_net8() {
    let csproj = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("sdks/csharp/ZeroClaw.PluginSdk.csproj");
    assert!(
        csproj.is_file(),
        "ZeroClaw.PluginSdk.csproj not found at {}",
        csproj.display()
    );

    let content = std::fs::read_to_string(&csproj)
        .expect("failed to read ZeroClaw.PluginSdk.csproj");

    assert!(
        content.contains("<TargetFramework>net8.0</TargetFramework>"),
        "Expected TargetFramework net8.0 in ZeroClaw.PluginSdk.csproj"
    );
}

#[test]
fn csharp_project_produces_wasi_wasm_output() {
    let wasi_output = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("sdks/csharp/bin/Debug/net8.0/wasi-wasm/ZeroClaw.PluginSdk.dll");
    assert!(
        wasi_output.is_file(),
        "wasi-wasm build output not found at {}; run `dotnet build -r wasi-wasm` in sdks/csharp/",
        wasi_output.display()
    );
}
