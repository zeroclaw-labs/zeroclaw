fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var_os("CARGO_FEATURE_TOOL_GEMINI_CLI").is_some() {
        println!(
            "cargo:warning=the `tool-gemini-cli` feature is deprecated and does nothing: the \
             `gemini_cli` tool was removed because Google retired Gemini CLI for consumer \
             accounts. Drop the feature from your build; it will be removed in a future release."
        );
    }
    zeroclaw_buildinfo::emit();
}
