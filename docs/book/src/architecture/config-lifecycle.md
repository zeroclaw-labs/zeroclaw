# Config lifecycle

Configuration is both an operator interface and a runtime contract. Treat it as
state with a clear owner, not as loose settings copied into whichever subsystem
needs them.

The canonical source is `zeroclaw_config::schema::Config`, loaded from
`config.toml`. User-facing config surfaces, the generated config reference, the
gateway config editor, env-var overrides, `zeroclaw config set`,
`zeroclaw config patch`, Quickstart, and RPC config methods all route through
that same typed schema.

For the build order, tracked-output rules, and drift checks that turn the typed schema into the config reference, see [Generated documentation pipeline](./generated-documentation-pipeline.md).

## What owns what

| Surface | Owner | Persistence boundary | Runtime apply boundary |
| --- | --- | --- | --- |
| Config schema | `crates/zeroclaw-config/src/schema.rs` plus `Configurable` derives | Code, not generated docs | New binary build |
| Generated reference | `cargo mdbook refs` / `markdown-schema` | `docs/book/src/reference/config.md` at build time | Documentation only |
| Bootstrap location | `ZEROCLAW_CONFIG_DIR`, `ZEROCLAW_DATA_DIR`, deprecated `ZEROCLAW_WORKSPACE` | Environment only | Before `Config` exists |
| Schema-mirror overrides | `ZEROCLAW_<lowercase_path>` with `__` for dots | In-memory only | Each `Config::load_or_init()` |
| Disk-only CLI config writes | Config and alias writes outside daemon-routed agent-alias paths, including authorization sections | `save_dirty()` to `config.toml` | Next load/reload unless the current command uses the new in-memory value |
| Daemon-routed CLI agent mutations | With `agent-runtime`, agent-targeting `config set` / `config init` and agent-alias create, rename, and delete | Daemon RPC (`config/*` and `agents/delete*`) while running; guarded disk writes when offline | The daemon coordinates the mutation; agent-targeting `config patch` refuses while the daemon owns config |
| RPC and TUI config writes | `config/*` RPC methods used by zerocode | Admitted config commit: `save_dirty()` then publish | Published config pair and accepted authorization policy update before success; daemon-owned subsystems that need rebuilding still need reload |
| Quickstart apply | Shared web, CLI, and zerocode apply path | Staged apply completed as a config commit (supervised surfaces) | Web and RPC can signal daemon reload; standalone CLI applies on next load/reload |
| Gateway config writes | Config API handlers through `persist_and_swap()` | Admitted config commit: `save_dirty()` then publish | Published config pair and accepted authorization policy update before success; shared with RPC in a supervised run; other daemon subsystems apply after reload |
| Daemon reload | `/admin/reload`, RPC `config/reload`, or the in-process reload channel | Re-reads `config.toml` | Recreates daemon subsystems in the same PID |

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

Do not add ad-hoc `std::env::var("PROVIDER_API_KEY")` reads inside provider, channel, tool, transcription, TTS, memory, or gateway constructors. That creates a second credential source outside `Config`, bypasses env-override visibility, and can make CLI, gateway, RPC/TUI, quickstart, and reload behavior disagree.

If ZeroClaw intentionally supports a native environment bridge for an integration family, document that bridge at the integration boundary and map it into the same typed config value before construction. Otherwise, ecosystem-default shell names such as `ANTHROPIC_API_KEY`, `OPENROUTER_API_KEY`, or `QDRANT_URL` should be bridged by operators into the corresponding `ZEROCLAW_*` schema-mirror variable; see [Environment variables](../reference/env-vars.md#bridging-ecosystem-default-env-vars).

## Dirty paths and incremental writes

Most editing surfaces use `Config::mark_dirty()` plus `save_dirty()`, not a
full rewrite. `save_dirty()` writes only changed dotted paths, preserves
non-dirty entries and comments where possible, stamps the current
`schema_version`, and writes through an atomic temp-file replacement.

That path is also responsible for map-key sections. Creating an alias such as a
model provider, MCP server, skill bundle, or knowledge bundle must dirty the
right section so the alias survives a save and reload. A config edit that only
updates the in-memory dashboard state is not complete.

When reviewing a config write, check that:

- the edited path is marked dirty before persistence;
- map-key creates, renames, and deletes dirty the parent section or natural key;
- secret and env-overridden paths keep their save masking behavior;
- `schema_version` remains current after incremental writes;
- the changed value survives `save_dirty()` followed by reload.

## Full saves need load provenance

`Config::save()` refuses to overwrite an existing `config.toml` unless the in-memory value was populated from that exact file (`loaded_from`, set by `load_or_init` in both its existing-file and fresh-init branches, and by the loaders that re-read a config file before mutating it). A default-constructed, programmatically built, or repointed `Config` therefore cannot replace an operator's populated config with a near-empty snapshot.

Creating a missing file (first run) still succeeds: a value that never read a file gets exactly one create and must then reload or force, and `force_save()` is the explicit path for a verified intentional overwrite. When reviewing a code path that builds a fresh `Config` and expects to write it over an existing file, either load first or justify the `force_save()` call.

`force_save()` skips only the load-provenance check. Destination-path checks still apply. To replace an existing file, set `config_path` to the intended path with a nonempty parent directory. A bare filename such as `config.toml` is refused if its runtime-resolved destination already exists, preserving the existing safeguard against overwriting a file at an inferred destination.

## Saved vs applied

A successful save means the file changed. It does not always mean every runtime
component has adopted the change.

The daemon owns the long-lived subsystem graph: gateway, channel listeners,
scheduler, MQTT listener, session wiring, memory backend, provider factories,
and cost wiring. `POST /admin/reload` signals the daemon loop, which re-reads
`config.toml` and re-instantiates those subsystems in the same process. The PID
stays the same, but listeners briefly rebind.

Gateway config writes call `persist_and_swap()`: validate the staged authorization
policy, save to disk inside an admitted, serialized config commit, publish that
policy, publish the saved config and its revision as the new pair, and set
`pending_reload`. In a supervised run, the gateway and RPC context share the
daemon generation's config, accepted authorization authority, and process-wide
config write lock. A policy edit through either surface therefore reaches both
before the write returns success, without a daemon reload. See
[Authentication & principals](../security/authentication.md#the-model-in-one-pass)
for connection revalidation and the separate pairing-token revocation boundary.

The reload banner still tells the operator that channels, providers, scheduler,
or other daemon-owned components may be running from the previous subsystem
instance. Direct file edits, and CLI writes outside daemon-routed agent-alias
paths (including authorization sections), do not use that in-process publication
path: they take effect at the next load, reload, or restart. These disk-only CLI
writes do not trigger a daemon reload after saving. With `agent-runtime`, supported
agent-alias mutations use the running daemon's RPC; agent-targeting
`config patch` refuses while the daemon owns config.

Standalone `zeroclaw gateway start` has no daemon supervisor. Its reload
endpoint returns a restart-required response because there is no outer daemon
loop to signal.

## Published pair and writer serialization

The supervised process keeps one canonical published pair: the `Config` together with its revision, an opaque authority epoch plus a checked sequence. Readers receive a read-only live handle (`zeroclaw_config::live::LiveConfigHandle`) and observe the config and its revision as one unit; that handle neither exposes mutation nor maintains a second config copy.

Every participating HTTP, RPC, TUI, Quickstart, pairing, and channel-identity writer admits through the daemon generation's `LiveConfigAuthority` (`begin_config_commit`). The commit acquires the process-wide config writer mutex once, then retains a generation config-write lease through publication. Waiting on the mutex does not hold a config-write lease. A separate agent-alias reservation, such as RPC Quickstart's reservation before admission, can still count toward generation drain. The irreversible phase (persist, then publish under a revision allocated before any disk I/O) runs retained, so a cancelled request cannot abandon a dispatched commit between the atomic file replacement and its publication. Commits fail closed once the generation is closing; a full reload starts a fresh authority with a fresh epoch (sequences compare only within one epoch).

The derived accepted authorization policy has its own publication and revision counter, separate from the config pair. HTTP config writes publish that policy before the config pair; RPC config writes publish it afterward. These existing orders do not provide one atomic visibility boundary for both publications, and the policy counter is not a `ConfigRevision` sequence.

Other boundaries worth naming:

- A committed publication is not rolled back when a later side effect fails. Quickstart publishes the committed config even when installing personality files subsequently fails. The CLI reports the saved alias and directs workspace-file recovery instead of rerunning Quickstart. Web and RPC still return the same error outcome used for refusal, without a distinct committed outcome, even though they signal reload after this partial success.
- The config migrate endpoint prepares a fully hydrated candidate (secrets decrypted, env overrides applied, strict validation) *before* replacing the file, so a candidate that cannot hydrate or validate is refused with the original file untouched.
- The `model_routing_config` and `proxy_config` tools still save independently loaded disk snapshots without config-commit admission or publication. These non-participating writers can lose concurrent edits or leave published readers stale. Migrating them into the participating writer contract remains separate work.
- This publication foundation does not itself apply config to running subsystems: daemon-owned components still apply after `/admin/reload`, and per-target apply results remain future work proposed in [ADR-012](./decisions/ADR-012-generation-scoped-live-config-apply.md).

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
persistence fails before publishing the new pair.

There is no general transactional rollback for a valid but undesired config
change after it has been saved and applied. Restore the previous `config.toml`
from backup, edit the field back through the CLI or dashboard, then reload or
restart according to the runtime boundary above.

## Config-visible is not always runtime-supported

A field can be schema-visible before every runtime path consumes it. That is
acceptable only when the docs and review notes say so plainly.

For example, `knowledge_bundles` is schema-visible and appears in config
section APIs. A PR that adds or changes such a surface must be precise about
whether it only stores configuration, wires the runtime behavior, or completes
both.

When reviewing a PR that touches a schema-visible but not yet runtime-consumed
field, require the PR description to say whether runtime wiring is deferred, out
of scope, or completed by the same change.

## Reviewer checklist

For config-schema, env-var, default, or reload changes, ask:

- What is the source of truth for the new value?
- Is this creating duplicate state, or resolving from `Config` at use time?
- Does the generated reference come from code rather than hand-maintained
  prose?
- Are env overrides load-time only and masked during saves?
- Do CLI, gateway, RPC/TUI, and quickstart surfaces agree on the dotted path?
- Are credentials resolved through typed config or documented schema-mirror bridges rather than ad-hoc provider-native env reads?
- Does a save survive process reload, not just immediate in-memory rendering?
- Does the PR say whether users need reload, restart, migration, or manual
  rollback?
- If the field is only config-visible, does the PR avoid claiming runtime
  support?

## Source pointers

- Config schema and persistence: `crates/zeroclaw-config/src/schema.rs`
- Published pair storage and read-only handle: `crates/zeroclaw-config/src/live.rs`
- Live-config authority, commits, and lifecycle: `crates/zeroclaw-runtime/src/live_config_authority.rs`
- Env override grammar: `crates/zeroclaw-config/src/env_overrides.rs`
- Config CLI commands: `src/main.rs`
- RPC and TUI config methods: `crates/zeroclaw-runtime/src/rpc/dispatch.rs`
- Shared Quickstart apply path: `crates/zeroclaw-runtime/src/quickstart/mod.rs`
- Web Quickstart reload signaling: `crates/zeroclaw-gateway/src/api_quickstart.rs`
- Gateway config API and reload banner: `crates/zeroclaw-gateway/src/api_config.rs`
- Reload endpoint and access gate: `crates/zeroclaw-gateway/src/lib.rs`
- Gateway bearer auth helper: `crates/zeroclaw-gateway/src/api.rs`
- Generated reference pipeline: `xtask/src/cmd/mdbook/refs.rs`
