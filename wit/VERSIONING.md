#### Directory layout

```
wit/
  VERSIONING.md       ← this file
  v0/                 ← zeroclaw:plugin@0.x (experimental → stable)
    .frozen           ← created when v0 stabilizes; absent = experimental
    channel.wit
    config.wit
    logging.wit
    memory.wit
    plugin-info.wit
    README.md
    secrets.wit
    tool.wit
    types.wit
  v1/                 ← (future) breaking changes → zeroclaw:plugin@1.0.0
```

Each `vN/` directory maps to one WIT package major version. Minor bumps (0.2,
0.3, …) stay inside the same directory using `@since` annotations.

#### Breaking vs non-breaking changes

**Breaking — requires a new `vN+1/` directory:**

- Removing or renaming any type, function, record field, or variant case
- Changing the type of any function parameter or return value
- Changing the type of any record field
- Reordering fields in a record

**Non-breaking — allowed inside an existing `vN/` directory via `@since`:**

- Adding new `flags` bits to `*-capabilities`
- Adding new capability-gated functions to an interface
- Adding new record types, variant types, or enums
- Adding new WIT `interface` definitions to the package
- Adding new `world` definitions

#### `@unstable` / `@since` lifecycle

1. **During development** — annotate with
   `@unstable(feature = your-feature-name)`. The item is invisible to
   `bindgen!` callers that do not opt in with `features: ["your-feature-name"]`.
2. **At release** — remove `@unstable`, add `@since(version = 0.x.0)`.
   `bindgen!` callers without a feature gate now see the item automatically.

All current content in `wit/v0/` is gated behind
`@unstable(feature = plugins-wit-v0)`. It graduates when the first
stable Component Model release ships.

#### Host compatibility window

The host maintains adapters for **the current major version and one previous
(N-1)**:

| When ships   | Supported | Dropped |
| ------------ | --------- | ------- |
| V0 (current) | V0        | —       |
| V1           | V1, V0    | —       |
| V2           | V2, V1    | V0      |

Dropping a version requires a CHANGELOG entry, a deprecation notice in the
prior release, and a clear error message naming the detected WIT version.

#### Stability fence

`wit/vN/.frozen` is created in a dedicated PR when the corresponding version is
declared stable. After it exists:

- The `wit-breaking-change-check` skill will be able to evaluate any PR that
  removes or modifies existing lines in `wit/vN/*.wit`.
- Only additive changes (new types, new functions, `@since` annotations) are
  accepted.
- This fence has some automated features, but still relies on human diligence:
  reviewers must ensure the skill is run and any reported breaking changes are
  addressed before merge.

#### Migration guide for plugin authors

**Current experimental V0:** the tool and channel worlds import the `secrets`
interface, and the channel world also imports `config`. Channel `configure`
changed from `configure(config: string)` to `configure()`. Rebuild both kinds of
component against the current `wit/v0/` definitions before installing them on
this host. Channel authors must update source to call `config.get` for the typed
public object during `configure` and again in every operational export that
uses config; call `secrets.get` at the same point of use and do not retain either
value in warm guest state. Tool authors keep the existing `__config` injection
and `secrets.get`-during-`execute` contract. Publish each rebuilt component's new
registry digest, and re-sign if any signature-covered manifest content changes.
Prebuilt components from the earlier experimental worlds are not a conformance
target; this is an intentional pre-stability break while `wit/v0/.frozen` is
absent.

**Targeting a minor bump (e.g. 0.1 → 0.2):** recompile. No source changes
needed for items added via `@since`.

**Targeting a new major version (e.g. V0 → V1):**

1. Update the `package` declaration to `zeroclaw:plugin@1.0.0`.
2. Update import paths to reference the new interfaces.
3. Adapt to any renamed/removed items per the V1 CHANGELOG entry.
4. Recompile targeting `wasm32-wasip2`.
