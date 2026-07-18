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
| `wasm_sha256` | in strict mode for WASM capabilities | Hexadecimal SHA-256 of the exact component bytes. When present it is always enforced; strict signature policy requires it so the signed manifest binds the admitted executable. |
| `capabilities` | yes, non-empty | What the plugin is: any of `tool`, `channel`, `memory`, `observer`, `skill` (`PluginCapability`, serialized snake_case). |
| `permissions` | no | Host services the code may reach: `http_client`, `config_read`, `state_read`, `state_write`, `file_read`, `file_write`, `memory_read`, `memory_write` (`PluginPermission`). HTTP, config, and exact-instance state permissions are enforced for tool/channel adapters; filesystem and memory permissions remain inert. Declaring `config_read` requires `config_schema`. |
| `config_schema` | exactly with `config_read` | Draft 2020-12 JSON Schema for this plugin's private config; it is included in the canonical manifest bytes and therefore covered when the manifest is signed. The root must be an object with a `properties` map and `additionalProperties = false`. Every top-level property must have one explicit supported type, directly or through a local JSON Pointer: `string`, `boolean`, `integer`, `number`, `array`, or `object`. Tool and channel consumers may set `x-secret = true` directly on a top-level string property to remove it from public config and expose it through the scoped `secrets.get` host import. A secret property name is a 1–128 byte portable ASCII name containing only letters, digits, `_`, `-`, or `.`. Tools receive public config under `__config` and may read secrets during `execute`. Channels read the current public object through `config.get` and secrets through `secrets.get` during `configure` and operational calls; both imports are unavailable during instantiation and static metadata discovery. Nested, false, or non-boolean secret markers and secret non-string properties are rejected. A schema without `config_read`, or `config_read` without a schema, is rejected. |
| `signature` | no | Base64url Ed25519 signature over the canonical manifest bytes. Set when signing for distribution. |
| `publisher_key` | no | Hex-encoded Ed25519 public key of the signer. |

Declare only the permissions the code actually uses. An undeclared permission
is a host surface the component cannot reach; an unnecessary declared one is
attack surface you asked for and audit burden for whoever reviews your plugin.

Operator values remain strings in `plugins.entries` and are encrypted when
persisted, keyed by a versioned `zpi1_…` string derived from the host-owned
package, capability, and binding identity (installation prints and seeds the
package-name tool key): strings are stored as-is, booleans and numbers use JSON
scalar text, and arrays and objects use JSON text. Before any guest code runs,
the host materializes those strings to the package schema's types and validates
the complete object for tool and channel adapters. Non-secret tool properties
form `__config`; a channel obtains the non-secret object through `config.get`.
A property marked `x-secret = true` is omitted from both public surfaces and is
available only through `secrets.get("property")` in an authorized service
frame. A channel's public and secret reads within one call share one canonical
revision, and the host drops that materialized view when the call ends. A
compliant channel plugin **must** resolve both at each point of use and must not
retain config or credential values in warm guest state; returning plaintext to
the guest means the host cannot enforce non-retention against malicious code.
If `config_read` was requested but not effectively granted, the host validates
an empty object; therefore a schema with required properties fails closed
instead of starting without required configuration. If the empty object is
valid, a tool omits empty `__config` and channel config/secret imports return
`access-denied`; calls outside an authorized frame, resolution failure, and
host-call budget exhaustion return `unavailable`.
