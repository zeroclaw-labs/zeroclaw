//! Verify that the C# SDK directory exists with the expected project
//! and source layout: ZeroClaw.PluginSdk.csproj and src/ directory.

use std::path::Path;

#[test]
fn csharp_sdk_directory_exists_with_csproj_and_src() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("sdks/csharp");
    assert!(
        base.is_dir(),
        "sdks/csharp/ directory does not exist at {}",
        base.display()
    );

    let csproj = base.join("ZeroClaw.PluginSdk.csproj");
    assert!(
        csproj.is_file(),
        "ZeroClaw.PluginSdk.csproj is missing from sdks/csharp/"
    );

    let src_dir = base.join("src");
    assert!(
        src_dir.is_dir(),
        "src/ directory is missing from sdks/csharp/"
    );
}
