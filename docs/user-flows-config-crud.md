# Config CRUD User Flows — Channels & Model Providers

Source of truth for what "working" means. Every story below must complete without error, modal pop-up, data loss, or navigation dead-end.

---

## Data model

```
providers.models.<type>.<alias>   e.g. providers.models.anthropic.work
channels.<type>.<alias>           e.g. channels.discord.alerts
```

`<type>` is the canonical provider/channel name (e.g. `anthropic`, `discord`).  
`<alias>` is user-chosen. `"default"` is only a suggestion, not structurally special.  
One type can have many aliases. Aliases within a type must be unique.

---

## Macro-generated operations (single source of truth)

| Method | Signature | Effect |
|---|---|---|
| `get_map_keys(path)` | `&str → Option<Vec<String>>` | List aliases at path |
| `create_map_key(path, key)` | `&str, &str → Result<bool>` | Insert default entry; idempotent |
| `delete_map_key(path, key)` | `&str, &str → Result<bool>` | Remove alias |
| `rename_map_key(path, from, to)` | `&str, &str, &str → Result<bool>` | Rename alias; error if `to` exists |

Gateway endpoints wrap these thinly. CLI calls them directly on `Config`. No logic duplicated.

---

## Gateway endpoints

| Method | Path | Body / Query | Purpose |
|---|---|---|---|
| GET | `/api/config/map-keys?path=<p>` | — | List aliases |
| POST | `/api/config/map-key?path=<p>&key=<k>` | — | Create alias |
| DELETE | `/api/config/map-key?path=<p>&key=<k>` | — | Delete alias |
| POST | `/api/config/rename-map-key` | `{path, from, to}` | Rename alias |
| GET | `/api/config/list?prefix=<prefix>` | — | Read fields |
| PATCH | `/api/config` | `{path, value}` | Write field |

---

## User stories

### US-1 · Create first channel (no existing config)

**Web:**
1. Navigate to `/config/channels`
2. Section overview shows "Nothing configured yet. Click + Add."
3. Click **+ Add** → SectionPicker shows all channel types
4. Click **Discord**
5. URL becomes `/config/channels/discord` — alias list page
6. Alias list is empty; inline input pre-filled `default`
7. User types alias name (or keeps `default`), clicks **Add**
8. `POST /api/onboard/sections/channels/items/discord` with `{alias}` body
9. URL becomes `/config/channels/discord/default` — FieldForm renders
10. User fills fields, saves → persisted to disk

**CLI/TUI:**
1. `zeroclaw onboard channels` (or select Channels in onboard flow)
2. Channel type list shown; user picks **Discord**
3. Alias prompt: "Alias (name for this configuration)" pre-filled `default`
4. User confirms alias → fields prompted one by one
5. ESC at any field → alias removed from in-memory config (not saved), returns to type list

---

### US-2 · Create second alias for existing channel type

**Web:**
1. Navigate to `/config/channels/discord` (type already has `default`)
2. Alias list shows `default` as a row
3. Inline input pre-filled `default-2`; user types `alerts`, clicks **Add**
4. `POST /api/onboard/sections/channels/items/discord` with `{alias: "alerts"}`
5. URL becomes `/config/channels/discord/alerts` — FieldForm renders

**CLI/TUI:**
1. Select **Discord** (shows `[configured]` badge)
2. Alias picker shows: `default`, `+ Add new`
3. User picks `+ Add new`
4. Alias prompt pre-filled `default-2`; user types `alerts`
5. Fields prompted → saved

---

### US-3 · Edit existing alias

**Web:**
1. Navigate to `/config/channels` → overview shows Discord `[configured]`
2. Click **Discord** → `/config/channels/discord` — alias list: `default`, `alerts`
3. Click **default** → `POST /api/onboard/sections/channels/items/discord {alias:"default"}` (idempotent)
4. URL becomes `/config/channels/discord/default` — FieldForm with current values
5. User edits fields, saves

**CLI/TUI:**
1. Select **Discord** → alias picker: `default`, `alerts`, `+ Add new`
2. Select **default**
3. Fields prompted with current values shown as "(current: …)"
4. ESC at any field → in-memory state untouched (existing alias survives), returns to alias picker

---

### US-4 · Rename an alias

**Web:**
1. Navigate to `/config/channels/discord/default` (form view)
2. Rename input at top of form, pre-filled `default`
3. User types `primary`, presses Enter or clicks **Rename**
4. `POST /api/config/rename-map-key {path:"channels.discord", from:"default", to:"primary"}`
5. On success: URL updates to `/config/channels/discord/primary`; form remains open

**CLI/TUI:** *(not yet implemented — tracked as follow-up)*

---

### US-5 · Delete an alias

**Web:**
1. Navigate to `/config/channels/discord/default`
2. **Delete** button at bottom of form
3. Confirmation: "Delete alias 'default'?" — inline, not a modal — e.g. button changes to "Confirm delete"
4. `DELETE /api/config/map-key?path=channels.discord&key=default`
5. Redirect to `/config/channels/discord` (alias list)

**CLI/TUI:** *(not yet implemented — tracked as follow-up)*

---

### US-6 · Rename collision (error case)

**Web:**
1. At `/config/channels/discord/default`, rename to `alerts` (already exists)
2. `POST /api/config/rename-map-key` → 422 from gateway
3. Inline error shown below rename input: "Alias 'alerts' already exists"
4. URL unchanged; user corrects and retries

---

### US-7 · Create model provider (first alias)

Identical to US-1 with path `providers.models.<type>.<alias>` and section key `providers`.  
Alias-tier path: `/config/providers/anthropic` → `/config/providers/anthropic/work`

---

### US-8 · ESC / Back at each step (no data loss)

| Step | ESC/Back action | Expected result |
|---|---|---|
| Type list (Web SectionPicker) | Click Back | Return to section overview; nothing created |
| Alias list (Web) | Browser back / sidebar click | Return to section overview |
| Alias list (CLI) | ESC | Return to type list |
| New alias name input (CLI) | ESC | Return to alias list (type already had aliases) or type list (no aliases) |
| Field form — new alias (Web) | Browser back | Alias entry NOT persisted (no `create_map_key` was called before form opened) |
| Field form — existing alias (Web) | Browser back | Existing config unchanged |
| Field prompt — new alias (CLI) | ESC | Remove in-memory alias, return to alias list |
| Field prompt — existing alias (CLI) | ESC | Existing config unchanged, return to alias list |

---

## Known bugs to fix (pre-merge)

1. **`GET /api/config/map-keys` returns 400** — `MapKeyQuery` requires `key` field but this endpoint only needs `path`. Use a separate `MapPathQuery { path }` struct.
2. **Onboard.tsx still shows modal** for alias naming — replace with inline alias list + input (same pattern as Config.tsx `AliasListView`).
3. **Rename alias (US-4)** — not wired in FieldForm yet.
4. **Delete alias (US-5)** — not wired in FieldForm yet.
5. **CLI/TUI ESC on existing alias** — verify fix from last commit covers all field prompts, not just api-key and model.
