# WASM Tools Guide

This guide covers everything you need to build, install, and use WASM-based tools
(skills) in ZeroClaw. WASM tools let you extend the agent with custom capabilities
written in any language that compiles to WebAssembly — without modifying ZeroClaw's
core source code.

---

## Table of Contents

1. [How It Works](#1-how-it-works)
2. [Prerequisites](#2-prerequisites)
3. [Creating a Tool](#3-creating-a-tool)
   - [Scaffold from template](#31-scaffold-from-template)
   - [Protocol: stdin / stdout](#32-protocol-stdin--stdout)
   - [manifest.json](#33-manifestjson)
   - [Template: Rust](#34-template-rust)
   - [Template: TypeScript](#35-template-typescript)
   - [Template: Go](#36-template-go)
   - [Template: Python](#37-template-python)
4. [Building](#4-building)
5. [Testing Locally](#5-testing-locally)
6. [Installing](#6-installing)
   - [From a local path](#61-install-from-a-local-path)
   - [From a git repository](#62-install-from-a-git-repository)
   - [From ZeroMarket registry](#63-install-from-zeromarket-registry)
7. [How ZeroClaw Loads and Uses the Tool](#7-how-zeroclaw-loads-and-uses-the-tool)
8. [Directory Layout Reference](#8-directory-layout-reference)
9. [Configuration (`[wasm]` section)](#9-configuration-wasm-section)
10. [Security Model](#10-security-model)
11. [Troubleshooting](#11-troubleshooting)

---

## 1. How It Works

```
┌─────────────────────────────────────────────────────────────┐
│  Your WASM tool (.wasm binary)                              │
│                                                             │
│  stdin  ← JSON args from LLM                                │
│  stdout → JSON result { success, output, error }            │
└───────────────────────┬─────────────────────────────────────┘
                        │  WASI stdio protocol
┌───────────────────────▼─────────────────────────────────────┐
│  ZeroClaw WASM engine (wasmtime + WASI)                     │
│                                                             │
│  • loads tool.wasm + manifest.json from skills/ directory   │
│  • registers the tool with the agent's tool registry        │
│  • invokes the tool when the LLM selects it                 │
│  • enforces memory, fuel, and output size limits            │
└─────────────────────────────────────────────────────────────┘
```

The key insight: **no custom SDK or ABI boilerplate**. Any language that can read
from stdin and write to stdout works. The only contract is the JSON shape described
in [section 2](#32-protocol-stdin--stdout).

---

## 2. Prerequisites

| Requirement | Purpose |
|---|---|
| ZeroClaw built with `--features wasm-tools` | Enables the WASM runtime |
| `wasmtime` CLI | Local testing (`zeroclaw skill test`) |
| Language-specific toolchain | Building `.wasm` from source |

Install `wasmtime` CLI:

```bash
# macOS / Linux
curl https://wasmtime.dev/install.sh -sSf | bash

# Or via cargo
cargo install wasmtime-cli
```

Enable WASM support at compile time:

```bash
cargo build --release --features wasm-tools
```

---

## 3. Creating a Tool

### 3.1 Scaffold from template

```bash
zeroclaw skill new <name> --template <typescript|rust|go|python>
```

Example:

```bash
zeroclaw skill new weather_lookup --template rust
```

This creates a new directory `./weather_lookup/` with all boilerplate files ready
to build. The `--template` flag defaults to `typescript` if omitted.

Supported templates:

| Template | Runtime | Build tool |
|---|---|---|
| `typescript` | Javy (JS → WASM) | `npm run build` |
| `rust` | native wasm32-wasip1 | `cargo build` |
| `go` | TinyGo | `tinygo build` |
| `python` | componentize-py | `componentize-py` |

---

### 3.2 Protocol: stdin / stdout

Every WASM tool must follow this single contract:

**Input** (written to the tool's stdin by ZeroClaw):

```json
{ "param1": "value1", "param2": 42 }
```

The shape of the input object is whatever you define in `manifest.json` under
`parameters`. ZeroClaw passes the LLM-provided argument object verbatim.

**Output** (read from the tool's stdout by ZeroClaw):

```json
{ "success": true,  "output": "result text shown to LLM", "error": null }
{ "success": false, "output": "",                          "error": "reason" }
```

| Field | Type | Required | Description |
|---|---|---|---|
| `success` | bool | yes | `true` if tool completed normally |
| `output` | string | yes | Result text forwarded to the LLM |
| `error` | string or null | yes | Error message when `success` is `false` |

---

### 3.3 manifest.json

Every tool must ship a `manifest.json` alongside `tool.wasm`. This file tells
ZeroClaw the tool's name, description, and the JSON Schema for its parameters.

```json
{
  "name": "weather_lookup",
  "description": "Fetches the current weather for a given city name.",
  "version": "1",
  "parameters": {
    "type": "object",
    "properties": {
      "city": {
        "type": "string",
        "description": "City name to look up (e.g. Hanoi, Tokyo)"
      },
      "units": {
        "type": "string",
        "enum": ["metric", "imperial"],
        "description": "Temperature unit system"
      }
    },
    "required": ["city"]
  },
  "homepage": "https://github.com/yourname/weather_lookup"
}
```

| Field | Required | Description |
|---|---|---|
| `name` | yes | snake_case tool name exposed to the LLM |
| `description` | yes | Human-readable description (shown to LLM for tool selection) |
| `version` | no | Manifest format version, default `"1"` |
| `parameters` | yes | JSON Schema for the tool's input parameters |
| `homepage` | no | Optional URL shown in `zeroclaw skill list` |

The `name` field is the identifier the LLM uses when it decides to call your tool.
Keep it descriptive and unique.

---

### 3.4 Template: Rust

**Scaffolded files:** `Cargo.toml`, `src/lib.rs`, `.cargo/config.toml`

`src/lib.rs`:

```rust
use std::io::{self, Read, Write};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct Args {
    city: String,
    #[serde(default)]
    units: String,
}

#[derive(Serialize)]
struct ToolResult {
    success: bool,
    output: String,
    error: Option<String>,
}

fn main() {
    let mut buf = String::new();
    io::stdin().read_to_string(&mut buf).unwrap();

    let result = match serde_json::from_str::<Args>(&buf) {
        Ok(args) => run(args),
        Err(e) => ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!("invalid input: {e}")),
        },
    };

    io::stdout()
        .write_all(serde_json::to_string(&result).unwrap().as_bytes())
        .unwrap();
}

fn run(args: Args) -> ToolResult {
    // Your logic here
    ToolResult {
        success: true,
        output: format!("Weather in {}: sunny 28°C", args.city),
        error: None,
    }
}
```

**Build:**

```bash
# Add the target once
rustup target add wasm32-wasip1

# Build
cargo build --target wasm32-wasip1 --release
cp target/wasm32-wasip1/release/weather_lookup.wasm tool.wasm
```

---

### 3.5 Template: TypeScript

**Scaffolded files:** `package.json`, `tsconfig.json`, `src/index.ts`

`src/index.ts`:

```typescript
// Read input from stdin (Javy provides Javy.IO)
const input = JSON.parse(
  new TextDecoder().decode(Javy.IO.readSync())
);

function run(args: Record<string, unknown>): string {
  const city = String(args["city"] ?? "");
  // Your logic here
  return `Weather in ${city}: sunny 28°C`;
}

try {
  const output = run(input);
  Javy.IO.writeSync(
    new TextEncoder().encode(
      JSON.stringify({ success: true, output, error: null })
    )
  );
} catch (err) {
  Javy.IO.writeSync(
    new TextEncoder().encode(
      JSON.stringify({ success: false, output: "", error: String(err) })
    )
  );
}
```

**Build:**

```bash
# Install Javy: https://github.com/bytecodealliance/javy/releases
npm install
npm run build   # → tool.wasm
```

---

### 3.6 Template: Go

**Scaffolded files:** `go.mod`, `main.go`

`main.go`:

```go
package main

import (
    "encoding/json"
    "fmt"
    "io"
    "os"
)

type Args struct {
    City  string `json:"city"`
    Units string `json:"units"`
}

type ToolResult struct {
    Success bool    `json:"success"`
    Output  string  `json:"output"`
    Error   *string `json:"error"`
}

func main() {
    data, _ := io.ReadAll(os.Stdin)
    var args Args
    if err := json.Unmarshal(data, &args); err != nil {
        msg := err.Error()
        out, _ := json.Marshal(ToolResult{Error: &msg})
        os.Stdout.Write(out)
        return
    }
    result := run(args)
    out, _ := json.Marshal(result)
    os.Stdout.Write(out)
}

func run(args Args) ToolResult {
    return ToolResult{
        Success: true,
        Output:  fmt.Sprintf("Weather in %s: sunny 28°C", args.City),
    }
}
```

**Build:**

```bash
# Install TinyGo: https://tinygo.org/getting-started/install/
tinygo build -o tool.wasm -target wasi .
```

---

### 3.7 Template: Python

**Scaffolded files:** `app.py`, `requirements.txt`

`app.py`:

```python
import sys
import json

def run(args: dict) -> str:
    city = str(args.get("city", ""))
    # Your logic here
    return f"Weather in {city}: sunny 28°C"

def main():
    raw = sys.stdin.read()
    try:
        args = json.loads(raw)
        output = run(args)
        result = {"success": True, "output": output, "error": None}
    except Exception as exc:
        result = {"success": False, "output": "", "error": str(exc)}
    sys.stdout.write(json.dumps(result))

if __name__ == "__main__":
    main()
```

**Build:**

```bash
pip install componentize-py
componentize-py -d wit/ -w zeroclaw-skill componentize app -o tool.wasm
```

---

## 4. Building

After editing your tool logic, build it into `tool.wasm`:

| Template | Build command | Output |
|---|---|---|
| Rust | `cargo build --target wasm32-wasip1 --release && cp target/wasm32-wasip1/release/*.wasm tool.wasm` | `tool.wasm` |
| TypeScript | `npm run build` | `tool.wasm` |
| Go | `tinygo build -o tool.wasm -target wasi .` | `tool.wasm` |
| Python | `componentize-py -d wit/ -w zeroclaw-skill componentize app -o tool.wasm` | `tool.wasm` |

The output must always be named `tool.wasm` at the root of the skill directory.

---

## 5. Testing Locally

Before installing, test the tool directly without starting the full ZeroClaw agent:

```bash
zeroclaw skill test . --args '{"city":"Hanoi","units":"metric"}'
```

You can also test an installed skill by name:

```bash
zeroclaw skill test weather_lookup --args '{"city":"Tokyo"}'
```

Or test a specific tool inside a multi-tool skill:

```bash
zeroclaw skill test . --tool my_tool_name --args '{"city":"Paris"}'
```

Under the hood, `skill test` pipes the JSON args into `wasmtime run tool.wasm` via
stdin and prints the raw stdout response. This lets you iterate quickly without
restarting the agent.

You can also test manually using `wasmtime` directly:

```bash
echo '{"city":"Hanoi"}' | wasmtime tool.wasm
```

Expected output:

```json
{"success":true,"output":"Weather in Hanoi: sunny 28°C","error":null}
```

---

## 6. Installing

### 6.1 Install from a local path

```bash
zeroclaw skill install ./weather_lookup
```

This copies your skill directory into `<workspace>/skills/weather_lookup/`.
ZeroClaw will auto-discover it on next startup.

### 6.2 Install from a git repository

```bash
zeroclaw skill install https://github.com/yourname/weather_lookup.git
```

ZeroClaw clones the repository into the skills directory and scans for WASM tools.

### 6.3 Install from ZeroMarket registry

```bash
# Format: namespace/package-name
zeroclaw skill install acme/weather-lookup

# With a specific version
zeroclaw skill install acme/weather-lookup@0.2.1
```

ZeroClaw fetches the package index from the configured registry URL, then downloads
`tool.wasm` and `manifest.json` for each tool in the package.

**Verify the install:**

```bash
zeroclaw skill list
```

---

## 7. How ZeroClaw Loads and Uses the Tool

### 7.1 Startup discovery

Every time the ZeroClaw agent starts, it scans the `skills/` directory and loads
all valid WASM tools automatically. No config change or restart command is needed
after installation.

```
<workspace>/
└── skills/
    └── weather_lookup/           ← skill package root
        ├── SKILL.toml
        └── tools/
            └── weather_lookup/   ← individual tool directory
                ├── tool.wasm     ← compiled WASM binary
                └── manifest.json ← tool metadata
```

A simpler "dev layout" is also supported (useful right after building):

```
<workspace>/
└── skills/
    └── weather_lookup/
        ├── tool.wasm
        └── manifest.json
```

### 7.2 Tool registration

After discovery, each `WasmTool` is registered in the agent's tool registry
alongside built-in tools like `shell`, `file`, `web_fetch`, etc. The LLM sees
all registered tools equally — it has no way to distinguish a built-in tool from
a WASM plugin.

### 7.3 LLM tool selection

When a user sends a message, the agent attaches the full tool registry (including
all WASM tools) to the LLM context. The LLM reads each tool's `name` and
`description` from the manifest and decides which tool to call based on the
user's request.

Example conversation:

```
User:  What is the weather in Hanoi right now?

Agent: [internally, LLM selects tool "weather_lookup" with args {"city":"Hanoi"}]

       ZeroClaw calls weather_lookup WASM tool:
         stdin  → {"city":"Hanoi"}
         stdout ← {"success":true,"output":"Weather in Hanoi: sunny 28°C","error":null}

Agent: The current weather in Hanoi is sunny with a temperature of 28°C.
```

### 7.4 Invocation flow

```
LLM decides to call "weather_lookup"
  │
  ▼
WasmTool::execute(args: JSON)
  │
  ├─ serialize args to stdin bytes
  ├─ spin up wasmtime WASI sandbox
  ├─ write stdin → WASM process
  ├─ read stdout ← WASM process  (capped at 1 MiB)
  ├─ enforce fuel limit          (≈ 1 billion instructions)
  ├─ enforce wall-clock timeout  (30 seconds)
  └─ deserialize ToolResult JSON
  │
  ▼
Agent formats output and responds to user
```

### 7.5 Error handling

If a tool fails (non-zero exit, invalid JSON, timeout, fuel exhaustion), ZeroClaw
logs a warning and returns the error to the LLM. The agent continues running —
a broken plugin never crashes the process.

---

## 8. Directory Layout Reference

**Installed layout** (created by `zeroclaw skill install`):

```
skills/
└── <skill-name>/
    ├── SKILL.toml                 ← package metadata (shown in skill list)
    └── tools/
        └── <tool-name>/
            ├── tool.wasm          ← WASM binary
            └── manifest.json      ← tool metadata
```

**Dev layout** (for quick iteration, right after `cargo build`):

```
skills/
└── <skill-name>/
    ├── tool.wasm
    └── manifest.json
```

Both layouts are discovered automatically. Use dev layout while developing, switch
to installed layout for distribution.

---

## 9. Configuration (`[wasm]` section)

Add this section to your `zeroclaw.toml` to tune WASM tool behavior:

```toml
[wasm]
# Disable all WASM tools (default: true)
enabled = true

# Maximum memory per invocation in MiB, clamped 1–256 (default: 64)
memory_limit_mb = 64

# CPU fuel budget — roughly one unit per WASM instruction (default: 1_000_000_000)
fuel_limit = 1_000_000_000

# Registry URL used by `zeroclaw skill install namespace/package`
registry_url = "https://registry.zeromarket.dev"
```

To disable all WASM tools without uninstalling them:

```toml
[wasm]
enabled = false
```

---

## 10. Security Model

WASM tools run inside a strict WASI sandbox enforced by wasmtime:

| Constraint | Default |
|---|---|
| Filesystem access | **Denied** — no preopened directories |
| Network sockets | **Denied** — WASI network not enabled |
| Max memory | 64 MiB (configurable, max 256 MiB) |
| Max CPU instructions | ~1 billion (configurable) |
| Max wall-clock time | 30 seconds hard limit |
| Max output size | 1 MiB |
| Registry transport | HTTPS only — HTTP is rejected |
| Registry path traversal | Tool names validated before writing to disk |

A malicious or buggy WASM tool cannot:
- Read or write files on the host
- Make network connections
- Access environment variables
- Consume unbounded CPU or memory
- Crash the ZeroClaw process

---

## 11. Troubleshooting

**`WASM tools are not enabled in this build`**

Recompile with the feature flag:

```bash
cargo build --release
```

**`wasmtime` not found during `skill test`**

Install the wasmtime CLI:

```bash
curl https://wasmtime.dev/install.sh -sSf | bash
# or
cargo install wasmtime-cli
```

**`WASM module must export '_start'`**

Your binary must be compiled as a WASI executable (not a library). For Rust, ensure
your `Cargo.toml` does **not** set `crate-type = ["cdylib"]` — use the default
binary crate instead. For Go, use `tinygo build -target wasi` (not `wasm`).

**`WASM tool wrote nothing to stdout`**

Your tool exited without writing a JSON result. Check that your `run()` function
always writes to stdout before returning, including in error paths.

**Tool not appearing in `zeroclaw skill list`**

- Verify `manifest.json` exists alongside `tool.wasm`
- Validate the JSON is well-formed: `cat manifest.json | python3 -m json.tool`
- Restart the agent — tools are discovered at startup

**`curl failed` during registry install**

Ensure `curl` is installed and the registry URL uses HTTPS. Custom registries must
be reachable and return the expected package index JSON format.
