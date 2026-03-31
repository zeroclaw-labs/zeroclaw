# Python Plugin SDK Reference

The `zeroclaw-plugin-sdk` package provides Python bindings for building ZeroClaw WASM plugins. It wraps the Extism PDK host functions in a Pythonic API so you can write plugins without touching raw shared-memory serialization.

**Package location:** `sdks/python/`

---

## Installation and project setup

### Prerequisites

- Python 3.10+
- [`extism-py`](https://github.com/extism/python-pdk) compiler (converts Python to WASM)
- [`binaryen`](https://github.com/WebAssembly/binaryen) (`wasm-merge`, `wasm-opt`)

Install `extism-py`:

```bash
curl -Ls https://raw.githubusercontent.com/extism/python-pdk/main/install.sh | bash
```

### Project layout

A minimal Python plugin has this structure:

```
my-plugin/
    plugin.toml          # Plugin manifest
    my_plugin.py         # Plugin source
```

### Installing the SDK

The SDK lives at `sdks/python/` in the ZeroClaw repository. For local development, install it in editable mode:

```bash
pip install -e sdks/python/
```

Or add it as a dependency:

```
zeroclaw-plugin-sdk >= 0.1.0
```

The only runtime dependency is `extism-pdk >= 1.0.0`.

---

## `@plugin_fn` decorator

The `@plugin_fn` decorator is the entry point for every plugin function. It handles JSON serialization/deserialization over Extism shared memory so your function receives a parsed Python object and returns a Python object.

**Import:**

```python
from zeroclaw_plugin_sdk import plugin_fn
```

**Usage:**

```python
@plugin_fn
def my_tool(input):
    # `input` is a parsed JSON value (dict, list, str, etc.)
    # Return any JSON-serializable value
    return {"result": input["name"].upper()}
```

**What it does under the hood:**

1. Reads raw JSON from Extism input (`pdk.input_string()`)
2. Parses it with `json.loads()` (passes `None` if input is empty)
3. Calls your function with the parsed value
4. Serializes the return value with `json.dumps()`
5. Writes it to Extism output (`pdk.output_string()`)
6. Returns `0` (success) to the runtime

The decorated function's name becomes the WASM export name. This must match the `export` field in your `plugin.toml` manifest. For example, if your manifest says `export = "tool_echo"`, your function must be named `tool_echo`.

---

## SDK modules

The SDK exports four modules matching the four host capability groups. Each module wraps one or more ZeroClaw host functions.

```python
from zeroclaw_plugin_sdk import memory, tools, messaging, context
```

All functions raise `RuntimeError` when the host returns an error response.

---

## `memory` -- persistent storage

Store, recall, and forget key-value pairs in the agent's memory system.

**Manifest requirement:** `[plugin.host_capabilities.memory]`

### `memory.store(key, value)`

```python
def store(key: str, value: str) -> None
```

Persist a key-value pair. Overwrites any existing entry with the same key.

**Manifest requirement:** `memory.write = true`

```python
memory.store("user:preference", "dark-mode")
```

### `memory.recall(query)`

```python
def recall(query: str) -> str
```

Search memory for entries matching the query string. Returns the matching result as a string (empty string if no match).

**Manifest requirement:** `memory.read = true`

```python
previous = memory.recall("user:preference")
if previous:
    print(f"Found: {previous}")
```

### `memory.forget(key)`

```python
def forget(key: str) -> None
```

Delete a memory entry by exact key.

**Manifest requirement:** `memory.write = true`

```python
memory.forget("user:preference")
```

---

## `tools` -- tool delegation

Call other tools registered in the agent from within your plugin.

**Manifest requirement:** `[plugin.host_capabilities.tool_delegation]`

### `tools.tool_call(tool_name, arguments)`

```python
def tool_call(tool_name: str, arguments: dict | None = None) -> str
```

Invoke a named tool with JSON arguments. Returns the tool's output string on success.

**Parameters:**

| Name | Type | Default | Description |
|---|---|---|---|
| `tool_name` | `str` | required | Name of the tool to invoke |
| `arguments` | `dict \| None` | `None` | Arguments for the tool (defaults to `{}` if `None`) |

**Manifest requirement:** tool must be listed in `tool_delegation.allowed_tools`

**Depth limit:** 5 levels of nested tool calls.

```python
result = tools.tool_call("web_search", {"query": "python wasm plugins"})
print(result)
```

---

## `messaging` -- channel messaging

Send messages through the agent's configured channels.

**Manifest requirement:** `[plugin.host_capabilities.messaging]`

### `messaging.send(channel, recipient, message)`

```python
def send(channel: str, recipient: str, message: str) -> None
```

Send a message to a recipient on the specified channel.

**Manifest requirement:** channel must be listed in `messaging.allowed_channels`

**Rate limit:** enforced per `rate_limit_per_hour` (default 60).

```python
messaging.send("slack", "#general", "Build completed successfully!")
```

### `messaging.get_channels()`

```python
def get_channels() -> list[str]
```

List all channel names available in the agent.

```python
channels = messaging.get_channels()
for ch in channels:
    print(f"Available: {ch}")
```

---

## `context` -- session and identity

Access runtime context about the current invocation.

**Manifest requirement:** `[plugin.host_capabilities.context]`

> **Note:** All context access is denied in Paranoid security level regardless of manifest declarations. See [security.md](security.md) for details.

### `context.session()`

```python
def session() -> SessionContext
```

Returns the current session context.

**Manifest requirement:** `context.session = true`

**Return type — `SessionContext` dataclass:**

| Field | Type | Description |
|---|---|---|
| `channel_name` | `str` | Channel name (e.g. `"telegram"`, `"slack"`) |
| `conversation_id` | `str` | Opaque conversation/session ID |
| `timestamp` | `str` | ISO-8601 timestamp of the current request |

```python
session = context.session()
print(f"Channel: {session.channel_name}, ID: {session.conversation_id}")
```

### `context.user_identity()`

```python
def user_identity() -> UserIdentity
```

Returns information about the user who triggered this invocation.

**Manifest requirement:** `context.user_identity = true`

**Return type — `UserIdentity` dataclass:**

| Field | Type | Description |
|---|---|---|
| `username` | `str` | Username (e.g. `"jdoe"`) |
| `display_name` | `str` | Display name (e.g. `"Jane Doe"`) |
| `channel_user_id` | `str` | Channel-specific user identifier |

```python
user = context.user_identity()
print(f"Hello, {user.display_name}!")
```

### `context.agent_config()`

```python
def agent_config() -> AgentConfig
```

Returns the agent's personality and identity configuration.

**Manifest requirement:** `context.agent_config = true`

**Return type — `AgentConfig` dataclass:**

| Field | Type | Description |
|---|---|---|
| `name` | `str` | Agent's display name |
| `personality_traits` | `list[str]` | Personality traits (e.g. `["friendly", "concise"]`) |
| `identity` | `dict[str, str]` | Arbitrary identity key-value pairs |

```python
config = context.agent_config()
tone = "formal" if "formal" in config.personality_traits else "casual"
```

---

## Error handling

All SDK functions raise `RuntimeError` when the host returns an error (e.g. capability not declared, rate limit exceeded). Use standard Python exception handling for graceful degradation:

```python
try:
    user = context.user_identity()
    name = user.display_name
except RuntimeError:
    name = "friend"
```

Errors from the host arrive as JSON with `{"success": false, "error": "..."}`. The SDK parses these and raises `RuntimeError` with the error message.

---

## Build process

Python plugins are compiled to WebAssembly using `extism-py` from the [Extism Python PDK](https://github.com/extism/python-pdk).

### Using the build script

The repository includes `build-python-plugins.sh` which handles the full build pipeline:

```bash
./build-python-plugins.sh
```

This script:

1. Generates a thin entry-point wrapper that re-exports your `@plugin_fn`-decorated functions with the raw `@extism.plugin_fn` decorator (required by `extism-py` for AST detection)
2. Compiles the entry-point plus your plugin source and SDK modules into a `.wasm` binary
3. Copies the artifact to `tests/plugins/artifacts/`

### Manual build

To compile a plugin manually:

```bash
# Set PYTHONPATH so extism-py can find the SDK modules
PYTHONPATH="sdks/python/src" extism-py my_plugin.py -o my_plugin.wasm
```

> **Important:** `extism-py` requires `@extism.plugin_fn` decorators (its own AST detection). The build script handles the translation from `@plugin_fn` (SDK decorator) to `@extism.plugin_fn` automatically. If building manually, you need to write an entry-point wrapper — see `build-python-plugins.sh` for the pattern.

### Build requirements

| Tool | Purpose | Install |
|---|---|---|
| `extism-py` | Python-to-WASM compiler | `curl -Ls https://raw.githubusercontent.com/extism/python-pdk/main/install.sh \| bash` |
| `wasm-merge` | Merges WASM modules (binaryen) | [binaryen releases](https://github.com/WebAssembly/binaryen/releases) |
| `wasm-opt` | Optimizes WASM output (binaryen) | Included with binaryen |

---

## End-to-end walkthrough: building a plugin from scratch

This walkthrough builds a "reminder" plugin that stores notes in memory and retrieves them on demand.

### Step 1: Create the project structure

```
reminder-plugin/
    plugin.toml
    reminder_plugin.py
```

### Step 2: Write the manifest (`plugin.toml`)

Declare the plugin identity, tools, and host capabilities. See [manifest-reference.md](manifest-reference.md) for the full schema.

```toml
[plugin]
name = "reminder-plugin"
version = "0.1.0"
description = "Store and recall reminders using agent memory."
wasm_path = "reminder_plugin.wasm"
capabilities = ["tool"]

[plugin.host_capabilities.memory]
read = true
write = true

[[tools]]
name = "save_reminder"
description = "Save a reminder for later"
export = "save_reminder"
risk_level = "low"

[tools.parameters_schema]
type = "object"
required = ["key", "text"]

[tools.parameters_schema.properties.key]
type = "string"
description = "Short key for the reminder"

[tools.parameters_schema.properties.text]
type = "string"
description = "Reminder text"

[[tools]]
name = "get_reminder"
description = "Retrieve a saved reminder"
export = "get_reminder"
risk_level = "low"

[tools.parameters_schema]
type = "object"
required = ["key"]

[tools.parameters_schema.properties.key]
type = "string"
description = "Key of the reminder to retrieve"
```

### Step 3: Write the plugin (`reminder_plugin.py`)

```python
from zeroclaw_plugin_sdk import plugin_fn
from zeroclaw_plugin_sdk import memory


@plugin_fn
def save_reminder(input):
    """Save a reminder to agent memory."""
    key = f"reminder:{input['key']}"
    memory.store(key, input["text"])
    return {"saved": True, "key": input["key"]}


@plugin_fn
def get_reminder(input):
    """Retrieve a reminder from agent memory."""
    key = f"reminder:{input['key']}"
    try:
        text = memory.recall(key)
    except RuntimeError:
        text = ""

    if text:
        return {"found": True, "key": input["key"], "text": text}
    else:
        return {"found": False, "key": input["key"]}
```

### Step 4: Build the WASM binary

Add your plugin to the `PLUGINS` array in `build-python-plugins.sh`:

```bash
PLUGINS=(
    # ... existing entries ...
    "reminder-plugin|reminder_plugin|save_reminder,get_reminder|reminder_plugin.wasm"
)
```

Then run:

```bash
./build-python-plugins.sh
```

Or build manually with an entry-point wrapper (see [Build process](#build-process) above).

### Step 5: Install and test

Copy the manifest and WASM binary to your plugins directory:

```bash
mkdir -p ~/.zeroclaw/plugins/reminder-plugin/
cp plugin.toml ~/.zeroclaw/plugins/reminder-plugin/
cp reminder_plugin.wasm ~/.zeroclaw/plugins/reminder-plugin/
```

Verify it loads:

```bash
zeroclaw plugin audit ~/.zeroclaw/plugins/reminder-plugin/plugin.toml
```

### Step 6: Security hardening (optional)

Generate a SHA-256 sidecar for integrity verification:

```bash
sha256sum reminder_plugin.wasm | awk '{print $1}' > reminder_plugin.wasm.sha256
```

See [security.md](security.md) for signature verification, network security levels, and production configuration.

---

## See also

- [Manifest Reference](manifest-reference.md) -- full `plugin.toml` schema
- [Security Model](security.md) -- sandbox layers, capability enforcement, rate limiting
- [SDK Reference (Rust)](sdk-reference.md) -- equivalent Rust API
- `tests/plugins/python-echo-plugin/` -- minimal echo plugin example
- `tests/plugins/python-sdk-example-plugin/` -- full example using all four SDK modules
