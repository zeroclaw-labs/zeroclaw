fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=apps/tauri/windows/app.manifest");
    println!("cargo:rerun-if-changed=apps/tauri/windows/zeroclaw.rc");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile_for(
            "apps/tauri/windows/zeroclaw.rc",
            &["zeroclaw"],
            embed_resource::NONE,
        )
        .manifest_required()
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    }

    Ok(())
}
