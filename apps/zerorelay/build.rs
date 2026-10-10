fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    zeroclaw_buildinfo::emit();
}
