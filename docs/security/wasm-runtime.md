# Minimal WASM Runtime

## Status

This document describes the first shipped `runtime.kind = "wasm"` path in ZeroClaw.
It is intentionally narrow.

## What Exists

- Runtime factory support for `runtime.kind = "wasm"`
- Config schema for `[runtime.wasm]`
- In-process execution via Wasmtime
- Per-invocation fuel limits
- Per-invocation linear-memory limits
- Module discovery under `tools/wasm`
- Support for modules exporting `run() -> i32` or `_start()`

## What Does Not Exist Yet

- No shell bridge
- No long-running daemon mode
- No generic host syscall surface
- No unrestricted filesystem mounts
- No host-mediated network bridge
- No attempt to compile the whole ZeroClaw app into WASM

That last point matters. The current runtime is for sandboxed guest modules, not for moving the full agent loop into a WASM guest.

## Contract

Example config:

```toml
[runtime]
kind = "wasm"

[runtime.wasm]
tools_dir = "tools/wasm"
memory_limit_mb = 64
fuel_limit = 1000000
allow_workspace_read = false
allow_workspace_write = false
allowed_hosts = []
```

Guest contract:

- Place modules under `tools/wasm/<name>.wasm`
- Export `run() -> i32` for pure compute tools
- Export `_start()` for start-style modules with no host imports

## Design Notes

This follows the same basic shape used by `agent-sandbox`: a native host runtime owns policy, resource limits, and the execution envelope. The guest stays small.

The current implementation stops there on purpose. Shipping a thin, testable contract is better than pretending the full runtime already works inside WASM.
