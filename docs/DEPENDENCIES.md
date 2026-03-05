# Dependencies for MCP Hot Reload

## Required Dependencies

Add these to `Cargo.toml`:

### Minimal Set (Recommended)

```toml
[dependencies]
# Signal handling (required)
signal-hook = "0.3"

# Hashing for config change detection (required)
sha2 = "0.10"

# Hex encoding for hash strings (usually already present)
hex = "0.4"
```

### Full Set (With Optional Features)

```toml
[dependencies]
# Core signal handling
signal-hook = "0.3"
signal-hook-tokio = { version = "0.3", features = ["futures-v0_3"] }

# Process management
nix = { version = "0.27", features = ["signal", "process"] }

# Hashing and encoding
sha2 = "0.10"
hex = "0.4"

# Config diffing (optional - can implement manually)
similar = "2.4"

# Async runtime (usually already present)
tokio = { version = "1", features = ["full"] }

# Logging (usually already present)
log = "0.4"
```

## Check Current Dependencies

See what's already in ZeroClaw:

```bash
grep -E "^(\[dependencies\]|signal-hook|sha2|nix|tokio)" Cargo.toml
```

## Version Compatibility

| Dependency | Minimum | Recommended | Notes |
|------------|---------|-------------|-------|
| signal-hook | 0.3.0 | 0.3.17 | Tokio-compatible |
| sha2 | 0.10.0 | 0.10.8 | Standard SHA-256 |
| hex | 0.4.0 | 0.4.3 | For hash encoding |
| nix | 0.27.0 | 0.27.1 | Unix process signals |

## Cargo.toml Examples

### Option 1: Just Add What's Needed

```toml
[package]
name = "zeroclaw"
version = "0.1.0"
edition = "2021"

[dependencies]
# ... existing deps ...

# For MCP hot reload - add these two lines
signal-hook = "0.3"
sha2 = "0.10"
```

### Option 2: Full Cargo.toml Section

```toml
[dependencies]
# Async runtime
tokio = { version = "1.35", features = ["full"] }

# Logging
tracing = "0.1"
tracing-subscriber = "0.3"

# Configuration
toml = "0.8"
serde = { version = "1.0", features = ["derive"] }

# CLI
clap = { version = "4.4", features = ["derive"] }

# HTTP/client
reqwest = { version = "0.11", features = ["json"] }

# NEW: Signal handling for MCP hot reload
signal-hook = "0.3"

# NEW: Hashing for config change detection
sha2 = "0.10"
hex = "0.4"

# Optional: Process signal operations
nix = { version = "0.27", features = ["signal", "process"] }
```

## Verify Installation

After updating Cargo.toml:

```bash
# Update dependencies
cargo update

# Check what will be compiled
cargo tree -p signal-hook -p sha2

# Build to verify
cargo check
```

## Feature Flags

If you want to make hot reload optional:

```toml
[features]
default = ["mcp-hot-reload"]
mcp-hot-reload = ["signal-hook", "sha2"]

[dependencies]
signal-hook = { version = "0.3", optional = true }
sha2 = { version = "0.10", optional = true }
```

Then in code:

```rust
#[cfg(feature = "mcp-hot-reload")]
mod mcp_reload;

#[cfg(feature = "mcp-hot-reload")]
{
    // Initialize signal handler
    mcp_reload::init_signal_handler(...).await?;
}
```

## Size Impact

| Dependency | Binary Size | Notes |
|------------|-------------|-------|
| signal-hook | ~50 KB | Small, efficient |
| sha2 | ~100 KB | Crypto primitives |
| hex | ~20 KB | Minimal |
| **Total** | ~170 KB | Negligible impact |

## Alternative: Zero Additional Dependencies

If you absolutely cannot add dependencies, implement signal handling manually:

```rust
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

static RELOAD_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_sighup(_: libc::c_int) {
    RELOAD_REQUESTED.store(true, Ordering::SeqCst);
}

// In main:
unsafe {
    libc::signal(libc::SIGHUP, handle_sighup as libc::sighandler_t);
}

// In event loop:
if RELOAD_REQUESTED.swap(false, Ordering::SeqCst) {
    reload_mcps().await?;
}
```

⚠️ **Warning**: Unsafe code, not recommended for production. Use `signal-hook` instead.

## Security Considerations

- `signal-hook` is widely audited and production-ready
- `sha2` is the standard cryptographic hash (used in Bitcoin, TLS, etc.)
- No network dependencies added
- No unsafe code required (when using signal-hook)

## License Compatibility

All dependencies are permissively licensed:
- `signal-hook`: Apache-2.0 OR MIT
- `sha2`: MIT OR Apache-2.0
- `hex`: MIT OR Apache-2.0

Compatible with ZeroClaw's license (likely MIT/Apache-2.0).
