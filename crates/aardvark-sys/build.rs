//! Build script for aardvark-sys.
//!
//! # SDK present (real hardware)
//! When the Total Phase SDK files are in `vendor/`:
//!   - Sets linker search path for aardvark.so
//!   - Generates src/bindings.rs via bindgen
//!
//! # SDK absent (stub)
//! Does nothing.  All AardvarkHandle methods return errors at runtime.

fn main() {
    // Stub: SDK not yet in vendor/
    // Uncomment and fill in when aardvark.h + aardvark.so are available:
    //
    //   println!("cargo:rustc-link-search=native=crates/aardvark-sys/vendor");
    //   println!("cargo:rustc-link-lib=dylib=aardvark");
    //   println!("cargo:rerun-if-changed=vendor/aardvark.h");
    //
    //   let bindings = bindgen::Builder::default()
    //       .header("vendor/aardvark.h")
    //       .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
    //       .generate()
    //       .expect("Unable to generate aardvark bindings");
    //   bindings
    //       .write_to_file("src/bindings.rs")
    //       .expect("Could not write bindings");
}
