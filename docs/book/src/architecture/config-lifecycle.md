# Config lifecycle

Configuration is both an operator interface and a runtime contract. Treat it as
state with a clear owner, not as loose settings copied into whichever subsystem
needs them.

The canonical source is `zeroclaw_config::schema::Config`, loaded from
`config.toml`. User-facing config surfaces, the generated config reference, the
gateway config editor, env-var overrides, `zeroclaw config set`,
`zeroclaw config patch`, and Quickstart all route through that same typed
schema.

For the build order, tracked-output rules, and drift checks that turn the typed schema into the config reference, see [Generated documentation pipeline](./generated-documentation-pipeline.md).

## What owns what

| Surface | Owner | Persistence boundary | Runtime apply boundary |
| --- | --- | --- | --- |
| Config schema | `crates/zeroclaw-config/src/schema.rs` plus `Configurable` derives | Code, not generated docs | New binary build |
| Generated reference | `cargo mdbook refs` / `markdown-schema` | `docs/book/src/reference/config.md` at build time | Documentation only |
| Bootstrap location | `ZEROCLAW_CONFIG_DIR`, `ZEROCLAW_DATA_DIR`, deprecated `ZEROCLAW_WORKSPACE` | Environment only | Before `Config` exists |
| Schema-mirror overrides | `ZEROCLAW_<lowercase_path>` with `__` for dots | In-memory only | Each `Config::load_or_init()` |
| CLI config writes | `zeroclaw config set`, `config patch`, aliases, model helpers | `save_dirty()` to `config.toml` | Next load/reload unless the current command uses the new in-memory value |
| Quickstart apply | CLI apply path | `save_dirty()` to `config.toml` | Web can signal daemon reload; standalone CLI applies on next load/reload |
| Gateway config writes | Config API handlers and `persist_and_swap()` | `save_dirty()` to `config.toml` | Gateway-visible state updates immediately; daemon subsystems apply after reload |
| Daemon reload | `/admin/reload` or the in-process reload channel | Re-reads `config.toml` | Recreates daemon subsystems in the same PID |

Do not hand-edit the generated config reference. If a field, enum, alias
section, secret marker, or description is wrong there, fix the schema or the
generator and regenerate the reference.

## Load order

Config load has a few distinct phases:

1. Resolve the install root from bootstrap env vars. This happens before any
   `Config` exists, so bootstrap names keep their uppercase form and do not use
   the schema-mirror grammar.
2. Read `config.toml`, run schema migration in memory, decrypt configured
   secrets, and record any malformed security-critical sections as degraded
   security.
3. Apply schema-mirror overrides to the in-memory config. In env vars,
   `__` maps to `.`, so `ZEROCLAW_providers__models__openai__api_key`
   targets `providers.models.openai.api_key`.
4. Validate and warn without locking the operator out of the gateway editor.

On a fresh install, defaults are saved before env overrides are applied. This
keeps env-injected secrets and local CI values out of the new file.

## Proxy runtime ownership

`zeroclaw-config` owns `[proxy]`, URL validation and the persisted-config read
used before runtime startup. `zeroclaw-http` owns HTTP client construction,
connection caching and the process-wide runtime proxy projection. Startup and
daemon reload supply that projection from the loaded config; reload invalidates
cached clients. Process proxy environment variables are applied only before the
multi-threaded runtime starts, so environment-scope changes still require a
restart. Existing clients retain their original configuration until recreated.

## Execution adapter ownership

`zeroclaw-config` owns `[runtime]`, including the shell choice and Docker
isolation settings. `zeroclaw-platform` constructs the native or Docker command
adapter from that config. The existing runtime factory call sites in CLI turns,
gateway startup and channel startup remain responsible for construction. The
shared `RuntimeAdapter` contract and typed mount errors live in `zeroclaw-api`.
Command construction does not grant approval or start an external agent;
local tools still apply their existing security policy, and external harness
sessions remain Tachi-owned under ADR-017.

## Cost accounting ownership

`zeroclaw-config` keeps `[cost]` and its validation. `zeroclaw-cost` owns
pricing, budget checks, the process-global tracker and `data/state/costs.jsonl`.
`zeroclaw-api::cost` holds the shared records and reporting types, so a reader
can consume results without linking the ledger implementation. Runtime, gateway
and CLI callers use the same tracker through the runtime forwarding module.
Reload still updates the resident tracker's policy; existing rows retain their
recorded prices. This is accounting for the existing controller paths, not a
second task ledger or an external harness execution owner.

## Authorization policy ownership

`zeroclaw-config` owns risk and runtime profiles, cards, and the canonical
command/path defaults in `policy_defaults`. `zeroclaw-policy` resolves those
settings into the existing runtime policy and enforces command, filesystem,
tool, action-budget and child-policy boundaries. The shared `ToolOperation`
and `CommandRiskLevel` classifications live in `zeroclaw-api::policy`.

Runtime callers retain `zeroclaw-runtime::security::policy` as a forwarding
module. The Tachi delegation resolver still reads current config for each
operation and shares the existing action tracker. This extraction does not
change the timing of policy resolution, introduce cached config or transfer
external session ownership from Tachi.

## Pairing and token ownership

`zeroclaw-runtime::security::pairing` owns `PairingGuard`, pairing-code
generation and consumption, per-client lockout, and in-memory token revocation.
Gateway and channel callers retain their existing runtime import paths and
shared guard handles. Operator bridge-token minting uses the same runtime
module.

Config retains the paired-token hashes and bridge settings. Its `tokens`
module owns the stateless SHA-256 encoding and exact-string comparison used by
both bridge lookup and the guard. Device-registry registration and capability
updates use that same digest function. The device registry remains a projection
of the canonical paired-token set; moving the guard does not change admission,
reload, persistence or revocation behavior.

## Schema presentation ownership

`zeroclaw-config` owns the typed schema and its `schema-export` feature.
`zeroclaw-schema-markdown` renders an in-memory JSON Schema into Markdown and
HTML field views. It has no normal dependency on config or the agent runtime;
the CLI and mdBook preprocessor supply the canonical schema at each call.
The CLI enables the renderer only through `schema-export`, including builds
without `agent-runtime`. Generated reference text and config persistence are
unchanged.

## Quickstart preset ownership

`zeroclaw-runtime::quickstart::presets` owns the risk/runtime preset catalogue,
factories, lookup and fallback policy, and provider recommendations used by
Quickstart. The CLI consumes this catalogue through the existing Quickstart
snapshot and apply paths. Provider capability reporting and provider
construction stay in `zeroclaw-providers`; the Gateway's separate config editor
and model-catalog helpers are unchanged.

Config retains submission DTOs, profile schemas and canonical policy defaults.
Preset factories still use those types and defaults. Applying a preset retains
the existing configured profile when its alias already exists; new profiles
still go through the same validation, dirty-path persistence and reload path.
Moving the catalogue does not change preset values, approval rules or external
harness ownership.

## Shared config metadata ownership

`zeroclaw-api::config_metadata` owns the shared property kinds, tabs,
credential classifications, secret/nested-option descriptors, integration
metadata and channel descriptions. Config's `traits` and derive shim re-export
the same types. `Configurable` still generates metadata from canonical config;
config retains property coercion, credential classification, secret handling,
validation and persistence. CLI, Gateway, channels and runtime read the API
contract directly without a new dependency edge.

This owner split preserves both existing tag formats: metadata serde uses
snake_case property kinds, while Gateway list responses use kebab-case tags.
It does not retire the corresponding schema or transfer execution ownership
from Tachi into ZeroClaw.

## Remaining config reduction boundaries

[Issue #382](https://github.com/kckylechen1/zeroclaw/issues/382) tracks the
consumer census; a module's size alone is not permission to remove it.

| Surface | Current consumers and next boundary |
| --- | --- |
| HTTP and local command implementations | Moved to `zeroclaw-http` and `zeroclaw-platform`; config keeps their settings. |
| Cost ledger and shared results | Moved to `zeroclaw-cost` and `zeroclaw-api::cost`; config retains `CostConfig`. Budget enforcement, ledger compatibility and reload remain covered at the new owner and runtime call sites. |
| Runtime authorization policy | Moved to `zeroclaw-policy`; profile settings and canonical defaults remain in config. Existing denial behavior and shared budgets are retained. |
| Pairing and token helpers | Stateful pairing moved to the existing runtime owner. Config keeps paired-token/bridge settings and stateless digest/comparison helpers in `tokens`; the same helpers serve runtime authentication and device registration. |
| Secrets | Retained encryption and credential paths are used by config loading, saving and editing. They are not retired settings. |
| Channel/provider schema sections | Retained listeners and inference clients still read them. Remove exclusive config only after the corresponding #378/#382 consumer replacement or retirement. |
| Schema Markdown rendering | Moved to `zeroclaw-schema-markdown`; CLI and mdBook consume JSON Schema without placing presentation code on every config consumer. Config still owns schema generation. |
| Config-list field visibility | The Gateway's private `api_config::field_visibility` owns its existing HTTP list filters. Config retains schema and persistence ownership. The canonical retention setting (`conversation_retention_days`) remains in `MemoryConfig` and is visible when `memory.backend = "tachi"`. This move does not change visibility, validation, write access or authorization. |
| Quickstart preset selection | Risk/runtime catalogues, factories and provider recommendations moved to the existing Quickstart runtime owner. Config keeps submission DTOs and canonical profile types/defaults. |
| Shared config metadata | Moved to `zeroclaw-api::config_metadata`; config keeps derive generation and compatibility re-exports. Config-specific traits, coercion, validation and writes remain config-owned. |
| `sections`, `comment_writer`, `card`, `typed_value` and onboarding DTOs | Config editing, onboarding and persistence still consume these surfaces. #379 owns editor retirement; keep canonical types and write paths until their retained callers have replacements. |
| Legacy delegation/SOP configuration | #381 owns duplicate execution retirement after replacement proof. Keep L1 reasoning, the Tachi bridge and their shared types. |

## Env overrides are not saved

Schema-mirror env vars are runtime injections. They land on the in-memory
`Config` at load time and are tracked in `env_overridden_paths` so the CLI,
dashboard, and quickstart can show the override marker.

Saving must mask these paths back to their pre-override disk or default value
before encryption. This matters most for secrets: if an operator has an
encrypted on-disk API key and temporarily boots with an env override for the
same path, an unrelated config save must not replace the real credential with
the env value or with a masked display string.

Review config changes with this invariant in mind:

- `ZEROCLAW_*` schema-mirror values affect the running process after load.
- They do not become durable config.
- Save paths must preserve encrypted secrets and external secret references
  unless the same path was intentionally edited.

## Credential inputs stay typed

Credential-like runtime values are still config values. API keys, OAuth tokens, endpoint URLs, and other provider/channel credentials should flow through the typed config schema, config secret handling, or schema-mirror `ZEROCLAW_*` overrides before a runtime constructor sees them.

Do not add ad-hoc `std::env::var("PROVIDER_API_KEY")` reads inside provider, channel, tool, transcription, TTS, memory, or gateway constructors. That creates a second credential source outside `Config`, bypasses env-override visibility, and can make CLI, gateway, quickstart, and reload behavior disagree.

If ZeroClaw intentionally supports a native environment bridge for an integration family, document that bridge at the integration boundary and map it into the same typed config value before construction. Otherwise, ecosystem-default shell names such as `ANTHROPIC_API_KEY`, `OPENROUTER_API_KEY`, or `QDRANT_URL` should be bridged by operators into the corresponding `ZEROCLAW_*` schema-mirror variable; see [Environment variables](../reference/env-vars.md#bridging-ecosystem-default-env-vars).

## Dirty paths and incremental writes

Most editing surfaces use `Config::mark_dirty()` plus `save_dirty()`, not a
full rewrite. `save_dirty()` writes only changed dotted paths, preserves
non-dirty entries and comments where possible, stamps the current
`schema_version`, and writes through an atomic temp-file replacement.

That path is also responsible for map-key sections. Creating an alias such as a
model provider, MCP server, or skill bundle must dirty the
right section so the alias survives a save and reload. A config edit that only
updates the in-memory dashboard state is not complete.

When reviewing a config write, check that:

- the edited path is marked dirty before persistence;
- map-key creates, renames, and deletes dirty the parent section or natural key;
- secret and env-overridden paths keep their save masking behavior;
- `schema_version` remains current after incremental writes;
- the changed value survives `save_dirty()` followed by reload.

## Full saves need load provenance

`Config::save()` refuses to overwrite an existing `config.toml` unless the
in-memory value was populated from that exact file (`loaded_from`, set by
`load_or_init` in both its existing-file and fresh-init branches, and by the
loaders that re-read a config file before mutating it). A
default-constructed, programmatically built, or repointed `Config` therefore
cannot replace an operator's populated config with a near-empty snapshot.
Creating a missing file (first run) still succeeds: a value that never read
a file gets exactly one create and must then reload or force, and
`force_save()` is the explicit path for a verified intentional overwrite.
When reviewing a code path that builds a fresh `Config` and expects to write
it over an existing file, either load first or justify the `force_save()`
call.

## Saved vs applied

A successful save means the file changed. It does not always mean every runtime
component has adopted the change.

The daemon owns the long-lived subsystem graph: gateway, channel listeners,
scheduler, MQTT listener, session wiring, memory backend, provider factories,
and cost wiring. `POST /admin/reload` signals the daemon loop, which re-reads
`config.toml` and re-instantiates those subsystems in the same process. The PID
stays the same, but listeners briefly rebind.

Gateway config writes call `persist_and_swap()`: save to disk, then replace the
gateway-visible in-memory config and set `pending_reload`. This makes the config
editor reflect the write immediately, while the reload banner tells the operator
that channels, providers, scheduler, or other daemon-owned components may still
be running from the previous subsystem instance.

Standalone `zeroclaw gateway start` has no daemon supervisor. Its reload
endpoint returns a restart-required response because there is no outer daemon
loop to signal.

## Reload access

Local reload is allowed from loopback. Remote reload requires both:

1. `gateway.allow_remote_admin = true`
2. pairing enabled and a valid paired bearer token

Opting into remote admin while pairing is disabled is rejected rather than
treated as anonymous remote reload access.

Security-critical malformed config sections are allowed to degrade only when
the operator explicitly opts into degraded serving. Otherwise the process
refuses to serve because reset-to-default security posture may be weaker than
the file intended.

## Rollback and repair

Config writes use an atomic temp-file replacement and owner-only permissions.
When replacing an existing file, the writer creates a same-directory
`config.toml.bak` during the replace and removes it after a successful write.
Gateway writes also snapshot the pre-write file and best-effort restore it if
persistence fails before swapping in-memory state.

There is no general transactional rollback for a valid but undesired config
change after it has been saved and applied. Restore the previous `config.toml`
from backup, edit the field back through the CLI or dashboard, then reload or
restart according to the runtime boundary above.

## Config-visible is not always runtime-supported

A field can be schema-visible before every runtime path consumes it. That is
acceptable only when the docs and review notes say so plainly.

For example, a map-key section can be schema-visible and appear in config
section APIs while a runtime consumer is still being wired. A PR that adds or
changes such a surface must be precise about
whether it only stores configuration, wires the runtime behavior, or completes
both.

When reviewing a PR that touches a schema-visible but not yet runtime-consumed
field, require the PR description to say whether runtime wiring is deferred, out
of scope, or completed by the same change.

### Retired OTP action-gating fields

The four `[security.otp]` action-gating fields are a security-sensitive example:
`gated_actions`, `gated_domains`, `gated_domain_categories`, and
`challenge_max_attempts` are compatibility-only schema fields and have no
action-execution consumer. Their removal follows the owner-ratified two-stage
[compatibility policy](https://github.com/kckylechen1/zeroclaw/issues/220#issuecomment-5390041471)
rather than an unannounced availability break:

1. **Current compatibility window.** Non-default legacy values parse and emit a
   structured `otp_action_gating_unsupported` warning that explicitly says the
   value is not enforced. Defaults and absence are unaffected. Operators should
   remove all four fields and configure the action's real authority boundary;
   keeping the warning does not preserve any protection.
2. **Next declared config-breaking/schema-removal window.** Release and migration
   notes must announce the boundary before it ships. At that boundary, an
   explicitly supplied legacy field becomes a hard config error or unsupported
   field, while absence/default behavior remains unaffected. No release number
   or date is assigned yet.

After the announced window expires, remove the retired fields and their
compatibility parser. Do not weaken the current warning, silently rewrite user
config, or add OTP matching logic to keep the old names alive. The action-owner
mapping and the distinction between authentication and authorization are in
[The security model](../security/model.md#otp-scope-and-action-authority).

## Reviewer checklist

For config-schema, env-var, default, or reload changes, ask:

- What is the source of truth for the new value?
- Is this creating duplicate state, or resolving from `Config` at use time?
- Does the generated reference come from code rather than hand-maintained
  prose?
- Are env overrides load-time only and masked during saves?
- Do CLI, gateway, and quickstart surfaces agree on the dotted path?
- Are credentials resolved through typed config or documented schema-mirror bridges rather than ad-hoc provider-native env reads?
- Does a save survive process reload, not just immediate in-memory rendering?
- Does the PR say whether users need reload, restart, migration, or manual
  rollback?
- If the field is only config-visible, does the PR avoid claiming runtime
  support?

## Source pointers

- Config schema and persistence: `crates/zeroclaw-config/src/schema.rs`
- Env override grammar: `crates/zeroclaw-config/src/env_overrides.rs`
- Config CLI commands: `src/main.rs`
- Shared Quickstart apply path: `crates/zeroclaw-runtime/src/quickstart/mod.rs`
- Web Quickstart reload signaling: `crates/zeroclaw-gateway/src/api_sections.rs`
- Gateway config API and reload banner: `crates/zeroclaw-gateway/src/api_config.rs`
- Reload endpoint and access gate: `crates/zeroclaw-gateway/src/lib.rs`
- Gateway bearer auth helper: `crates/zeroclaw-gateway/src/api.rs`
- Generated reference pipeline: `xtask/src/cmd/mdbook/refs.rs`
