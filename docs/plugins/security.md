# Plugin Security Model

ZeroClaw plugins run in a multi-layered security sandbox. This page documents each layer and how operators configure enforcement.

## Security layers at a glance

```
  1. Manifest audit       (what does the plugin claim to need?)
  2. Signature verification (who published it?)
  3. WASM sandbox          (code can't escape the runtime)
  4. Capability enforcement (host functions check declarations)
  5. Rate limiting          (messaging and tool calls are throttled)
  6. Integrity verification (SHA-256 hash checks on every load)
```

---

## 1. Network security levels

Configured via `[plugins.security].network_security_level` in `config.toml`.

### Default

Standard validation. Wildcard `allowed_hosts` and `allowed_tools` produce warnings but are permitted.

### Strict

- Wildcard (`"*"`) in `allowed_hosts` is **rejected** (`WildcardHostRejected` error).
- Wildcard in `tool_delegation.allowed_tools` is **rejected** (`WildcardDelegationRejected` error).
- Filesystem paths must be within the workspace root (`PathOutsideWorkspace` error).

### Paranoid

Everything in Strict, plus:

- Only plugins listed in `allowed_plugins` are loaded (`PluginNotAllowlisted` error).
- All `context` host functions are **denied** regardless of manifest declarations.

```toml
[plugins.security]
network_security_level = "strict"
allowed_plugins = []                    # Only used in paranoid mode
```

---

## 2. Signature verification

Plugins can be signed with Ed25519 keys. Enforcement is controlled by `signature_mode`:

| Mode | Unsigned | Untrusted key | Trusted key |
|---|---|---|---|
| `disabled` (default) | Loads | Loads | Loads |
| `permissive` | Warning, loads | Warning, loads | Loads |
| `strict` | Rejected | Rejected | Loads |

```toml
[plugins.security]
signature_mode = "strict"
trusted_publisher_keys = [
    "a1b2c3d4e5f6..."  # Hex-encoded Ed25519 public key
]
```

---

## 3. WASM sandbox

Each plugin runs in an isolated WASM instance via the Extism runtime:

- **No shared memory** between plugins or with the host process.
- **No raw syscalls** -- I/O goes through WASI with operator-controlled path mappings.
- **Execution timeout** -- enforced per call (default 30 seconds, configurable via `timeout_ms`).
- **Host function imports** -- only functions matching declared capabilities are linked.

---

## 4. Capability enforcement

Host functions check the plugin's declared capabilities before executing:

| Host function | Required capability |
|---|---|
| `zeroclaw_memory_store` | `memory.write = true` |
| `zeroclaw_memory_recall` | `memory.read = true` |
| `zeroclaw_memory_forget` | `memory.write = true` |
| `zeroclaw_tool_call` | Tool in `tool_delegation.allowed_tools` |
| `zeroclaw_send_message` | Channel in `messaging.allowed_channels` |
| `zeroclaw_get_channels` | Any `messaging` capability |
| `context_session` | `context.session = true` |
| `context_user_identity` | `context.user_identity = true` |
| `context_agent_config` | `context.agent_config = true` |

Calls without the required capability return a JSON error:

```json
{ "success": false, "error": "capability 'memory.write' not declared" }
```

---

## 5. Rate limiting

### Messaging

Each (plugin, channel) pair has a sliding-window rate limit. Default: **60 messages per hour**. Override per plugin in the manifest:

```toml
[plugin.host_capabilities.messaging]
rate_limit_per_hour = 30
```

### Tool calls

Tool operations pass through the security policy's `enforce_tool_operation()` check, which applies the configured rate limits.

### Tool call depth

Nested tool delegation (plugin calls tool, tool calls tool, ...) is capped at **5 levels**. A thread-local depth counter with an RAII guard prevents infinite recursion.

---

## 6. Integrity verification

When a `.wasm.sha256` sidecar file exists alongside a plugin binary, ZeroClaw verifies the SHA-256 hash on every load.

```
~/.zeroclaw/plugins/my-plugin/
    manifest.toml
    plugin.wasm
    plugin.wasm.sha256       <-- contains the expected hex-encoded hash
```

| Scenario | Behavior |
|---|---|
| Hash matches | Plugin loads normally |
| Hash mismatch | `HashMismatch` error, plugin is **not loaded** |
| Sidecar missing | Warning logged, plugin loads (backward compatible) |

Generate a sidecar:

```bash
sha256sum plugin.wasm | awk '{print $1}' > plugin.wasm.sha256
```

---

## 7. Risk level ceiling

Each tool declares a risk level (`low`, `medium`, `high`). Operators can enforce a ceiling -- tools above the ceiling are rejected at load time and not registered with the agent.

---

## 8. Plugin enable/disable

Operators can disable plugins without removing them:

```toml
[plugins]
disabled_plugins = ["untrusted-plugin"]
```

Or via the CLI / API:

```bash
zeroclaw plugin disable untrusted-plugin
zeroclaw plugin enable untrusted-plugin
```

Disabled plugins remain on disk but are not loaded into the runtime.

---

## Auditing a plugin before installation

Always audit unknown plugins before installing:

```bash
zeroclaw plugin audit /path/to/manifest.toml
```

This prints a human-readable summary of everything the plugin declares: network access, filesystem paths, host capabilities, permissions, tool risk levels, and signature status.

---

## Configuration checklist for production

```toml
[plugins]
enabled = true
plugins_dir = "~/.zeroclaw/plugins"
max_plugins = 50

[plugins.security]
signature_mode = "strict"                # Require signed plugins
trusted_publisher_keys = ["..."]         # Your team's Ed25519 public keys
network_security_level = "strict"        # Reject wildcards
```

Additionally:
- Generate `.wasm.sha256` sidecar files for all plugin binaries.
- Run `zeroclaw plugin doctor` periodically to verify plugin health.
- Review `zeroclaw plugin audit` output for any new plugin before deploying.
