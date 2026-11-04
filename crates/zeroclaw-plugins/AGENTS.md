# zeroclaw-plugins

## What this crate is

WASM plugin host for ZeroClaw. Handles plugin discovery, manifest parsing,
Ed25519 signature verification, and the WIT / direct `wasmtime` execution
bridge. Bridges plugin-exported functions into ZeroClaw's `Tool` and
`Channel` traits so plugins appear as native capabilities to the agent runtime.

## What this crate is allowed to depend on

- `zeroclaw-api` (traits only — `Tool`, `Channel`, `ToolResult`)
- `wasmtime` (Component Model host)
- `wasmtime-wasi` and `wasmtime-wasi-http` (WASI Preview 2 host support)
- `ring` (Ed25519 signatures)
- `cap-std` (directory handles that install, recovery and admission work through)
- `same-file` (file identity for the recovery checks, and for the non-Unix payload checks)
- `windows`, `winx` (Windows only: handle paths, and package moves that never replace an existing entry)
- `rustix` (Unix only: handle-relative, no-follow opens when admitting a component, and package moves that refuse an existing destination where the platform can)
- `serde`, `serde_json`, `toml`, `toml_edit` (serialization)
- `jsonschema` (manifest-owned plugin config-contract validation; network resolvers disabled)
- `tokio` (async bridging via `spawn_blocking`)
- `tracing` (logging)
- `anyhow`, `thiserror` (error handling)

Do not add dependencies on specific tools, channels, providers, or config
schemas. This crate knows how to run plugins, not what they do.

## Extension points

- **New WIT interfaces:** Add or revise the versioned interface under
  `wit/v0/`, then update the matching component bridge. Treat WIT edits as
  compatibility-sensitive architecture changes.
- **New host imports:** Add them through `component.rs` or the per-world
  linker for the relevant bridge, not as ad hoc Extism-style JSON host
  functions. Gate the import on a `PluginPermission` variant (add to `lib.rs`
  if needed). HTTP support is wired through WASI HTTP when
  `PluginPermission::HttpClient` is granted.
- **New capability bridges:** Add alongside `wasm_tool.rs` and
  `wasm_channel.rs` (e.g., `wasm_memory.rs` for memory backend plugins).
- **New permissions:** Add variants to `PluginPermission` in `lib.rs`.

## What does NOT belong here

- Concrete tool or channel implementations (those go in `zeroclaw-tools` or
  `zeroclaw-channels`, or in a WASM plugin)
- Plugin business logic (that belongs in the plugin's own crate)
- Config schema definitions (those go in `zeroclaw-config`)
- Plugin registry client or distribution (future separate crate)

## Proposed #12216 registry placement clarification (inactive)

**PENDING Core Team interpretation and approval; INACTIVE.** This section
requests a narrow clarification of this crate's own contract. The runtime
exception process does not delegate authority to waive this contract. Core
has not selected continued placement or agreed the destination; it may instead
direct a named minimal relocation. Record the approving Core reviewer and
review permalink here before this separate proposal merges, and land the
approved record before #12216. The feature cannot grant itself permission.

The proposed carve-out is limited to the existing registry client utilities in
`src/registry.rs` and their focused in-file tests, plus the source-compatibility
checks in `tests/registry_literal_compat.rs`, for the selection, cache and
origin-preserving multi-registry behaviors added by #12216: parse the existing
registry selection as one URL or a bounded JSON URL list; retain each
publisher (including empty indexes) in the cached collection; require exact
source-set matches; preserve legacy single-index compatibility; and keep
search, resolution, origin labels and rendered install-command suggestions
bound to their publisher, refusing ambiguous cross-publisher resolution.
Cache work is limited to that existing metadata cache and its bounded write
integrity; it does not authorize a cache-cleanup project or new storage owner.

This is not a blanket registry/distribution waiver. The general exclusion above
remains in force outside this exact proposal. It grants no fetching,
installation, activation or execution authority, new subsystem, dependency on
runtime/config schemas, broader distribution ownership, or scope growth. Other
#12216 paths and implementation acceptance are not approved by this text.

Proposed future destination: the already-planned separate registry owner,
with the skills adapter following the planned skills extraction; Core must
confirm the actual destination and registry/skills split, not an author-named
new crate. Reconsider at the first relevant skills/registry extraction design
review, or before widening scope, selection/fallback policy, retained state or
authority. This is a review condition, not a calendar expiry.

The bounded request avoids forcing an unagreed cross-crate extraction into the
ordinary #12109 feature. It neither changes the roadmap, security model or RFC
process nor claims that continued placement has already been chosen.

## Related ADRs

- ADR-003: historical Extism plugin bootstrap, superseded by ADR-009
- ADR-009: WIT components and direct `wasmtime` plugin execution
