# Writing a Channel Plugin

A channel plugin is a messaging-platform integration: it delivers the agent's
responses to a platform and surfaces the platform's messages to the agent. It
is the most involved plugin kind, because a channel is long-lived, stateful,
and interacts with the runtime through a
{{#include ../_snippets/plugin-channel-func-count.md}}-function surface of
which only {{#include ../_snippets/plugin-channel-required-count.md}} are
mandatory.

This guide assumes you have built the [tool plugin](./writing-a-tool-plugin.md)
and understand crate setup, the `__config` rule, logging, and install. It is
checked against `wit/v0/channel.wit` and the host adapter in
`crates/zeroclaw-plugins/src/wasm_channel.rs`.

> **Wiring status.** The host side of channel plugins is complete and
> unit-covered: `WasmChannel` implements the runtime's `Channel` trait, and
> `PluginHost::channel_plugin_details()` exposes discovered channel plugins.
> The remaining seam is orchestrator registration plus the per-vendor host
> listener; until that lands, a channel plugin loads and passes its contract
> tests but is not yet constructed by a running daemon. Build against the
> contract now; the contract is what freezes.

## The lifecycle

A channel plugin's runtime shape differs from a tool's in three fundamental
ways, and each drives a design decision in your code:

1. **One warm store for the plugin's lifetime.** The host instantiates your
   component once (`WasmChannel::from_wasm`) and holds the store behind an
   async mutex. The component may keep guest-owned protocol state between
   calls, but operator config remains host-owned. A compliant plugin **must**
   call `config.get` and `secrets.get` in every operation that needs them and
   must not copy their results into warm guest state. The host drops its
   materialized view after each call, but it cannot stop malicious guest code
   from retaining returned JSON or plaintext. The store is refueled before
   every call (`call_channel!` in `component.rs`), so a long-lived channel gets
   a fresh fuel budget per call rather than draining over its lifetime.
2. **Configuration is requested at point of use.** The host calls your
   no-argument `configure` export exactly once, at load, before any other
   export. Call `config.get` for the typed public JSON object validated against
   your manifest's `config_schema`; properties marked `x-secret = true` are
   omitted and must be read through `secrets.get`. Public and secret reads in
   `configure`, or in any later operational export, share one resolved config
   revision. A same-binding public config plus credential rotation is therefore
   visible together on the next operation. Calls during instantiation and
   static discovery return `unavailable` without resolving config. Static
   discovery includes `name`, `plugin-info`, `get-channel-capabilities`,
   `self-handle`, `self-addressed-mention`, and `multi-message-delay-ms`;
   changing bot/account identity or other static metadata requires channel
   lifecycle reconstruction.
3. **You do not listen; the host feeds you.** The WASI context has no network
   listener capability. Inbound traffic reaches you through the imported
   `inbound` interface: the host runs the actual listener (webhook server,
   vendor tunnel, polling client), enqueues each received message onto an
   `InboundQueue`, and your `poll-message` export drains it by calling
   `inbound-poll`. Batch-drain with `inbound-pending` if useful.

## Required exports

Five functions have no Rust trait default and must genuinely work
(`world channel-plugin` doc, `channel.wit`):

| Export | Contract |
|--------|----------|
| `name` | Human-readable channel name. |
| `configure` | Complete load-time initialization. It takes no arguments; call `config.get` and `secrets.get` for one current revision. An error string fails the load. |
| `send` | Deliver a `send-message` (content, recipient, optional subject/thread/attachments) to the platform. |
| `poll-message` | Non-blocking: return the next inbound message or `none` immediately. Never block; the host's poll bridge handles pacing. |
| `get-channel-capabilities` | Return the bitmask of optional methods you actually implement. Called once at load. |

The poll bridge deserves a note: the host runs a poll-to-push loop
(`listen` in `wasm_channel.rs`) that calls `poll-message` with exponential
backoff from 50ms to 500ms while the queue is empty, resetting on traffic. If
your `poll-message` traps, the host marks the channel poll-unhealthy, logs,
and backs off; a plugin whose poll keeps trapping reports unhealthy through
`health_check` even if it exports no `health-check` of its own. Trapping in
`poll-message` is therefore visible, not fatal, but it makes your channel
useless. Keep it simple: drain the queue, translate, return.

## Capability flags: the 22 optional methods

Everything else in the interface is gated by `channel-capabilities` flags.
The pattern (identical to the memory world):

- The host reads your flags once at load.
- For every **unset** flag, the host uses the Rust trait default and never
  calls your export.
- You must still export every function; a stub returning the documented
  default value compiles and is never called.

The flag-by-flag defaults are documented inline in `channel.wit` next to the
flags declaration, which is the source of truth. In summary, the groups:

| Group | Flags | What implementing buys you |
|-------|-------|---------------------------|
| Health | `health-check` | Report platform reachability; combined with poll health by the host adapter. |
| Identity | `self-handle`, `self-addressed-mention`, `drop-self-message` | Self-loop protection (the runtime drops the bot's own messages) and correct @-mention forms in the per-channel system prompt. The host caches `self-handle` and `self-addressed-mention` at load; they are read once. |
| Typing | `start-typing`, `stop-typing` | Composing indicators while the agent thinks. |
| Drafts | `supports-draft-updates`, `send-draft`, `update-draft`, `update-draft-progress`, `finalize-draft`, `cancel-draft` | Progressive message editing: the runtime streams the response into an editable platform message instead of waiting for completion. Implement all six together or none. |
| Multi-message streaming | `supports-multi-message-streaming`, `multi-message-delay-ms` | Paragraph-by-paragraph delivery with a minimum inter-message delay (default 800ms, cached at load). |
| Moderation | `add-reaction`, `remove-reaction`, `pin-message`, `unpin-message`, `redact-message` | Emoji reactions, pinning, message deletion. |
| Interaction | `request-approval`, `request-choice`, `supports-free-form-ask` | Tool-call approval prompts and multiple-choice questions presented natively on the platform. |

Start with the required {{#include ../_snippets/plugin-channel-required-count.md}} plus `health-check`, and add groups as the
platform supports them. Advertising a flag you have not implemented is worse
than omitting it: the host will call your export and trust the answer.

### The approval surface

`request-approval` is the deepest integration point. The runtime presents a
compact `approval-request` (tool name, arguments summary, optional raw JSON
arguments) and your channel renders it however the platform allows (buttons,
reactions, a reply convention). The `approval-response` variant you return
drives the security machinery:

- `approve`: execute this one call
- `deny`: refuse it
- `always-approve`: execute and add the tool to the session-scoped allowlist
- `deny-with-edit(string)`: refuse, but supply edited replacement arguments

Return `none` when the prompt cannot be presented; the caller falls back to
auto-deny. Fail closed.

## Inbound message shape

Translate platform events into `inbound-message` records faithfully. The
runtime's threading logic keys off the platform payload fields, while routing
identity comes only from the host-issued endpoint
(`channel.wit`, `from_wit_inbound` in `wasm_channel.rs`):

- `id`, `sender`, `content`: the basics. `reply-target` is where a response
  should go (channel ID, chat ID, email address).
- `channel` and `channel-alias` are legacy hints retained in the v0 record. The
  host ignores both for routing and stamps the admitted channel type and
  configured binding, so a plugin cannot select another owner or session.
- `thread-ts` carries the platform's thread identifier for threaded replies;
  `subject` exists for email threading.
- `interruption-scope-id` groups messages for interruption/cancellation.
  Leave it `none` for top-level messages.
- `attachments` carry full raw bytes across the boundary (`media-attachment`:
  file name, bytes, optional MIME type). A voice note is several megabytes
  crossing by value; this is the documented cost of the 32-bit boundary, and
  a resource-handle model is explicitly deferred to a future WIT revision.

On the outbound side, `send-message` mirrors the same fields; the Rust
`SendMessage`'s cancellation token is deliberately omitted from the WIT
record because it is a host-side concept with no meaning inside the plugin.

## Skeleton

The structure, omitting the per-platform translation that is your actual
work:

```rust
#[cfg(target_family = "wasm")]
mod component {
    wit_bindgen::generate!({
        path: "wit/v0",
        world: "channel-plugin",
        features: ["plugins-wit-v0"],
    });

    use exports::zeroclaw::plugin::channel::{
        ApprovalRequest, ApprovalResponse, ChannelCapabilities,
        Guest as Channel, InboundMessage, SendMessage,
    };
    use exports::zeroclaw::plugin::plugin_info::Guest as PluginInfo;
    use zeroclaw::plugin::config::get as config_get;
    use zeroclaw::plugin::inbound::inbound_poll;
    use zeroclaw::plugin::secrets::get as secret_get;

    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ChannelConfig {
        api_base: String,
    }

    fn current_config() -> Result<ChannelConfig, String> {
        let json = config_get().map_err(|_| "public config is unavailable".to_string())?;
        serde_json::from_str(&json).map_err(|e| format!("invalid config JSON: {e}"))
    }

    fn current_api_token() -> Result<String, String> {
        secret_get("api_token").map_err(|_| "api_token is unavailable".to_string())
    }

    fn current_inputs() -> Result<(ChannelConfig, String), String> {
        // Both imports in this export share one resolved canonical revision.
        Ok((current_config()?, current_api_token()?))
    }

    struct MyChannel;

    impl Channel for MyChannel {
        fn name() -> String {
            "my-platform".to_string()
        }

        fn configure() -> Result<(), String> {
            let (config, api_token) = current_inputs()?;
            validate_configuration(&config.api_base, &api_token)
        }

        fn send(message: SendMessage) -> Result<(), String> {
            let (config, api_token) = current_inputs()?;
            // Outbound platform delivery via wasi:http
            // (requires the http_client permission in the manifest). Build the
            // request from this call's values; never retain a second copy.
            send_to_platform(&config.api_base, &api_token, message)
        }

        fn poll_message() -> Option<InboundMessage> {
            // Drain the host-fed queue and translate.
            inbound_poll().map(translate_inbound)
        }

        fn get_channel_capabilities() -> ChannelCapabilities {
            ChannelCapabilities::HEALTH_CHECK
        }

        fn health_check() -> bool {
            current_inputs().is_ok()
        }

        // Every other method: a stub returning the WIT-documented default.
        // The host never calls them while their flag is unset.
        // ...
    }

    export!(MyChannel);
}
```

`current_inputs` is deliberately called at point of use. The host binds both
imports to this admitted package, `channel` capability, and alias; reads in one
export share one resolved config revision, while the next export can observe a
same-binding public config plus credential rotation. `ChannelConfig` is a
per-call typed view and is dropped with the token. Do not add a `thread_local`
config or credential cache.

## Manifest and permissions

{{#include ../_snippets/plugin-manifest-fields.md}}

For a channel: `capabilities` containing `channel`, and almost certainly
`config_read` because platforms require credentials. Request the narrow network
surface the implementation actually uses: `http_client` for outbound
`wasi:http`, `websocket_client` for the host-mediated WebSocket resource, or
`socket_client` for typed TCP, direct TLS, or STARTTLS. A platform with separate
REST and streaming APIs may need more than one. None allows a listener; inbound
host-owned traffic arrives through `inbound`. The channel adapter implements
these network surfaces but links each one only after its grant is validated;
without both adapter support and the corresponding grant, `send` has no network
path to the platform.

Pair `config_read` with the schema consumed by `ChannelConfig`:

```toml
name = "my-platform"
version = "0.1.0"
wasm_path = "my_platform.wasm"
capabilities = ["channel"]
permissions = ["config_read", "http_client"]

[config_schema]
"$schema" = "https://json-schema.org/draft/2020-12/schema"
type = "object"
additionalProperties = false
required = ["api_base", "api_token"]

[config_schema.properties.api_base]
type = "string"
minLength = 1

[config_schema.properties.api_token]
type = "string"
minLength = 1
x-secret = true
```

The host validates both properties as one object. `config.get` returns typed
JSON containing `api_base` and omits `api_token`, which is available only through
`secrets.get`. Because both are required, withholding `config_read` fails closed
before guest code runs instead of starting a channel without required config.
Each channel instance selects the `plugins.entries` key derived from its full
package, `channel` capability, and binding identity while reusing this one
package-owned schema. Identical aliases in different packages therefore remain
isolated.

Call `config.get` and `secrets.get` inside each operation that uses them. The
host resolves at most one canonical revision for that call and drops its view
afterward. A public config plus credential rotation within the same logical
binding is visible together on the next operation without daemon reload or
channel reconstruction. Changing the bot/account identity, advertised
capabilities, self-handle, mention, or other load-time metadata requires channel
lifecycle reconstruction because those exports are read once during static
discovery.

For an optional schema whose empty object is valid, an instance denied the
effective `config_read` grant can load, but `config.get` and `secrets.get` return
`access-denied`. Either import returns `unavailable` during instantiation or
static discovery, after resolver/validation failure, or when the shared host-call
budget is exhausted. `secrets.get` additionally returns `not-found` for a name
that is absent or not marked `x-secret = true`.

## Build and install

{{#include ../_snippets/plugin-build-component.md}}

{{#include ../_snippets/plugin-install-layout.md}}

## Testing against the host contract

The host adapter and config resolver tests are the executable specification:
they cover typed materialization and schema validation, point-of-use public and
secret scope, coherent same-revision rotation, denied grants, static-discovery
denial, the inbound queue handoff, capability-gated dispatch, and poll-health
accounting.

To run your own component under those exact semantics, write an integration
test that instantiates it through the real host adapter. `zeroclaw-plugins`
is not published to crates.io, so pull it as a git dev-dependency pinned to
the tag matching your target host:

```bash
cargo add --dev zeroclaw-plugins \
  --git https://github.com/zeroclaw-labs/zeroclaw --tag <host-version> \
  --no-default-features --features plugins-wasm-cranelift
```

The test then wraps a `PluginConfigResolver::new` backed by the manifest and
test operator values in `PluginHostServices`, loads your component through
`WasmChannel::from_wasm`, enqueues onto the `InboundQueue` handle it exposes,
and asserts your `poll-message` drains and translates the message. That is the
same code path a production daemon will run; passing it is the strongest
pre-distribution signal you can get without a live host.

## Next

- [Writing a memory plugin](./writing-a-memory-plugin.md): the other
  warm-store world, with agent attribution semantics.
- [Distributing plugins](./distributing-plugins.md) for signing and registry
  publication.
