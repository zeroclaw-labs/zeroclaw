# Cost tracking

ZeroClaw records every priced API call to an append-only ledger,
attributes spend to the originating agent, enforces daily / monthly
budgets, and surfaces the rollup on the dashboard `Cost` tab. The
pricing rules live in config so operators can edit them without a
rebuild.

This page describes the schema, the lookup pipeline, and the operator
surfaces. The code lives in `crates/zeroclaw-config/src/cost/` and
`crates/zeroclaw-runtime/src/agent/cost.rs`.

## Config schema

Two related sections own the surface:

```toml
[cost]
enabled            = true
daily_limit_usd    = 10.0
monthly_limit_usd  = 100.0
warn_at_percent    = 80
allow_override     = false
track_per_agent    = true

[cost.enforcement]
mode               = "warn"   # "warn" | "block" | "route_down"
route_down_model   = "claude-haiku-4-5"  # used with route_down
reserve_percent    = 10

[cost.rates.providers.models.anthropic."claude-opus-4-7"]
input_per_mtok        = 15.0
output_per_mtok       = 75.0
cached_input_per_mtok = 1.5

[cost.rates.providers.tts.openai."tts-1-hd"]
per_mchar = 30.0

[cost.rates.providers.transcription.openai.whisper-1]
per_minute = 0.006

[cost.rates.tools.web_search]
per_call = 0.005
```

`[cost]` covers budget enforcement and recording behavior. `[cost.rates.*]`
is the operator-managed rate sheet; every subsection's dotted path mirrors
the matching `[providers.*]` path with the trailing ``` `&lt;alias&gt;` ``lt;alias`` `&lt;alias&gt;` ``gt;` segment
replaced by the upstream resource being priced.

### Why the key is a resource id, not an alias

A `[providers.models.anthropic.&lt;alias&gt;]` entry is keyed by an operator-chosen
alias (`glados`, `production`) that follows the alias validator: lowercase
ASCII, single underscores, no hyphens. A `[cost.rates.providers.models.anthropic.&lt;resource&gt;]`
entry is keyed by the **upstream model id** as it appears in usage telemetry
(`claude-opus-4-7`, `gpt-4o-mini`, `whisper-1`) — those id strings come from
the provider's namespace and almost always contain hyphens.

The schema marks every rate-sheet HashMap with `#[resource_key]` (in
`crates/zeroclaw-macros/src/lib.rs`). That attribute opts the field out of
`validate_alias_key` in `create_map_key` / `rename_map_key`, so the
gateway's `POST /api/config/map-key` accepts hyphenated ids. Without it,
`create_map_key` rejects every realistic model id and the rate-sheet UI
falls flat. Aliases and resource ids share the on-disk structure
(`HashMap<String, T>`) but they're different naming systems with different
validators.

### Slot lists are the single source of truth

The per-provider-type slots under `[cost.rates.providers.models.&lt;type&gt;]`,
`[cost.rates.providers.tts.&lt;type&gt;]`, and `[cost.rates.providers.transcription.&lt;type&gt;]`
expand from the same macros that drive the `[providers.*]` slot wrappers:

```rust
// crates/zeroclaw-config/src/providers.rs
for_each_model_provider_slot!(emit_model_cost_rates_struct);
for_each_tts_provider_slot!(emit_tts_cost_rates_struct, super::schema::TtsCostRates);
for_each_transcription_provider_slot!(emit_transcription_cost_rates_struct, super::schema::TranscriptionCostRates);
```

Adding a new model provider type is one row in `for_each_model_provider_slot!`;
the rate-sheet slot, the provider config slot, and the dashboard dropdowns
all expand from it. No hand-typed dispatch tables, no parallel string lists
on the frontend.

## Pricing at request time

The pipeline from `[cost.rates.*]` to a recorded `cost_usd` value is:

1. **Orchestrator startup builds the pricing map.** When the channels
   supervisor instantiates a runtime context for an agent it walks
   `config.cost.rates.providers.models.iter_entries()` and merges the
   rates into a `HashMap<provider_type, HashMap<key, f64>>` where `key`
   is `"&lt;model_id&gt;.input"`, `"&lt;model_id&gt;.output"`, or
   `"&lt;model_id&gt;.cached_input"`. The legacy per-alias
   `[providers.models.&lt;type&gt;.&lt;alias&gt;].pricing` table is merged in too;
   `[cost.rates.*]` wins on conflict because it's the forward-looking
   surface.
   (See `crates/zeroclaw-channels/src/orchestrator/mod.rs` —
   the closure under `cost_tracking: CostTracker::get_or_init_global(...).map(|tracker| ...)`.)

2. **Recording inside the agent loop.** Every successful LLM response
   reaches `record_tool_loop_cost_usage(provider_name, model, usage)`
   in `crates/zeroclaw-runtime/src/agent/cost.rs`. The function pulls
   the pricing map slot for `provider_name`, calls `resolve_rates(map,
   model)`, multiplies by token counts, and stores a `CostRecord` via
   the global `CostTracker`.

3. **resolve_rates** tries the model id first, then the path-suffix
   form for `provider/model` strings (so `anthropic/claude-opus-4-7`
   degrades to `claude-opus-4-7` if the operator stored only the
   short form). Returns `(0.0, 0.0)` on miss and triggers a one-shot
   `missing_pricing` warn so silent zero-cost records show up in logs.

4. **CostTracker is a process-global singleton** (`OnceLock` in
   `crates/zeroclaw-config/src/cost/tracker.rs`). Its `CostConfig` is
   frozen at first init; if the operator flips `cost.enabled` after
   that, the daemon must restart for the tracker to honor the new
   value. The orchestrator's pricing map, in contrast, is rebuilt on
   every daemon reload from the live config — so rate edits take
   effect on the next request after reload.

## Persistence

`CostTracker::record_usage_with_agent` appends one `CostRecord` per
priced response to `&lt;workspace&gt;/state/costs.jsonl`, one JSON object
per line. The file is read on startup to seed `daily_records()` so
the dashboard's per-agent rollup survives restarts.

`cost_usd` is computed at record time from the rate sheet in effect
**at that moment**. Records are immutable — if the operator adds
rates after some requests have already been recorded, those existing
records keep `cost_usd = 0`. Only requests made after the rate is
configured (and the daemon reloaded so the orchestrator's pricing
map rebuilds) carry a non-zero cost.

This is the most common surprise after first enabling the rate sheet.
The fix is to wait for new requests; there's no retroactive repricing.

## Budget enforcement

`CostConfig::enforcement.mode` decides what happens when a projected
cost would push `daily_total` or `monthly_total` past the configured
limit:

- `warn` — the default; record the event with a warn-level log and
  let the request through.
- `block` — refuse the request with a `BudgetExceeded` error.
- `route_down` — substitute `route_down_model` (a cheaper
  alternative) for the original model. The substitution happens before
  the request is dispatched.

`allow_override = true` lets a request bypass `block` by passing an
override token on the CLI (`zeroclaw --override`). Defaults to
`false`. `warn_at_percent` controls when the gateway surfaces a
warning banner ahead of the hard limit; defaults to 80%.

## Per-agent attribution

When `cost.track_per_agent` is true (default) every recorded
`CostRecord` carries the originating agent alias. The dashboard's
**Spend by agent** panel and `GET /api/cost?agent=&lt;alias&gt;` consume
this field. Setting `track_per_agent = false` is an optimization for
high-volume installs where the extra HashMap aggregation shows up in
profiles; the trade-off is losing the per-agent dimension everywhere.

## Operator surfaces

### Config UI

- `/config/cost` → **Limits** tab: every flat `[cost].*` field
  (enabled, limits, enforcement, track_per_agent). Rate-sheet rows
  are not edited here — they're tied to the provider that owns the
  model, so they live one tier down.
- `/config/providers.&lt;category&gt;/&lt;type&gt;` → **Costs** tab: rate-sheet
  editor for that provider type. The `+ Add` input suggests upstream
  resource ids drawn from `providers.&lt;category&gt;.&lt;type&gt;.*.model`
  across configured aliases, so the operator can one-click a rate row
  for every model they've actually bound. This is the only entry
  point for editing `[cost.rates.providers.&lt;category&gt;.&lt;type&gt;.*]`.

### Dashboard

The dashboard's **Cost** tab shows three panels plus a Window picker (today / last 7 days / last 30 days / this month / all time):
(today / last 7 days / last 30 days / this month / all time):

- **Spend totals** — daily and monthly totals from `costs.jsonl`.
- **Spend by agent · `` `<window>` ``** — per-agent rollup over the picked
  window. Visible when `track_per_agent` is true.
- **Spend by model · `` `<window>` ``** — per-model rollup. Each row's model
  id is clickable; the click resolves the owning provider type from
  configured aliases and navigates to that provider's Costs tab. When
  the model id isn't bound to any configured provider the click is a
  no-op (there's no qualified rate-sheet route for an orphan model).

### Gateway

- `GET /api/cost` — current `CostSummary` (matches the dashboard's
  Cost overview shape). Add `?agent=&lt;alias&gt;` for a single-agent view.
- `GET /api/config/templates` — every map-keyed section the schema
  registers, used by the Rates tab's category × provider-type
  dropdowns.
- `POST /api/config/map-key?path=cost.rates.providers.&lt;category&gt;.&lt;type&gt;&key=&lt;resource&gt;`
  — create a new rate row. The path is rejected if no such map
  section exists; the resource key passes `#[resource_key]` instead
  of `validate_alias_key`.

## Troubleshooting

**Dashboard shows $0.0000 for all agents after configuring rates.**
Old records are immutable — they were recorded with `cost_usd = 0`
because no rate was set when they happened. Make a new chat request
after the daemon reload and check **Cost overview > Session** plus
**Spend by model**; both should populate for the new request.

**Drift detected against `cost.rates.*` paths after save.** A pre
v0.8.0 daemon mangled hyphenated HashMap keys in the dirty-save path,
silently dropping every write to the rate sheet. If you see this on
v0.8.0+ it's a real bug — the dirty-path resolution lives in
`crates/zeroclaw-config/src/schema.rs::apply_dirty_path`; file an
issue with the daemon version and the path that drifted.

**`missing_pricing` warns spam the log.** Emitted once per
`(provider_type, model)` pair when `resolve_rates` returns `(0.0,
0.0)`. Either the rate isn't configured for that model, or the
upstream returned a different model id than what's in the rate
sheet (some providers return versioned ids like
`claude-3-5-sonnet-20241022` even when you configured
`claude-3-5-sonnet`). Add the exact id the warn names, or set the
unversioned id and rely on `resolve_rates`'s suffix-match path.
