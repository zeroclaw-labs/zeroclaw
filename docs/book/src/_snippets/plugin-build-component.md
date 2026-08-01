<!-- Canonical plugin component build steps. Edit here; reuse via {{#include}}. -->
Install the WASI Preview 2 target once, then build the component:

```bash
rustup target add wasm32-wasip2
cargo build --release --target wasm32-wasip2
```

The component lands at `target/wasm32-wasip2/release/<crate_name>.wasm`
(hyphens in the crate name become underscores). Rename it to whatever your
manifest's `wasm_path` declares when you assemble the plugin directory.

{{#include plugin-wasm-binary-warning.md}}

If the target host is a runtime-only build (no JIT backend compiled in), it
cannot compile `.wasm` on load; it deserializes a precompiled `.cwasm` instead.
Precompile with a wasmtime CLI whose version matches the host's and ship the
`.cwasm` as the `wasm_path` artifact. A version-mismatched artifact is rejected
by wasmtime's deserialization check, not silently misloaded.
