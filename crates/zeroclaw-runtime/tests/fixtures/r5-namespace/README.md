# Namespace test component

`namespace.component` is a synthetic first-party WebAssembly component built
from the adjacent `guest.rs`. It declares the tool name `execute_pipeline`,
which belongs to a native tool, and its `execute` returns
`synthetic guest executed`. It uses no host I/O, credentials or egress grants.
The namespace tests in `crates/zeroclaw-runtime/src/tools/r5_namespace_tests.rs`
install it as an ordinary plugin package to check that a guest cannot take a
native tool's name. Those tests enable and discover only that package; plugin
defaults and authority are unchanged.

The checked-in file is 52,176 bytes, SHA-256
`ee072a30a4624317cfeed4b759f4afae70088194209c37c9d80dddce937e4d65`, built for
`wasm32-wasip2` with Rust 1.98.0.

## Rebuild

`guest.rs` resolves the WIT contract relative to this directory, so build it
here with a throwaway manifest. From the repository root, this reproduces the
checked-in bytes exactly:

```sh
rustup target add wasm32-wasip2 --toolchain 1.98.0
cd crates/zeroclaw-runtime/tests/fixtures/r5-namespace
cat > Cargo.toml <<'TOML'
[package]
name = "namespace-probe"
version = "0.1.0"
edition = "2024"
publish = false

[lib]
path = "guest.rs"
crate-type = ["cdylib"]

[dependencies]
wit-bindgen = "=0.51.0"

[profile.release]
codegen-units = 1
lto = true
opt-level = "z"
strip = "symbols"

[workspace]
TOML
cargo +1.98.0 build --release --target wasm32-wasip2 --target-dir target
cp target/wasm32-wasip2/release/namespace_probe.wasm namespace.component
rm -r Cargo.toml Cargo.lock target
```
