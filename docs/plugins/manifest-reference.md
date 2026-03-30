# Plugin Manifest Reference

Every plugin requires a `manifest.toml` (or `plugin.toml`) file that declares the plugin's identity, tools, permissions, and capabilities.

## Minimal manifest

```toml
[plugin]
name = "hello-world"
version = "0.1.0"
wasm_path = "hello.wasm"
capabilities = ["tool"]

[[tools]]
name = "hello"
description = "Say hello"
export = "tool_hello"
```

## Full schema

### `[plugin]` section

| Field | Type | Required | Default | Description |
|---|---|---|---|---|
| `name` | string | yes | -- | Unique plugin identifier |
| `version` | string | yes | -- | Semantic version |
| `description` | string | no | `""` | Human-readable summary |
| `author` | string | no | `""` | Author name or organization |
| `wasm_path` | string | yes | -- | Path to `.wasm` binary (relative to manifest) |
| `capabilities` | array | yes | -- | What the plugin provides (see below) |
| `permissions` | array | no | `[]` | What the plugin requests (see below) |
| `wasi` | bool | no | `true` | Run in WASI environment |
| `timeout_ms` | integer | no | `30000` | Max execution time per call (milliseconds) |
| `signature` | string | no | -- | Base64url-encoded Ed25519 signature |
| `publisher_key` | string | no | -- | Hex-encoded Ed25519 public key |

### Capabilities (what the plugin provides)

| Value | Meaning |
|---|---|
| `tool` | Provides one or more tools to the agent |
| `channel` | Provides a channel implementation |
| `memory` | Provides a memory backend |
| `observer` | Provides an observer/metrics backend |

### Permissions (what the plugin requests from the host)

| Value | Meaning |
|---|---|
| `http_client` | Can make outbound HTTP requests |
| `file_read` | Can read from sandboxed filesystem paths |
| `file_write` | Can write to sandboxed filesystem paths |
| `env_read` | Can access environment variables |
| `memory_read` | Can read agent memory |
| `memory_write` | Can write agent memory |

---

## Network and filesystem access

### `[plugin.network]`

```toml
[plugin.network]
allowed_hosts = ["api.example.com", "*.internal.corp"]
```

- Only these hosts can be reached via `http_client`.
- Wildcard `"*"` is rejected in Strict and Paranoid security levels.

### `[plugin.filesystem]`

```toml
[plugin.filesystem]
config = "/etc/myapp"
data = "/var/data/myapp"
```

- Keys are logical names; values are absolute filesystem paths.
- In Strict mode, paths must be within the workspace root.

---

## Host capabilities

Host capabilities declare which agent subsystems the plugin needs access to. The host enforces these at runtime -- undeclared capabilities are silently denied.

### `[plugin.host_capabilities.memory]`

```toml
[plugin.host_capabilities.memory]
read = true
write = true
```

| Field | Type | Default | Description |
|---|---|---|---|
| `read` | bool | `false` | Can call `zeroclaw_memory_recall` |
| `write` | bool | `false` | Can call `zeroclaw_memory_store` and `zeroclaw_memory_forget` |

### `[plugin.host_capabilities.tool_delegation]`

```toml
[plugin.host_capabilities.tool_delegation]
allowed_tools = ["web_search", "fun_fact"]
```

| Field | Type | Default | Description |
|---|---|---|---|
| `allowed_tools` | array | `[]` | Tool names this plugin can delegate to via `zeroclaw_tool_call` |

- Wildcard `"*"` is allowed in Default security level (with a warning), rejected in Strict and Paranoid.
- Tool call depth is capped at **5 levels** to prevent infinite recursion.

### `[plugin.host_capabilities.messaging]`

```toml
[plugin.host_capabilities.messaging]
allowed_channels = ["slack", "telegram"]
rate_limit_per_hour = 30
```

| Field | Type | Default | Description |
|---|---|---|---|
| `allowed_channels` | array | `[]` | Channels the plugin can send messages through |
| `rate_limit_per_hour` | integer | `60` | Max messages per (plugin, channel) pair per hour |

### `[plugin.host_capabilities.context]`

```toml
[plugin.host_capabilities.context]
session = true
user_identity = false
agent_config = true
```

| Field | Type | Default | Description |
|---|---|---|---|
| `session` | bool | `false` | Access to channel name, conversation ID, timestamp |
| `user_identity` | bool | `false` | Access to username, display name, channel user ID |
| `agent_config` | bool | `false` | Access to agent name, personality traits, identity fields |

> **Note:** All context access is denied in Paranoid security level regardless of manifest declarations.

---

## Tools

Tools are declared with `[[tools]]` array-of-tables syntax.

```toml
[[tools]]
name = "get_weather"
description = "Look up current weather for a city"
export = "tool_get_weather"
risk_level = "low"

[tools.parameters]
type = "object"
required = ["city"]

[tools.parameters.properties.city]
type = "string"
description = "City name"

[tools.parameters.properties.units]
type = "string"
enum = ["metric", "imperial"]
description = "Temperature units"
```

| Field | Type | Required | Default | Description |
|---|---|---|---|---|
| `name` | string | yes | -- | Tool name (unique within plugin) |
| `description` | string | yes | -- | Human-readable description shown to the LLM |
| `export` | string | yes | -- | WASM export function name |
| `risk_level` | enum | no | `low` | `low`, `medium`, or `high` |
| `parameters` / `parameters_schema` | JSON Schema | no | -- | Describes tool input parameters |

### Risk levels

| Level | Meaning | Operator implication |
|---|---|---|
| `low` | Read-only or side-effect-free | Auto-approved in most policies |
| `medium` | Has side effects (writes data, sends messages) | May require operator approval |
| `high` | Destructive or security-sensitive | Always requires explicit approval |

The operator can enforce a **risk ceiling** per security level. Tools above the ceiling are rejected at load time.

---

## Plugin configuration

Plugins can declare configuration keys that operators fill in via `config.toml`.

### In the manifest:

```toml
[plugin.config]
api_key = { required = true, sensitive = true }
base_url = { default = "https://api.example.com" }
timeout = { default = "30" }
```

| Property | Meaning |
|---|---|
| `required = true` | Plugin fails to load if the key is missing from operator config |
| `sensitive = true` | Value is masked in logs, API responses, and the web UI |
| `default = "..."` | Used when the operator doesn't provide a value |

### In operator config.toml:

```toml
[plugins.my-plugin]
api_key = "sk-secret-key"
base_url = "https://custom.endpoint.com"
```

Configuration values are passed to the WASM runtime as Extism config and are accessible inside the plugin via `extism_pdk::config::get("api_key")`.

---

## Signature verification

Plugins can optionally be signed with Ed25519 keys.

```toml
[plugin]
signature = "base64url-encoded-signature"
publisher_key = "hex-encoded-ed25519-public-key"
```

Verification behavior depends on the operator's `signature_mode`:

| Mode | Unsigned plugin | Untrusted key | Trusted key |
|---|---|---|---|
| `disabled` | Loads | Loads | Loads |
| `permissive` | Loads (warning) | Loads (warning) | Loads |
| `strict` | **Rejected** | **Rejected** | Loads |

Trusted keys are configured in `[plugins.security].trusted_publisher_keys`.
