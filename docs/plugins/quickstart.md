# Plugin Quickstart

Build and run your first ZeroClaw WASM plugin in under 10 minutes.

## Prerequisites

- Rust toolchain with the `wasm32-wasi` target:
  ```bash
  rustup target add wasm32-wasi
  ```
- A running ZeroClaw instance with plugins enabled in `config.toml`:
  ```toml
  [plugins]
  enabled = true
  plugins_dir = "~/.zeroclaw/plugins"
  ```

## 1. Scaffold the project

```bash
cargo new --lib my-plugin
cd my-plugin
```

Edit `Cargo.toml`:

```toml
[package]
name = "my-plugin"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]          # Required for WASM output

[dependencies]
extism-pdk = "1"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
zeroclaw-plugin-sdk = { git = "https://github.com/zeroclaw-labs/zeroclaw", path = "crates/zeroclaw-plugin-sdk" }
```

> **Tip:** If you're developing against a local checkout, use a `path` dependency instead:
> ```toml
> zeroclaw-plugin-sdk = { path = "../zeroclaw/crates/zeroclaw-plugin-sdk" }
> ```

## 2. Write the plugin

Create `src/lib.rs`:

```rust
use extism_pdk::*;
use serde::{Deserialize, Serialize};
use zeroclaw_plugin_sdk::{context, memory, tools};

#[derive(Deserialize)]
struct GreetInput {
    #[serde(default)]
    name: String,
}

#[derive(Serialize)]
struct GreetOutput {
    greeting: String,
    channel: String,
    first_visit: bool,
}

#[plugin_fn]
pub fn tool_greet(input: String) -> FnResult<String> {
    // Parse input (defaults name to "friend")
    let params: GreetInput = serde_json::from_str(&input)
        .unwrap_or(GreetInput { name: "friend".into() });

    // Read session context
    let session = context::session()?;

    // Check if we've greeted this conversation before
    let memory_key = format!("greeted:{}", session.conversation_id);
    let previously_greeted = memory::recall(&memory_key)
        .map(|v| !v.is_empty())
        .unwrap_or(false);

    let greeting = if previously_greeted {
        format!("Welcome back, {}! (on #{})", params.name, session.channel_name)
    } else {
        // Delegate to another tool for a fun fact
        let fact = tools::tool_call(
            "fun_fact",
            serde_json::json!({}),
        ).unwrap_or_else(|_| "You're awesome!".into());

        // Remember this conversation
        let _ = memory::store(&memory_key, &params.name);

        format!("Hello, {}! Here's a fun fact: {} (on #{})",
            params.name, fact, session.channel_name)
    };

    let output = GreetOutput {
        greeting,
        channel: session.channel_name,
        first_visit: !previously_greeted,
    };

    Ok(serde_json::to_string(&output)?)
}
```

## 3. Write the manifest

Create `manifest.toml` alongside your `Cargo.toml`:

```toml
[plugin]
name = "my-plugin"
version = "0.1.0"
description = "A friendly greeter plugin"
author = "Your Name"
wasm_path = "target/wasm32-wasi/release/my_plugin.wasm"

capabilities = ["tool"]
permissions = []

[plugin.host_capabilities.memory]
read = true
write = true

[plugin.host_capabilities.tool_delegation]
allowed_tools = ["fun_fact"]

[plugin.host_capabilities.context]
session = true

[[tools]]
name = "greet"
description = "Greet a user with session-aware, memory-backed messages"
export = "tool_greet"
risk_level = "low"

[tools.parameters]
type = "object"
properties.name = { type = "string", description = "Name to greet" }
```

## 4. Build

```bash
cargo build --target wasm32-wasi --release
```

The output binary is at `target/wasm32-wasi/release/my_plugin.wasm`.

## 5. Audit before installing

Preview what the plugin requests without installing it:

```bash
zeroclaw plugin audit ./manifest.toml
```

Example output:

```
Plugin: my-plugin v0.1.0 by Your Name
  "A friendly greeter plugin"

Network access: none
Filesystem access: none
Host capabilities:
  memory: read + write
  tool delegation: fun_fact
  context: session

Tools (1):
  greet [low] — Greet a user with session-aware, memory-backed messages
```

## 6. Install

Copy the plugin directory into your plugins path:

```bash
mkdir -p ~/.zeroclaw/plugins/my-plugin
cp manifest.toml target/wasm32-wasi/release/my_plugin.wasm ~/.zeroclaw/plugins/my-plugin/
```

Then reload:

```bash
zeroclaw plugin reload
```

## 7. Verify

```bash
zeroclaw plugin list      # Should show my-plugin as loaded
zeroclaw plugin doctor    # Should show all checks passing
```

The `greet` tool is now available to the agent. When a user asks the agent to greet someone, it will use your plugin.

## Optional: generate an integrity sidecar

For production deployments, generate a SHA-256 hash alongside the binary:

```bash
sha256sum ~/.zeroclaw/plugins/my-plugin/my_plugin.wasm \
  | awk '{print $1}' \
  > ~/.zeroclaw/plugins/my-plugin/my_plugin.wasm.sha256
```

ZeroClaw verifies this hash on every load and warns if it's missing.

## Next steps

- [Manifest Reference](manifest-reference.md) -- all fields and capabilities
- [SDK Reference](sdk-reference.md) -- full API for memory, tools, messaging, and context
- [Security Model](security.md) -- understand how your plugin is sandboxed
