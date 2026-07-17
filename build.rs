fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=apps/tauri/windows/app.manifest");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR")
            .ok_or_else(|| std::io::Error::other("Cargo did not provide CARGO_MANIFEST_DIR"))?;
        let manifest = std::path::PathBuf::from(manifest_dir)
            .join("apps/tauri/windows/app.manifest")
            .canonicalize()?;

        println!("cargo:rustc-link-arg-bin=zeroclaw=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bin=zeroclaw=/MANIFESTINPUT:{}",
            manifest.display()
        );
        println!("cargo:rustc-link-arg-bin=zeroclaw=/MANIFESTUAC:NO");
    }

    Ok(())
}
