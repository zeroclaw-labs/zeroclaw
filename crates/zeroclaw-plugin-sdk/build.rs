fn main() {
    // If the wit files change, the bindgen needs to be rerun to regenerate the bindings.
    println!("cargo:rerun-if-changed=../../wit");
}
