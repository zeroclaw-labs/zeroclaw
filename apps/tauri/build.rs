fn main() {
    #[cfg(target_os = "windows")]
    {
        let attrs = tauri_build::Attributes::new().windows_attributes(
            tauri_build::WindowsAttributes::new()
                .app_manifest(include_str!("windows/app.manifest")),
        );
        tauri_build::try_build(attrs).expect("failed to run tauri_build");
        return;
    }

    tauri_build::build();
}
