<!-- Canonical plugin manifest field reference. Edit here; reuse via {{#include}}. -->
The manifest is the file named `manifest.toml` in the plugin directory. Its
fields are the serde surface of `PluginManifest` in
`crates/zeroclaw-plugins/src/lib.rs`, which is the source of truth:

| Field | Required | Meaning |
|-------|----------|---------|
| `name` | yes | Unique canonical package slug and the package component of each derived instance config key. It is not itself an operator config key. Use 1–128 lowercase ASCII characters; start and end with `[a-z0-9]`, with only `[a-z0-9._-]` between. Discovery rejects invalid or duplicate names. |
| `version` | yes | Version string, e.g. `0.1.0`. |
| `description` | no | Human-readable description shown by `zeroclaw plugin list`. |
| `author` | no | Author name or organization. |
| `wasm_path` | for WASM capabilities | Component file name, relative to the plugin directory. Required unless the only capability is `skill`. Discovery skips the plugin if the named file does not exist. |
| `capabilities` | yes, non-empty | What the plugin is: any of `tool`, `channel`, `memory`, `observer`, `skill` (`PluginCapability`, serialized snake_case). |
| `permissions` | no | Host services the code may reach: `http_client`, `config_read`, `file_read`, `file_write`, `memory_read`, `memory_write` (`PluginPermission`). Only the first two are enforced today; the rest are accepted but inert. Declaring `config_read` requires `config_schema`, and only tool/channel adapters currently deliver it. |
| `config_schema` | exactly with `config_read` | Draft 2020-12 JSON Schema for this plugin's private config; it is included in the canonical manifest bytes and therefore covered when the manifest is signed. The root must be an object with a `properties` map and `additionalProperties = false`. Every top-level property must have one explicit supported type, directly or through a local JSON Pointer: `string`, `boolean`, `integer`, `number`, `array`, or `object`. A schema without `config_read`, or `config_read` without a schema, is rejected. |
| `signature` | no | Base64url Ed25519 signature over the canonical manifest bytes. Set when signing for distribution. |
| `publisher_key` | no | Hex-encoded Ed25519 public key of the signer. |
| `egress` | no | Optional `[egress]` table carrying a `hosts` list: the destinations this plugin declares it needs, as exact hosts (`api.example.com`) or explicit suffix patterns (`*.example.com`, which matches subdomains but not the apex). It is part of the canonical manifest bytes, so it is covered when the manifest is signed and changing it requires re-signing. Invalid grammar rejects the whole manifest at discovery and at install. Absent, empty, and "declares nothing" are the same state. |

Declare only the permissions the code actually uses. An undeclared permission
is a host surface the component cannot reach; an unnecessary declared one is
attack surface you asked for and audit burden for whoever reviews your plugin.

`[egress]` is a declaration, not a grant. Nothing in a manifest confers network
reach: the allowlist the host enforces is the operator's
`plugins.entries[].egress_hosts` on the instance's own `zpi1_…` row, so an
unsigned component that writes its own `[egress]` table still reaches nothing.
What the declaration buys is the install ceremony: `zeroclaw plugin install`
seeds it into a row it creates and prints what it granted, and it never widens
a row that already exists. See
[Declaring and granting egress](../plugins/index.md#declaring-and-granting-egress).

Operator values remain strings in `plugins.entries` and are encrypted when
persisted, keyed by a versioned `zpi1_…` string derived from the host-owned
package, capability, and binding identity (installation prints and seeds the
default tool binding's full-instance key): strings are stored as-is, booleans
and numbers use JSON
scalar text, and arrays and objects use JSON text. Before any guest code runs,
the host materializes those strings to the package schema's types and validates
the resulting object for tool and channel adapters. If `config_read` was
requested but not effectively granted, the plugin receives an empty object;
therefore a schema with required properties fails closed instead of starting
without its required configuration.
