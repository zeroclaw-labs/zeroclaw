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
| `permissions` | no | Host services the code may reach: `http_client`, `config_read`, `file_read`, `file_write`, `memory_read`, `memory_write` (`PluginPermission`). Only the first two are enforced today; the rest are accepted but inert. Declaring `config_read` requires `config_schema`, and only tool/channel adapters currently deliver it. |
| `config_schema` | exactly with `config_read` | Draft 2020-12 JSON Schema for this plugin's private config; it is included in the canonical manifest bytes and therefore covered when the manifest is signed. The root must be an object with a `properties` map and `additionalProperties = false`. Every top-level property must have one explicit supported type, directly or through a local JSON Pointer: `string`, `boolean`, `integer`, `number`, `array`, or `object`. A schema without `config_read`, or `config_read` without a schema, is rejected. |
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
the resulting object for tool and channel adapters. If `config_read` was
requested but not effectively granted, the plugin receives an empty object;
therefore a schema with required properties fails closed instead of starting
without its required configuration.
