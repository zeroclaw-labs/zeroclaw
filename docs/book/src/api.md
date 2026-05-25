# API Reference

Full rustdoc for every public type in the workspace, auto-generated from the `///` comments on each type, function, and module. Use this when you need to know the exact shape of a struct, the methods on a trait, or what a function returns — anything the generated reference exposes better than prose can.

**[Open the rustdoc →](../api/zeroclaw/index.html)**

## How to navigate it

- The sidebar on the left lists every crate in the workspace
- Click `zeroclaw-api` first — that's where the public traits (`Provider`, `Channel`, `Tool`) live
- Use `cmd/ctrl+F` in the rustdoc page to search within a crate
- Click on any trait to see implementors across the workspace

## Crate index

| Crate | What it exposes |
|---|---|
| [`zeroclaw`](../api/zeroclaw/index.html) | Top-level umbrella with re-exports |
| [`zeroclaw-api`](../api/zeroclaw_api/index.html) | Public traits: `Provider`, `Channel`, `Tool`, `StreamEvent` |
| [`zeroclaw-config`](../api/zeroclaw_config/index.html) | Config schema, autonomy types, secrets |
| [`zeroclaw-runtime`](../api/zeroclaw_runtime/index.html) | Agent loop, security, SOP, onboarding |
| [`zeroclaw-providers`](../api/zeroclaw_providers/index.html) | Every LLM-provider implementation |
| [`zeroclaw-channels`](../api/zeroclaw_channels/index.html) | Messaging integrations |
| [`zeroclaw-gateway`](../api/zeroclaw_gateway/index.html) | HTTP/WebSocket gateway |
| [`zeroclaw-tools`](../api/zeroclaw_tools/index.html) | Agent-callable tools |
| [`zeroclaw-memory`](../api/zeroclaw_memory/index.html) | Conversation memory, embeddings |
| [`zeroclaw-plugins`](../api/zeroclaw_plugins/index.html) | WASM plugin host |
| [`zeroclaw-hardware`](../api/zeroclaw_hardware/index.html) | GPIO / I2C / SPI / USB |
| [`zeroclaw-infra`](../api/zeroclaw_infra/index.html) | Tracing, metrics |

See [Architecture → Crates](./architecture/crates.md) for a plain-English description of how the crates fit together.

## Regenerating the API reference

The rustdoc ships with every doc deploy. For local builds:

```bash
cargo mdbook refs     # generates CLI + config reference + rustdoc
cargo mdbook build    # rebuilds the full book including rustdoc bridge
```

See [Maintainers → Docs & Translations](./maintainers/docs-and-translations.md).
