# Migrating to typed plugin config

Typed instance config is a breaking change for every pre-1.0 plugin that reads
operator configuration. This page is the migration artifact for plugin authors
and operators: what breaks, why, and the exact steps to fix a package.

The behavior described here is checked against
`crates/zeroclaw-plugins/src/config.rs`,
`crates/zeroclaw-plugins/src/instance.rs`, and the admission path in
`crates/zeroclaw-plugins/src/host.rs`.

## Release decision

The enforcement ships with the feature. There is no compatibility shim, no
grace period, and no opt-out flag. Plugins are a pre-1.0 experimental surface,
so the project accepts the break rather than carrying a permanently weaker
config path: an untyped fallback would have to hand a guest values the host
cannot type, name, or bound, which is the exact hole this feature closes.

Packages that do not migrate stop being discovered. Nothing is silently
downgraded, and no partial config reaches guest code.

## What breaks

Three things, independently:

1. **A manifest that requests `config_read` without `config_schema` is no
   longer discovered or installed.** The two are a biconditional: a schema
   without the permission is equally invalid.
2. **Config entries keyed by package or binding name are no longer consulted.**
   Operator values now live under a full-instance key derived from the package,
   capability, and binding.
3. **Guests receive typed JSON, not a string map.** A guest that parsed strings
   itself now gets real booleans, numbers, arrays, and objects.

## Why the host needs a schema

Operator values are stored as a secret-marked string map, encrypted at rest,
and the guest is untrusted third-party code. Without a declared contract the
host cannot answer two questions it must answer before the guest starts: which
keys is this package allowed to receive, and what type is each value. The WIT
worlds are fixed and shared across all plugins, so per-package config types
cannot live in the ABI. The manifest is the only place the contract can be
declared, and `additionalProperties = false` plus an explicit `properties` map
is what makes the `config_read` grant mean something enumerable.

## Author steps

### 1. Declare the schema

Add a closed Draft 2020-12 object covering exactly the keys your plugin reads.
Every top-level property must resolve to one explicit type: `string`,
`boolean`, `integer`, `number`, `array`, or `object`.

```toml
name = "my-plugin"
version = "0.2.0"
wasm_path = "my_plugin.wasm"
capabilities = ["channel"]
permissions = ["config_read"]

[config_schema]
"$schema" = "https://json-schema.org/draft/2020-12/schema"
type = "object"
required = ["bot_token"]
additionalProperties = false

[config_schema.properties.bot_token]
type = "string"
minLength = 1

[config_schema.properties.poll_interval_secs]
type = "integer"
minimum = 1

[config_schema.properties.allowed_chats]
type = "array"
```

The host enforces these limits on the schema itself: 64 KiB serialized, at most
32 levels of nesting, no `$id`, and `$ref` targets must be local JSON Pointers.
Remote references are rejected, so a schema never causes a network fetch.

### 2. Match the value encodings

Operator storage stays a string map. The schema tells the host how to read each
stored string:

| Declared type | What the operator stores | What the guest receives |
| --- | --- | --- |
| `string` | `secret-value` | `"secret-value"` |
| `boolean` | `true` | `true` |
| `integer` | `4` | `4` |
| `number` | `0.5` | `0.5` |
| `array` | `["a","b"]` | `["a","b"]` |
| `object` | `{"k":"v"}` | `{"k":"v"}` |

Anything that fails to parse as the declared type is rejected before your code
runs.

### 3. Decide required versus optional per key

Effective grants are checked separately from manifest requests. When
`config_read` is requested but not granted, the host validates an empty object
against your schema:

- An all-optional schema receives `{}`, so give every field a guest-side
  default.
- A `required` field fails closed, which is what you want for credentials. A
  channel that cannot authenticate should refuse to start rather than run
  half-configured.

### 4. Deserialize typed JSON in the guest

Replace string parsing with one deserialization of the injected object. Tool
plugins read the reserved `__config` key, which the host merges into the call
arguments after deleting any model-supplied value of that name.

### 5. Rebuild and re-sign

`config_schema` is covered by the manifest signature, so a signed package must
be re-signed after adding it. See [Distributing plugins](./distributing-plugins.md)
for the signing flow.

## Operator steps

Existing `[[plugins.entries]]` blocks named after a package or binding are not
read. To move values onto the new key:

1. Run `zeroclaw plugin info <package>` to print the full-instance key, which
   looks like `zpi1_...`.
2. Rename the existing entry's `name` to that key, or reinstall the plugin to
   seed the entry, then set values with
   `zeroclaw config set plugins.entries.<instance-key>.config.<key>`.
3. Save the config. Values stay encrypted at rest.

The key is a versioned, reversible encoding of the package, capability, and
binding, which is why two packages can both use a binding named `main` without
sharing credentials. Fresh installs seed and print the key automatically.

## Diagnosing a rejection

| Message | Cause |
| --- | --- |
| requests `config_read` but declares no `config_schema` | step 1 not done |
| declares `config_schema` without requesting `config_read` | remove the schema or add the permission |
| `config_schema` must set `additionalProperties = false` | the root object is open |
| property uses unsupported type | a property has no explicit supported type, or an unresolvable local `$ref` |
| config contains a property absent from `config_schema` | an operator key is not declared, often a typo |
| config property must be a JSON integer | the stored string does not parse as the declared type |
| config violates `config_schema` at `<path>` | a constraint such as `minimum` or `required` failed |

## First-party packages

Every package published in `zeroclaw-labs/zeroclaw-plugins` requests
`config_read`, and none declared `config_schema` when this landed, so all of
them need step 1 and step 5. Migration is tracked in that repository rather
than here, since the packages version independently of the host.

## Memory plugins

Memory plugins have no config export yet and must not request `config_read`
until that ABI exists.
