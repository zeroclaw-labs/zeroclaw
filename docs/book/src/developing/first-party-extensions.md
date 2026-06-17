# First-Party Extension Architecture

Use this page when adding or changing a built-in ZeroClaw provider, channel, tool, memory backend, or hardware/peripheral integration.

This page is not for out-of-process plugins. If the extension can live outside the core binary, start with [Plugin protocol](./plugin-protocol.md) or [MCP](../tools/mcp.md) before adding a first-party implementation.

## Choose The Smallest Surface

Before writing code, decide which extension surface owns the behavior.

| Work | First stop | Usually do not change |
|---|---|---|
| OpenAI-compatible model endpoint | [Custom providers](../providers/custom.md) | `zeroclaw-runtime` |
| New model wire format or provider family | `crates/zeroclaw-api/src/model_provider.rs`, `crates/zeroclaw-providers/`, `crates/zeroclaw-config/` | Channels, tools |
| New messaging platform | `crates/zeroclaw-api/src/channel.rs`, `crates/zeroclaw-channels/`, `crates/zeroclaw-config/` | Provider routing |
| New agent-callable capability | `crates/zeroclaw-api/src/tool.rs`, `crates/zeroclaw-tools/`, risk policy docs | Channel adapters |
| New memory backend | `crates/zeroclaw-api/src/memory_traits.rs`, `crates/zeroclaw-memory/` | Provider or channel code |
| New hardware/peripheral surface | `crates/zeroclaw-api/src/peripherals_traits.rs`, `crates/zeroclaw-hardware/` | Runtime agent loop |

If the change needs new config, security policy, generated docs, or runtime routing, it is more than a local adapter. Check the [architecture map](../contributing/architecture-map.md) and [RFC process](../contributing/rfcs.md) before implementation.

## Source Of Truth

Do not duplicate state across config, runtime handles, generated docs, and UI state. Before adding a field or cache, name the source of truth.

| Data | Source of truth |
|---|---|
| Provider family slots, aliases, endpoint defaults, auth fields | `crates/zeroclaw-config/src/providers.rs` and the typed provider config structs |
| Channel aliases, secrets, allowed peers, channel-local config | `crates/zeroclaw-config/src/schema.rs` and peer-group/IAM config |
| Tool specs and model-visible descriptions | The `Tool` implementation's `spec()`, `description()`, and schema; Fluent catalogues such as `crates/zeroclaw-runtime/locales/en/tools.ftl` own localised tool text where the tool is wired through that path |
| Gateway API schema consumed by the dashboard | `zeroclaw_gateway::openapi::build_spec()` |
| Generated config docs | The config schema generator; do not edit generated output by hand |
| Provider, channel, tool, memory, and peripheral trait contracts | `crates/zeroclaw-api/src/model_provider.rs`, `channel.rs`, `tool.rs`, `memory_traits.rs`, and `peripherals_traits.rs` |
| Attribution and logging contracts | `crates/zeroclaw-api/src/attribution.rs` and [Logging architecture](../architecture/logging.md) |
| Runtime turn execution | `crates/zeroclaw-runtime/src/agent/turn/` |

Valid patterns resolve from the source when needed: pass `&Config`, resolve through a factory, use a closure over the live config handle, or build a per-call materialized view. Avoid storing long-lived snapshots such as `allowed_users: Vec<String>` inside a channel handle when the canonical data lives in config.

## Shared Implementation Checklist

Use this checklist when adding a new first-party extension or materially expanding an existing one.

1. **Read a similar implementation first.** Match the closest existing provider, channel, tool, memory backend, or hardware module before inventing a new pattern.
2. **Implement the public trait.** Keep the extension behind the relevant `zeroclaw-api` trait. Do not patch the runtime loop just to special-case one integration.
3. **Register through the factory.** Wire the implementation at the existing factory boundary when the change adds a new implementation. Add feature flags only when the dependency or platform surface justifies them.
4. **Add config at the schema boundary.** When a config key changes, define defaults, compatibility behavior, env-var expectations, and migration impact at the schema/source layer.
5. **Keep security at the right edge.** Channels enforce channel-specific authentication, peer-group/allowlist, and pairing policy before messages reach the runtime. Tools go through autonomy/risk policy and receipts. Providers must not smuggle tool or credential policy into model I/O code.
6. **Implement attribution and logging.** New first-party integrations should emit logs through the project logging surface with the correct kind/alias attribution rather than ad hoc tracing fields.
7. **Localize user-facing strings.** CLI messages, tool descriptions, onboarding prompts, and UI text use Fluent or the owning UI catalogue. Logs and tracing remain stable English.
8. **Document the operator surface.** If the extension adds config, commands, install steps, security behavior, or compatibility requirements, update the matching docs.
9. **Test the factory and edge cases.** Cover factory registration when applicable, plus config parsing/defaults, auth or policy rejection paths, error handling, and at least one success path.
10. **Keep the PR narrow.** Do not bundle an adapter, a runtime refactor, a config migration, and docs reorganization unless the change cannot work without all of them.

## Surface-Specific Notes

### Providers

Prefer the `custom` slot or an existing OpenAI-compatible family when the endpoint speaks a supported wire protocol. Add a first-party provider only when the endpoint needs a new family, auth model, capability declaration, or wire translation.

A provider PR usually touches:

- `crates/zeroclaw-config/src/providers.rs` and typed config structs;
- `crates/zeroclaw-providers/src/`;
- provider catalog/config docs;
- tests for factory routing, aliases, endpoint defaults, capability flags, and error handling.

### Channels

Channels are user-visible trust boundaries. Keep platform decoding, deduplication, reply-target construction, sender authorization, and any channel-specific pairing at the channel edge. Do not let unauthenticated or unauthorized events reach the runtime and then rely on the runtime to sort them out.

A channel PR usually touches:

- `crates/zeroclaw-api/src/channel.rs` only when the shared channel contract really changes;
- `crates/zeroclaw-channels/src/`;
- channel config schema and feature flags;
- channel docs, including setup and security notes;
- tests for inbound filtering, outbound formatting, retry/error behavior, and factory wiring.

### Tools

Tools are agent actions, so they must fit the autonomy model. A tool PR should define the tool's risk, arguments, result shape, receipt behavior, and user-facing description.

A tool PR usually touches:

- `crates/zeroclaw-tools/src/`;
- `crates/zeroclaw-runtime/locales/en/tools.ftl`;
- security/autonomy docs when risk behavior changes;
- tests for schema, allow/deny behavior, success, failure, and receipt-facing output.

### Memory Backends

Memory backends must preserve agent/session scoping. Do not bypass the agent-scoped wrapper or store unscoped data because it is easier for one backend.

A memory PR usually touches:

- `crates/zeroclaw-memory/`;
- config schema and setup docs;
- tests for store, recall, list, forget, health, agent scoping, and backend-specific failure modes.

### Hardware And Peripherals

Hardware integrations are platform-sensitive and can expose real devices. Keep unsafe calls, permissions, device discovery, and platform gates near the hardware crate. Document required OS packages, udev or permission steps, and failure behavior. If a peripheral is exposed through an agent-callable tool, it also follows the `Tool` and attribution contracts unless the team deliberately adds a new attribution role.

## When To Stop And Ask For An RFC

The [RFC process](../contributing/rfcs.md) is the canonical source for whether a change is RFC-shaped. For first-party extensions, stop and check it especially when the extension:

- changes a public trait used by multiple crates or plugins;
- adds a config migration that affects existing users;
- broadens file, shell, network, credential, or device access;
- changes runtime turn execution or tool approval semantics;
- changes generated API contracts used by clients;
- adds a heavy dependency or new always-on service;
- creates a new long-term ownership area that maintainers must staff.

## See Also

- [Architecture overview](../architecture/overview.md)
- [Crates](../architecture/crates.md)
- [Extension examples](./extension-examples.md)
- [Custom providers](../providers/custom.md)
- [Channels overview](../channels/overview.md)
- [Tools overview](../tools/overview.md)
- [Building the web dashboard](./web.md)
