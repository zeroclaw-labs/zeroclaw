# Plugin Tests

Plugins require the `plugins-wasm` feature flag. Without it, the build fails.

## Quick Reference

```bash
# Build + run all plugin tests (recommended)
./dev/test.sh plugins

# Full validation (fmt, clippy, build, all tests including plugins)
./dev/test.sh

# Verbose output (see full cargo output)
./dev/test.sh plugins -v
```

## Running Plugin Tests Directly

```bash
# All plugin integration tests
cargo test --test integration plugin_ --features plugins-wasm

# A specific plugin test file
cargo test --test integration plugin_echo_roundtrip --features plugins-wasm

# A specific test function
cargo test --test integration plugin_api_get_plugin::api_plugin_detail_has_name --features plugins-wasm
```

## Test Harness (`dev/test.sh`)

| Command | What it runs |
|---|---|
| `./dev/test.sh plugins` | Build + plugin integration tests |
| `./dev/test.sh` | Full: fmt, clippy, build, all test suites |
| `./dev/test.sh quick` | Fmt + clippy + unit tests |
| `./dev/test.sh build` | Build only |
| `./dev/test.sh test` | All test suites |
| `./dev/test.sh clippy` | Clippy only |
| `./dev/test.sh fmt` | Format check only |

Flags: `-v` / `--verbose` for full output, `--release` for release profile.

## Directory Layout

- `echo-plugin/` — Minimal plugin for round-trip tests
- `multi-tool-plugin/` — 6-tool plugin for registry, config, HTTP, and filesystem tests
- `http-plugin/` — HTTP access tests
- `fs-plugin/` — Filesystem sandbox tests
- `bad-actor-plugin/` — Security boundary tests (blocked hosts, path traversal, timeouts)
- `artifacts/` — Pre-compiled `.wasm` binaries used by integration tests
- `plugins/` — Empty dir created by `PluginHost::new()` during test runs (do not remove)

## Common Pitfalls

- **Missing `--features plugins-wasm`**: Build will fail. Always include it, or use `./dev/test.sh plugins`.
- **`PluginHost::new(path)`** appends `/plugins/` to the given path. Tests that use the checked-in plugins pass `tests/` (not `tests/plugins/`) as the workspace root.
- **WASM artifacts must be pre-compiled**. They live in `artifacts/` and are checked into git. If a plugin's source changes, rebuild with `cargo build --manifest-path tests/plugins/Cargo.toml --target wasm32-wasip1 --release` and copy the `.wasm` files into `artifacts/`.
