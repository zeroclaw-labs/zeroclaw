<!-- Canonical plugin manifest field reference. Edit here; reuse via {{#include}}. -->
The manifest is the file named `manifest.toml` in the plugin directory. Its
fields are the serde surface of `PluginManifest` in
`crates/zeroclaw-plugins/src/lib.rs`, which is the source of truth:

| Field | Required | Meaning |
|-------|----------|---------|
| `name` | yes | Unique canonical package slug and operator config key. Use 1–128 lowercase ASCII characters; start and end with `[a-z0-9]`, with only `[a-z0-9._-]` between. Discovery rejects invalid or duplicate names. |
| `version` | yes | Version string, e.g. `0.1.0`. |
| `description` | no | Human-readable description shown by `zeroclaw plugin list`. |
| `author` | no | Author name or organization. |
| `wasm_path` | for WASM capabilities | Component file name, relative to the plugin directory. Required unless the only capability is `skill`. Discovery skips the plugin if the named file does not exist. |
| `capabilities` | yes, non-empty | What the plugin is: any of `tool`, `channel`, `memory`, `observer`, `skill` (`PluginCapability`, serialized snake_case). |
| `permissions` | no | Host services the code may reach: `http_client`, `config_read`, `file_read`, `file_write`, `memory_read`, `memory_write` (`PluginPermission`). Only the first two are enforced today; the rest are accepted but inert. |
| `signature` | no | Base64url Ed25519 signature over the canonical manifest bytes. Set when signing for distribution. |
| `publisher_key` | no | Hex-encoded Ed25519 public key of the signer. |

Declare only the permissions the code actually uses. An undeclared permission
is a host surface the component cannot reach; an unnecessary declared one is
attack surface you asked for and audit burden for whoever reviews your plugin.
