//! Records the exact target triple this launcher binary was built for.
//!
//! Host identity must be an exact fact, not a runtime heuristic. `install.sh`
//! reaches the same answer with a `detect_libc` probe, and `src/commands/
//! update.rs` gets it wrong today: it has no libc input and mis-resolves musl
//! hosts onto glibc archives (see `crates/zeroclaw-dist/tests/parity.rs`
//! `KNOWN_UPDATE_MISSING_TARGETS`). A build-time constant cannot make that
//! class of mistake, because Cargo already knows the answer precisely —
//! including `musl` vs `gnu` and `armv7` vs `arm`, which
//! `std::env::consts::ARCH` cannot distinguish.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // Cargo always sets TARGET for build scripts; there is no non-Cargo path
    // that reaches this file.
    let target = std::env::var("TARGET")
        .expect("cargo always sets TARGET for build scripts (documented cargo invariant)");
    println!("cargo:rustc-env=ZEROCLAW_BOOTSTRAP_HOST_TARGET={target}");
}
