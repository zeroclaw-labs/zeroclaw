# Writing a Tool Plugin

This is the entry-level guide of the series: a complete worked path from empty
crate to a tool the model calls in conversation. The tool built here is
`redact`, which masks emails, known credential prefixes, and operator-supplied
patterns in text. It is deliberately config-driven, because reading your own
jailed config section is the thing every non-trivial plugin needs and the
thing easiest to get wrong.

Everything on this page is checked against the contract source: the
`tool-plugin` world in `wit/v0/tool.wit`, the host-side call path in
`crates/zeroclaw-plugins/src/runtime.rs` and `wasm_tool.rs`, and manifest
validation in `host.rs`. Source paths are citations into the ZeroClaw
repository for verification; the plugin itself is your own crate in your own
repository. You never need a ZeroClaw checkout to build one, only the `wit/`
contract files (fetched in step 1) and an installed `zeroclaw` binary with
the plugin host compiled in to run it.

> **The release binary is not that binary.** The prebuilt binaries the
> installer ships do not include the plugin host (`zeroclaw plugin …` is an
> unrecognized subcommand), and `plugins-wasm` is not in the crate's default
> feature set. Build the host side from source, and note the backend
> features do **not** imply the umbrella: `--features plugins-wasm-cranelift`
> alone builds cleanly and still produces a plugin-less binary, because the
> runtime integration is gated on `plugins-wasm` itself. The working
> invocation is:
>
> ```bash
> cargo build --release --features plugins-wasm,plugins-wasm-cranelift
> ```
>
> The [protocol page](../developing/plugin-protocol.md#build-features)
> documents the backend choices.

## How a tool call flows

Understand the runtime shape before writing code:

1. At startup, discovery finds your plugin directory, validates the manifest,
   and runs signature policy. Survivors become `WasmTool` instances.
2. At registration, the host instantiates the component once to read
   `name`, `description`, and `parameters-schema`. These are cached; they are
   never re-asked. If that probe fails, the tool registers under its manifest
   name and description with a generic single-`input` fallback schema, and
   the failure is logged.
3. Per call, `WasmTool::execute` creates a **fresh store** (new WASI context,
   new fuel budget, no state from the previous call), instantiates the
   component, injects your config under `__config`, and invokes `execute`.

The fresh-store-per-call model is the design constraint that matters most:
a tool plugin is stateless by construction. Anything you want to persist
between calls has to live outside the plugin (in the text you return, or in
operator config).

## 1. Crate setup

{{#include ../_snippets/plugin-crate-setup.md}}

## 2. Split logic from glue

Put the actual behavior in a plain Rust module with no wit-bindgen imports,
and keep the component glue thin. The reason is testability: the component
target cannot run `cargo test` natively, so logic trapped in the glue is logic
you can only verify end to end through a wasm host. The glue should be too
thin to be wrong.

`src/redact.rs` holds a config struct and a pure function:

```rust
use std::collections::HashMap;

pub const DEFAULT_REPLACEMENT: &str = "[REDACTED]";

/// Redaction policy resolved from the plugin's own config section.
pub struct RedactConfig {
    pub replacement: String,
    pub redact_emails: bool,
    pub patterns: Vec<String>,
}

impl RedactConfig {
    /// Build from the flat string map the host injects. Absent or empty keys
    /// fall back to defaults, which is also what an unprivileged plugin
    /// (no config_read) sees.
    pub fn from_section(section: &HashMap<String, String>) -> Self {
        let replacement = section
            .get("replacement")
            .filter(|v| !v.is_empty())
            .cloned()
            .unwrap_or_else(|| DEFAULT_REPLACEMENT.to_string());
        let redact_emails = section
            .get("redact_emails")
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(true);
        let patterns = section
            .get("patterns")
            .map(|v| {
                v.split(',')
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Self { replacement, redact_emails, patterns }
    }
}

/// Redact the input. Returns the output and the number of masked spans.
pub fn redact(input: &str, cfg: &RedactConfig) -> (String, usize) {
    // Mask emails when cfg.redact_emails, credential prefixes
    // (sk-, ghp_, AKIA, xoxb-), and each literal in cfg.patterns,
    // replacing every hit with cfg.replacement.
    // ...
}
```

Note the config shape: the host hands you a flat `string -> string` map, so
every typed field is a parse-with-default. Design the defaults so an **empty
map produces safe behavior**. The empty map is not an edge case; it is exactly
what your plugin receives when the operator has not configured it, and when
the manifest lacks `config_read` entirely (the host substitutes an empty map,
per `effective_config` in `runtime.rs`).

## 3. Implement the world

`wit/v0/tool.wit` defines the surface you must export. The world is:

```wit
world tool-plugin {
    import logging;
    export plugin-info;
    export tool;
}
```

and the `tool` interface is four functions:

```wit
record tool-result {
    success: bool,
    output: string,
    error: option<string>,
}

name: func() -> string;
description: func() -> string;
parameters-schema: func() -> json-string;
execute: func(args: json-string) -> result<tool-result, string>;
```

`src/lib.rs` generates the guest bindings and implements both exports:

```rust
pub mod redact;

#[cfg(target_family = "wasm")]
mod component {
    wit_bindgen::generate!({
        path: "wit/v0",
        world: "tool-plugin",
        features: ["plugins-wit-v0"],
    });

    use std::collections::HashMap;

    use crate::redact::{redact, RedactConfig};
    use exports::zeroclaw::plugin::plugin_info::Guest as PluginInfo;
    use exports::zeroclaw::plugin::tool::{Guest as Tool, ToolResult};
    use zeroclaw::plugin::logging::{
        log_record, LogLevel, PluginAction, PluginEvent, PluginOutcome,
    };

    struct RedactPlugin;

    #[derive(serde::Deserialize)]
    struct ExecuteArgs {
        text: String,
        #[serde(rename = "__config", default)]
        config: HashMap<String, String>,
    }

    impl PluginInfo for RedactPlugin {
        fn plugin_name() -> String {
            "my-redact-plugin".to_string()
        }
        fn plugin_version() -> String {
            "0.1.0".to_string()
        }
    }

    impl Tool for RedactPlugin {
        fn name() -> String {
            "redact".to_string()
        }

        fn description() -> String {
            "Redact secrets and PII from text before it reaches a log, \
             channel, or model. Masks emails, credential prefixes, and \
             operator-configured literal patterns."
                .to_string()
        }

        fn parameters_schema() -> String {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "The text to redact."
                    }
                },
                "required": ["text"]
            })
            .to_string()
        }

        fn execute(args: String) -> Result<ToolResult, String> {
            let parsed: ExecuteArgs = match serde_json::from_str(&args) {
                Ok(a) => a,
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("invalid arguments: {e}")),
                    });
                }
            };

            let cfg = RedactConfig::from_section(&parsed.config);
            let (output, count) = redact(&parsed.text, &cfg);

            log_record(
                LogLevel::Info,
                &PluginEvent {
                    function_name: "my_redact_plugin::tool::execute".into(),
                    action: PluginAction::Complete,
                    outcome: Some(PluginOutcome::Success),
                    duration_ms: None,
                    attrs: Some(format!("{{\"redactions\":{count}}}")),
                    message: "redacted input".into(),
                },
            );

            Ok(ToolResult { success: true, output, error: None })
        }
    }

    export!(RedactPlugin);
}
```

Contract points, each anchored in the host source:

- **`plugin-info` is a required export of every world.** It reports the
  component's own name and version. Keep both in sync with the manifest.
- **Metadata is read once.** `call_tool_metadata` in `runtime.rs` reads
  `name`, `description`, and `parameters-schema` at registration and caches
  them. Do not compute them from anything dynamic; they will never be
  re-observed.
- **The schema is the model's entire view of your tool.** The host parses it
  as JSON at load (`tool parameters-schema is not valid JSON` is a hard
  registration failure) and forwards it to the LLM verbatim. Describe every
  property. Never declare `__config` in it: that key is host-reserved, and
  the host strips any caller-supplied value before injection precisely so the
  model cannot pose as your operator.
- **`success: false` versus `Err`.** A `ToolResult` with `success: false`
  flows back to the model as a normal tool response it can react to (retry
  with fixed arguments, apologize, pick another tool). An `Err(String)`
  crosses the boundary as a plugin fault: the host wraps it as
  `plugin execute returned error` and the call fails. Reserve `Err` for
  genuinely broken states, and report bad input via `success: false`.
- **Log through the imported `logging` interface, never `wasi:logging`.**
  `log-record` is fire-and-forget; the host absorbs all errors so a failed
  log write can never crash your call, and events land in every destination
  `zeroclaw_log` writes to, carrying the
  [`zeroclaw.*` attribution](../ops/observability.md#zeroclaw-attribution)
  (`agent_alias`, `session_key`, provider, channel) of the host span your
  call runs under. Note the `attrs` field on `plugin-event` is **not**
  attribution: it is the free-form `attributes` payload of the log row.
  Attribution is alias-bound, inherited from the ambient tracing span on the
  host side, and nothing a plugin sends can set or clobber it.
  `PluginAction` and `PluginOutcome` are closed enums mirroring the host
  taxonomies; there is no free-form variant on purpose. Pick the closest.

## 4. The `__config` jail

A plugin never reads process environment variables and never sees global
config. When (and only when) the manifest declares `config_read`, the host
resolves the config section stored under your plugin's name and merges it
into the `execute` arguments under the reserved `__config` key. The mechanics
(`inject_config` / `effective_config` in `runtime.rs`):

- Any `__config` already present in the model-supplied arguments is deleted
  first. Spoofing is structurally impossible.
- Without `config_read`, the injected section is the empty map, so the
  arguments carry no `__config` key at all. This is why the
  `#[serde(default)]` on the field and the defaults-from-empty-map design in
  step 2 are load-bearing, not defensive fluff.
- Values arrive already decrypted. Operators set them per plugin name through
  zerocode, the gateway, or the CLI (`zeroclaw config set`), and they encrypt
  at rest under the config's secret key. Which keys exist is entirely your
  contract; document them in your plugin's README.

For this tool the section is three keys, all optional: `replacement` (mask
string, default `[REDACTED]`), `redact_emails` (`true`/`false` string, default
true), `patterns` (comma-separated literals, default empty).

## 5. The manifest

{{#include ../_snippets/plugin-manifest-fields.md}}

For this plugin: `name` and `version` matching what `plugin-info` reports,
`wasm_path` naming the component file you will ship next to it,
`capabilities` containing exactly `tool`, and `permissions` containing exactly
`config_read`. Add `http_client` only if your tool makes outbound HTTP calls;
that permission is what attaches the `wasi:http` context to your store, and
without it there is no network surface at all.

### Tools that call the network

Arguably the most common real-world tool shape is not a pure transform like
`redact` but a bridge to an external API: declare `http_client` in the
manifest, read credentials from `__config`, make an outbound request. The
missing piece relative to this guide is an HTTP client that works inside a
component: `reqwest` and friends do not, because there is no socket surface,
only `wasi:http`. A client known to work against this host is
[`waki`](https://crates.io/crates/waki), which is blocking and therefore fits
`execute`'s synchronous signature directly. Add it gated to the component
target so your pure-logic modules stay natively testable:

```bash
cargo add waki --target 'cfg(target_family = "wasm")'
```

The shape of a call, inside `execute` after parsing `__config`:

```rust
let resp = waki::Client::new()
    .get("https://api.example.com/search")
    .query([("q", term.as_str())])
    .header("Authorization", format!("Bearer {api_key}"))
    .connect_timeout(std::time::Duration::from_secs(5))
    .send()
    .map_err(|e| format!("request failed: {e}"))?;
```

Two version facts that look like breakage but are not: waki vendors its own
wit-bindgen (0.34) alongside the 0.46 your world bindings use; the two
coexist, each generating its own bindings. And waki emits `wasi:http@0.2.4`
imports while the current toolchain baseline is `@0.2.6`; the host links
both without issue. Neither requires action.

Remember the trust framing from the [overview](./index.md): `http_client` is
all-or-nothing. The sandbox does not bound where a granted plugin sends
data, so operators running `strict` signature policy are trusting your code,
not a URL allowlist.

## 6. Test the logic natively

Because `redact.rs` has no wasm dependency, plain `cargo test` covers it on
the host:

```rust
#[test]
fn empty_config_falls_back_to_defaults() {
    let cfg = RedactConfig::from_section(&HashMap::new());
    let (out, n) = redact("mail me at a@b.example", &cfg);
    assert_eq!(n, 1);
    assert!(out.contains("[REDACTED]"));
}
```

Cover at minimum: the jail case (empty section), the configured case, and
clean pass-through of text with nothing to mask. Every behavior the glue
forwards should be provable here without a wasm toolchain in sight.

## 7. Build

{{#include ../_snippets/plugin-build-component.md}}

## 8. Install and verify

{{#include ../_snippets/plugin-install-layout.md}}

## 9. Run it

Ask the agent to use the tool:

```text
> redact this before you log it: key sk-live-abc123, mail ops@example.com
```

The model sees `redact` in its catalog with your schema, calls it, and the
host runs the component in a fresh store under the configured fuel and memory
limits. Plugin tools are not in the builtin read-only auto-approve set, so at
non-full autonomy the call surfaces the operator approval prompt like any
other privileged tool; anticipate that in your tool description rather than
being surprised by it. Your `log-record` events appear in the structured log
with the
[span attribution](../ops/observability.md#zeroclaw-attribution) of the host
call site.

Two operational constraints worth repeating from the
[plugins overview](./index.md):

- **Tool names must not collide with built-ins.** Built-in tools register
  first and dispatch resolves first-match (`find_tool` in the runtime), so a
  plugin tool named like a built-in is never selected. There is no error;
  there is just silence. Pick a unique name.
- **One tool per component.** The `tool-plugin` world exports a single `tool`
  interface. A toolbox is several plugin directories, one component each.

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| Plugin missing from `zeroclaw plugin list` | Plugin system disabled; malformed manifest; `wasm_path` file missing; signature policy rejected it. The startup log carries the specific skip warning. |
| Tool registered but schema is a generic `input` object, name from manifest | The metadata probe failed at registration (the host substitutes the manifest name, description, and a fallback single-`input` schema; `wasm_tool.rs`). Check the log for the probe error; usually a component built against mismatched WIT. |
| Tool never selected by the model | Name collides with a built-in, or the description/schema do not tell the model when the tool applies. |
| `__config` absent despite configured section | Manifest lacks `config_read`, or the entry name does not match the manifest `name`. |
| Call traps | Fuel or memory ceiling hit. Raise `plugins.limits.call_fuel` / `plugins.limits.max_memory_mb`, or do less per call. |
| Load fails on a runtime-only host | You shipped `.wasm` to a host with no JIT; ship a version-matched `.cwasm` instead. |

## Next

- [Writing a channel plugin](./writing-a-channel-plugin.md) for the warm-store
  lifecycle, capability flags, and host-fed inbound.
- [Distributing plugins](./distributing-plugins.md) when this tool should
  leave your machine.
