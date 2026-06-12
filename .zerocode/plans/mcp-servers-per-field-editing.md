# Plan: per-field editing for `mcp.servers` in the zerocode TUI

## Why
- Last commit (`docs(mcp): document HTTP and stdio server config schema`)
  documented every field of `[[mcp.servers]]` but the operator still has to
  hand-edit `config.toml` because neither the dashboard nor the TUI can edit
  per-field — the dashboard ships an ObjectArray JSON-blob editor and the
  TUI has no MCP server UI at all.
- Goal: zerocode TUI gets a first-class per-field editor for `mcp.servers`,
  reaching dashboard *intent* parity (not literal parity — the dashboard's
  JSON-blob editor is the thing being replaced upstream). Leave a TODO in
  the commit so the dashboard catches up later.

## Scoping decisions (recorded 2026-06-05)
Operator was offline at decision time. Defaults picked, reversible later:

1. **Narrow scope.** The new `Vec<T>` per-entry routing is gated behind
   a `#[natural_key = "name"]` field attribute on `McpServerConfig`
   only. Other `Vec<T> + #[nested]` schema types (`ClassificationRule`,
   `EmbeddingRouteConfig`, `PeripheralBoardConfig`, …) keep their current
   no-per-entry-routing behaviour. This keeps the commit MCP-shaped and
   reviewable, and leaves the dashboard's existing `ObjectArrayEditor`
   working unchanged for those types.
2. **`name` is read-only after creation.** Per-prop `set_prop` on
   `mcp.servers.<name>.name` returns an error pointing the caller at
   `rename_map_key`. The TUI hides the `name` field from the editor and
   surfaces a `[r] Rename` keybind on the alias list that calls
   `config_map_key_rename`. Avoids stale-path bugs from mutating the
   routing key in-place.
3. **Ambiguous duplicate names surface as errors.** If the live Vec
   contains two entries with the same `name`, `set_prop` /
   `get_prop` / `prop_fields` for that key return / emit a
   `"mcp.servers.<name> is ambiguous: <n> entries share this name; fix
   the duplicate before editing"` error. `validate_mcp_config` already
   catches this at save time; we run it eagerly on `config_map_keys`
   for `mcp.servers` so the TUI's section view shows the banner before
   the operator clicks in.

## Strategy
The schema's own test
(`mcp_servers_addable_via_create_map_key_and_per_entry_props`) explicitly
defers per-entry path routing through `Vec<T>` to "future work." That
future work is the prerequisite for per-field editing on either surface,
and it is the bulk of this change. Once the runtime can route
`set_prop("mcp.servers.<name>.<field>", value)` end-to-end, the TUI side
is a small section-shape addition that reuses the existing
`OneTierAliasMap` rendering path.

## Steps

### 1. Runtime: `Vec<T> + #[nested]` per-entry path routing
File: `crates/zeroclaw-config/src/helpers.rs`
- Add `route_vec_path<'a, K, I>(name, my_prefix, field_name, inner_prefix, natural_keys)`
  mirroring `route_hashmap_path`. Yields `(matched_natural_key, inner_name)`
  or `None`. Longest-match on the natural-key string against `rest`.
- Add unit tests covering: prefix mismatch, multi-segment inner suffix,
  ambiguous natural-key collision, missing element.

File: `crates/zeroclaw-macros/src/lib.rs`
- In the `Vec<T> + #[nested]` branch (the `extract_vec_inner` arm around
  line 1179), add pushes to:
  - `nested_prop_fields` — enumerate `self.<field>.iter()`, derive each
    element's natural key from `<T as NaturalKey>::natural_key(elem)`,
    and prefix child paths as `<my_prefix>.<field>.<natural_key>.<leaf>`.
  - `nested_get_prop` / `nested_set_prop` — call `route_vec_path` with
    natural keys from `self.<field>.iter().map(|e| <T as NaturalKey>::natural_key(e))`,
    forward to the matched element's `get_prop` / `set_prop`.
  - `nested_prop_is_secret` — static-side: iterate every dot-split of
    `rest` and ask `<T as Configurable>::prop_is_secret(<inner>.<suffix>)`,
    same shape as the HashMap arm.
  - `get_map_keys_arms` — return `self.<field>.iter().map(<T as NaturalKey>::natural_key)`.
  - `delete_map_key_arms` — remove first element whose natural key matches
    `map_key`. Return `false` when absent.
  - `rename_map_key_arms` — find element by old key, set new key via
    `inner.set_prop("name", new_key)` (or `"hint"` fallback) after a
    duplicate check.
- Add a `NaturalKey` trait in `zeroclaw-config/src/traits.rs` with one
  method `fn natural_key(&self) -> &str`. Implement it for `McpServerConfig`
  (returns `&self.name`), `ClassificationRule` (returns `&self.hint`), and
  every other `Vec<T> + #[nested]` value type the schema currently exposes.
  - List of impls to add is derived by grepping for
    `impl HasPropKind for Vec<crate::schema::*>`: `ClassificationRule`,
    `EmbeddingRouteConfig`, `GoogleWorkspaceAllowedOperation`,
    `McpServerConfig`, `ModelRouteConfig`, `NevisRoleMappingConfig`,
    `PeripheralBoardConfig`, `ToolFilterGroup`. Each gets a one-line
    impl naming whichever string field is its de-facto identifier;
    pick the field the existing macro seeds (`name`, fallback to `hint`).
  - Why a trait rather than hardcoding `name`: `ClassificationRule` uses
    `hint`, not `name`, and we want the choice in *one* place per type
    rather than diffused across macro fallbacks.

### 2. Runtime: section table
File: `crates/zeroclaw-config/src/sections.rs`
- Insert a new variant `McpServers` between `Mcp` and `McpBundles`:
  ```
  McpServers => {
      key:   "mcp.servers",
      shape: OneTierAliasMap,
      help:  "Individual Model Context Protocol servers. Each entry binds \
              a transport (stdio, http, sse), the command or URL to reach \
              it, and a `tool_timeout_secs` cap (≤ 600). Servers added \
              here are addressable as `mcp.servers.<name>` and can be \
              grouped into bundles under `mcp_bundles` below.",
  }
  ```
- `OneTierAliasMap` is reused because the on-the-wire surface
  (`config/map_keys`, `config/map_key_create`, `config/map_key_delete`,
  per-prop `set_prop`) is now identical to a `HashMap<String, T>` section.

### 3. RPC: shape advertisement
File: `crates/zeroclaw-runtime/src/rpc/dispatch.rs` (or wherever
`handle_config_sections` lives)
- Verify `mcp.servers` surfaces in the sections list with
  `shape: one_tier_alias_map`. If the section enumerator is driven by
  the `sections!` macro this is automatic; otherwise add the variant.

### 4. TUI: no code change required
The `OneTierAliasMap` shape already calls `config_map_keys`,
`config_map_key_create`, `config_map_key_delete`, and `load_fields(prefix)`
for the field editor. After step 1 + 2, `mcp.servers` traverses the same
code path as `risk_profiles` / `runtime_profiles` / etc. with zero TUI
changes.

### 5. Docs + TODO
- `docs/book/src/tools/mcp.md`: add a one-line note pointing readers
  at the new TUI section.
- Commit message body: include a `TODO(web/dashboard)` block calling
  out that `web/src/components/sections/FieldForm.tsx`'s
  `ObjectArrayEditor` for `mcp.servers` should be replaced with the
  same per-prop GET/PUT surface the TUI now uses, and that the same
  treatment applies to the other `Vec<T> + #[nested]` sections
  (`peripheral.boards`, etc.).

## Verification
1. `cargo build -p zeroclaw-config -p zeroclaw-macros -p zeroclaw-runtime -p zerocode`
2. `cargo test -p zeroclaw-config --test '*' mcp_servers`
3. Unit tests to add:
   - `route_vec_path` — prefix match, longest match, miss.
   - `Config::set_prop("mcp.servers.fs.url", "http://x")` after
     `create_map_key("mcp.servers", "fs")` actually mutates the entry.
   - `Config::get_prop("mcp.servers.fs.transport")` round-trips.
   - `Config::prop_fields()` includes `mcp.servers.fs.url` after
     entry creation.
   - `Section::McpServers.shape() == OneTierAliasMap`.
4. Manual TUI walk: `zerocode` → Config → zeroclaw → mcp.servers →
   `+ Add` → enter `fs` → editor opens → set `command` → save →
   re-enter, observe value persisted.

## Out of scope (filed as follow-up)
- Dashboard migration to the per-field surface (TODO in commit body).
- Generalizing the same Vec routing to non-`name`-keyed types beyond
  the natural-key trait impls listed in step 1.
- Validation surfacing in the TUI (e.g. `tool_timeout_secs ≤ 600` —
  the runtime already rejects on save, error string flows back through
  the existing `set_prop` error handler).
