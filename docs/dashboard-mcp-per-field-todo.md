# TODO: migrate `mcp.servers` editor to the per-field surface

## Context

`web/src/components/sections/FieldForm.tsx` renders `mcp.servers`
through `ObjectArrayEditor` — a JSON-array editor that round-trips
the whole `Vec<McpServerConfig>` through `set_prop("mcp.servers",
"<json-array>")`. The editor was a bridge built before the runtime
supported per-element property addressing on `Vec<T> + #[nested]`
list sections (see commits adding `route_vec_path`, the macro's
`#[natural_key]` arm, and `Section::McpServers`).

That bridge is no longer necessary for `mcp.servers`. The runtime
now exposes the same per-element surface that
`HashMap<String, T>` sections have always had:

- `GET /api/config?prefix=mcp.servers.<name>` returns per-field
  entries for that server.
- `PUT /api/config/<path>` with `path = mcp.servers.<name>.<field>`
  edits a single field.
- `POST /api/config/map-key?path=mcp.servers&key=<name>` creates
  a new entry (already used by the existing editor for the
  whole-array path).
- `DELETE /api/config/map-key?path=mcp.servers&key=<name>` removes
  by natural key.
- `POST /api/config/map-key/rename?path=mcp.servers&key=<old>&new_key=<new>`
  renames the natural key in place, with validation.

The zerocode TUI uses exactly this surface today (it dispatches
`mcp.servers` through the same `OneTierAliasMap` rendering as
`risk_profiles`, `cron`, etc.). The dashboard should do the
same.

## Migration sketch

1. In `web/src/components/sections/FieldForm.tsx`, detect that
   `mcp.servers` (or any `ListSection` whose value type now opts
   into `#[natural_key]`) should be rendered through the
   existing per-alias path, not `ObjectArrayEditor`. The runtime
   reports the section's shape in `/api/config/sections`
   (`shape: "one_tier_alias_map"`); the dashboard already
   special-cases that shape for the other sections.
2. Surface the new error strings as toast/banner messages:
   - "natural key `…` is ambiguous in `mcp.servers`: N entries
     share it; fix duplicates first" — the runtime emits this
     for `get_prop` / `set_prop` / `rename_map_key` against a
     duplicated `name`.
   - "`mcp.servers.name` is the natural key for `mcp.servers`
     entries and is read-only; use `config_map_key_rename` to
     change it" — surfaces if any code path still tries to PUT
     to `mcp.servers.<name>.name` directly.
3. Drop the `ObjectArrayEditor` code path for `mcp.servers`
   only (other `Vec<T>` schema fields — `peripheral.boards`,
   `classification`, etc. — have not opted into `#[natural_key]`
   yet and still need the JSON-array editor).

## Wider follow-up

Each of the other `Vec<T> + #[nested]` schema fields can opt into
the same per-field surface by adding `#[natural_key = "<field>"]`
at the field declaration site:

- `Vec<ClassificationRule>` — natural key is `hint`.
- `Vec<EmbeddingRouteConfig>` — natural key is `name`.
- `Vec<GoogleWorkspaceAllowedOperation>` — natural key is `name`.
- `Vec<ModelRouteConfig>` — natural key is `name`.
- `Vec<NevisRoleMappingConfig>` — natural key is `name`.
- `Vec<PeripheralBoardConfig>` — natural key is `name`.
- `Vec<ToolFilterGroup>` — natural key is `name`.

Adding the attribute is one line per type; the dashboard then
needs the same shape-aware dispatch from step 1 above. Coordinate
the schema opt-ins with whoever maintains the dashboard so the
JSON-array fallback isn't deleted before the per-field path is
wired up for those sections.
