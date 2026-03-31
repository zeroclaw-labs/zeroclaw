//! Verify that the C# SDK project declares the Extism .NET PDK
//! as a PackageReference in ZeroClaw.PluginSdk.csproj.

use std::path::Path;

#[test]
fn csharp_csproj_declares_extism_pdk_package_reference() {
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
        content.contains(r#"<PackageReference Include="Extism.Pdk""#),
        "Extism.Pdk PackageReference not found in ZeroClaw.PluginSdk.csproj"
    );
}
