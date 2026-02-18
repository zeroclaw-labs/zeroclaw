use embuild::espidf::sysenv::output;

fn main() {
    output();
    slint_build::compile_with_config(
        "ui/main.slint",
        slint_build::CompilerConfiguration::new()
            .embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer)
            .with_style("material".into()),
    )
    .expect("Slint UI compilation failed");

    println!("cargo:rerun-if-changed=ui/");
}
