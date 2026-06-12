## Summary

- **Base branch:** `master`
- **What changed and why:**
  - Previously only `config.toml` could edit `[[mcp.servers]]` per-field. The web dashboard rendered it as a JSON-array blob (`ObjectArrayEditor`), and the zerocode TUI had no MCP UI at all — operators were forced into hand-editing TOML.
  - This PR ships the runtime capability that makes per-field editing work for `Vec<T> + #[nested]` list sections, opts `mcp.servers` into it, and surfaces the section in the zerocode TUI through the existing `OneTierAliasMap` rendering path (zero TUI-side code changes — the TUI already drives that shape for `risk_profiles`, `cron`, etc.).
  - Operators can now `+ Add` a server (seeded `name`), edit `transport` / `command` / `url` / `headers` / `env` / `tool_timeout_secs` as individual fields, and delete by name — all from the zerocode TUI, hitting the same RPCs the dashboard will eventually migrate onto.
  - Continues the line of work started by `docs(mcp): document HTTP and stdio server config schema` (`445de193e`) on this branch: the docs commit told operators which fields existed; this PR lets them set those fields without leaving the TUI.
- **Scope boundary:**
  - **No web dashboard changes.** The dashboard's `ObjectArrayEditor` still drives `mcp.servers`; migration sketch is filed in `docs/dashboard-mcp-per-field-todo.md` for separate follow-up.
  - **No rename UI in the TUI.** The runtime now supports `config_map_key_rename` for `mcp.servers`, but zerocode does not expose a rename keybind for any `OneTierAliasMap` section (matching every existing one). Adding it is a general TUI enhancement out of scope.
  - **Only `mcp.servers` opts into `#[natural_key]`.** The other seven `Vec<T> + #[nested]` schema fields (`ClassificationRule`, `EmbeddingRouteConfig`, `GoogleWorkspaceAllowedOperation`, `ModelRouteConfig`, `NevisRoleMappingConfig`, `PeripheralBoardConfig`, `ToolFilterGroup`) keep their existing `ObjectArray` JSON-blob behaviour — gated explicitly via the new attribute.
- **Blast radius:**
  - `zeroclaw-macros` derive emits new code for the `#[natural_key]` arm; **the arm is dead code on every existing struct** because no other field carries the attribute. Verified by 730 pre-existing config tests + 1984 runtime tests + 201 gateway tests + 6 macros tests + 14 zerocode tests — all green, zero regressions.
  - One genuine behavioural change touches all `Configurable` derives: nested `set_prop` / `get_prop` and the map-key recurse arms now propagate inner errors instead of silently swallowing them. This is justified separately in `fix(macros): propagate inner errors through nested-prop & map-key arms` and surfaces previously-hidden actionable errors (e.g. a typo in a property name now returns `Unknown property` pointing at the right path instead of being eaten by a wrong-field fallthrough). No existing test relied on the swallowed-error behaviour.
  - `handle_config_sections` refines its parent-collapse rule (`fix(rpc): keep parent sections with direct scalars beside dotted children`) so `mcp` does not disappear when `mcp.servers` is added to the curated section list. `providers` still vanishes correctly because it has no direct scalar fields.
- **Linked issue(s):** None.
- **Labels:** Snapshot after auto-labels apply.

## Validation Evidence (required)

- **Commands run and tail output:**

`cargo fmt --all -- --check`:

```
(exit 0, no output — tree is clean)
```

`cargo clippy -p zeroclaw-config -p zeroclaw-runtime -p zeroclaw-macros -p zeroclaw-gateway -p zerocode --all-targets -- -D warnings`:

```
    Checking zeroclaw-runtime v0.8.0-beta-2 (crates/zeroclaw-runtime)
    Checking zeroclaw-hardware v0.8.0-beta-2 (crates/zeroclaw-hardware)
    Checking zeroclaw-channels v0.8.0-beta-2 (crates/zeroclaw-channels)
    Checking zeroclaw-gateway v0.8.0-beta-2 (crates/zeroclaw-gateway)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 26.12s
```

(Initial clippy run flagged my macro arm's use of `anyhow::anyhow!` as a disallowed macro per the workspace lint config; fixed by switching to `anyhow::Error::msg(format!(...))` and fixed-up into the originating commit before the rebase. The above is from the final post-fix run.)

`cargo test -p zeroclaw-config -p zeroclaw-runtime -p zeroclaw-macros -p zeroclaw-gateway -p zerocode --lib` (tails of each crate):

```
test result: ok. 734 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s   (zeroclaw-config)
test result: ok. 201 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.53s   (zeroclaw-gateway)
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s     (zeroclaw-macros)
test result: ok. 1984 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 5.17s  (zeroclaw-runtime)
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s    (zerocode)
```

Totals: **2,939 passed, 0 failed, 1 ignored** (the ignored case is pre-existing and unrelated).

- **Beyond CI — what did you manually verify?**
  - End-to-end round-trip on `Config`: `create_map_key("mcp.servers", "fs")` then `set_prop("mcp.servers.fs.command", ...)` then `set_prop("mcp.servers.fs.transport", "http")` then `get_prop("mcp.servers.fs.command")` round-trips correctly. Covered by `mcp_servers_addable_via_create_map_key_and_per_entry_props` (replaces the prior "future work" stub).
  - Read-only `name` field: `set_prop("mcp.servers.fs.name", ...)` returns an explicit error pointing at `config_map_key_rename`. Covered by the same test.
  - Rename: `rename_map_key("mcp.servers", "fs", "filesystem")` mutates in place; new key resolves, old key stops resolving.
  - Ambiguity: two entries with the same `name` cause `set_prop` / `get_prop` / `rename_map_key` to return an actionable error without mutating state. Covered by `mcp_servers_routing_is_ambiguous_on_duplicate_names`.
  - Rename collision: `rename_map_key` refuses with `natural key already exists` when the target name is taken. Covered by `mcp_servers_rename_refuses_when_new_key_is_taken`.
  - `prop_fields()` hides the `name` field (read-only) but surfaces `mcp.servers.<name>.command` and friends. Covered by the same test.
  - `Section::McpServers.shape() == OneTierAliasMap`, parent `Section::Mcp` still `DirectForm`, canonical order `Mcp -> McpServers -> McpBundles`. Covered by `mcp_servers_section_has_alias_map_shape_and_parent_keeps_direct_form`.
  - **Not manually verified: an interactive TUI smoke walk** — I cannot run a TTY from this environment. The code path the TUI takes (`config_sections` returns `mcp.servers` with shape `one_tier_alias_map` -> `enter_section` dispatches `OneTierAliasMap` -> `load_aliases` calls `config_map_keys` -> `enter_alias` calls `load_fields` -> per-field `set_prop`) is identical to the path the other `OneTierAliasMap` sections drive successfully today, and every link in that chain is covered by an automated test in this PR.

- **If any command was intentionally skipped, why:**
  - `cargo test --workspace` (full-workspace test): **skipped — environment-blocked.** The Tauri-using desktop crate transitively requires `libdbus-1` system headers, which are not installed on this host; build fails inside `libdbus-sys`'s build.rs before reaching any test. Failure is unrelated to this PR (reproduces on master without these changes) and CI runs in a container that has the system deps. The five crates this PR actually touches all run cleanly per above.

## Security & Privacy Impact (required)

- New permissions, capabilities, or file system access scope? `No`
- New external network calls? `No`
- Secrets / tokens / credentials handling changed? `No` — `McpServerConfig::headers` was already `#[secret]` and the natural-key arm forwards through the existing per-element `set_secret` / `prop_is_secret` walk. The bias-toward-over-marking in the new `prop_is_secret` arm is the same shape as the HashMap arm.
- PII, real identities, or personal data in diff, tests, fixtures, or docs? `No`

## Compatibility (required)

- Backward compatible? `Yes`. The on-disk and on-the-wire shape of `[mcp]` and `[[mcp.servers]]` is unchanged — same TOML, same RPC method names, same JSON-Schema. Existing `config.toml` files load unchanged; the dashboard's JSON-array editor continues to work because `set_prop("mcp.servers", "<json>")` still routes through the legacy `ObjectArray` path on the JSON-array shape.
- Config / env / CLI surface changed? `No on existing surfaces; Yes on additions only`:
  - **Added** `Section::McpServers` curated section (URL key `mcp.servers`, kebab `mcp-servers`). Dashboards / CLIs that enumerate `QUICKSTART_SECTIONS` will see one new entry; nothing existing changes shape.
  - **Added** `#[natural_key]` field attribute on the `Configurable` derive. Existing structs that do not use it get unchanged emitted code.
- If `No` or `Yes` to either: exact upgrade steps for existing users: **None.** No-op upgrade.

## Rollback (required for `risk: medium` and `risk: high`)

Auto-label will likely tag this `risk: high` because it touches `crates/zeroclaw-runtime` and `crates/zeroclaw-gateway`. Filling the rollback section explicitly.

- **Fast rollback command/path:** `git revert 2ba5bab05 fb31c4604 d6b3d814a 2fb5055b6 e0eab48ca bf5a04763 6a62e06d6` (the seven commits in this PR, in reverse order). The previous commit `docs(mcp): document HTTP and stdio server config schema` (`445de193e`) is unaffected and stays.
- **Feature flags or config toggles:** `None`. The behaviour is gated structurally: existing `Vec<T> + #[nested]` fields without `#[natural_key]` keep their legacy behaviour, so the only field affected by a revert is `McpConfig::servers`. After revert, `set_prop("mcp.servers.<name>.<field>", ...)` stops working but the dashboard's JSON-blob editor and hand-editing `config.toml` continue to function unchanged.
- **Observable failure symptoms:**
  - Grep logs for `Unknown property 'mcp.servers.` — if this appears at runtime for a path the TUI is editing, the natural-key arm is not routing correctly.
  - Grep for `is ambiguous in 'mcp.servers'` — expected only when the operator has saved a duplicate-`name` config; if it fires under other conditions, the duplicate detection is misclassifying.
  - Grep for `no map-keyed/list section at 'mcp.servers'` — should never appear post-merge unless `Section::McpServers` was removed without also reverting the natural-key arm.
  - Metric: none added.
  - Alert: none added.
