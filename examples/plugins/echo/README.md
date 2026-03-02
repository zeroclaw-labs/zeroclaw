# Echo Plugin Example

This folder contains a minimal plugin manifest and a WAT template matching the current host ABI.

Files:
- `echo.plugin.toml` - plugin declaration loaded by ZeroClaw
- `echo.wat` - sample WASM text source

## Build

Convert WAT to WASM with `wat2wasm`:

```bash
wat2wasm examples/plugins/echo/echo.wat -o examples/plugins/echo/echo.wasm
```

## Enable in config

```toml
[plugins]
enabled = true
load_paths = ["examples/plugins/echo"]
```

## ABI exports required

- `memory`
- `alloc(i32) -> i32`
- `dealloc(i32, i32)`
- `zeroclaw_tool_execute(i32, i32) -> i64`
- `zeroclaw_provider_chat(i32, i32) -> i64`

The `i64` return packs output pointer/length:
- high 32 bits: pointer
- low 32 bits: length

Input/output payloads are UTF-8 JSON.

Note: this example intentionally keeps logic minimal and is not production-safe.
