# Live model discovery + tier-based switching for zeroclaw

**Date:** 2026-04-23
**Status:** Draft
**Scope:** zeroclaw provider client + `model_switch` tool; optional small addition to cliproxy
**Targets:** `adi-zeroclaw-shane`, `adi-zeroclaw-meg` (both use `adi-cliproxy` as sole provider)

## Goal

Let the agent pick a model by semantic tier — `chat`, `thinking`, `fast` — instead of memorizing exact model IDs that drift as Anthropic ships new models. Make `adi-cliproxy` the single source of truth for both the available catalog and the tier → model mapping. Fix the silent-hang bug that masks upstream errors.

## Non-goals

- Multi-provider routing. Today cliproxy fronts a single Anthropic account; this design assumes that continues. If a second provider is added later, `/v1/models` is already provider-agnostic and the tier mapping is the only thing that needs extending.
- Per-model cost accounting. Cost tracking stays in zeroclaw's existing `cost.prices` config.
- Retiring the raw `set` action of `model_switch`. Power users / debugging still want `set provider=… model=…`. Tiers are additive.
- Fixing the `tool_use`/`tool_result` pairing bug visible in the midnight cliproxy error log. Different issue.

## Context

### Current state (what broke 2026-04-23)

Zeroclaw's `model_switch` tool (`crates/zeroclaw-runtime/src/tools/model_switch.rs:218-230`) contains a hardcoded list of model names per provider. The list was written when the file was authored and has not been updated since. It includes names like `claude-sonnet-4-5` that the upstream API no longer accepts as bare names (the real name today is `claude-sonnet-4-5-20250929`). When the agent called `set provider=anthropic model=claude-sonnet-4-5`, shane dutifully sent `{"model": "anthropic/claude-sonnet-4-5"}` to cliproxy, cliproxy returned HTTP 502 `unknown provider for model`, and zeroclaw's provider client **never surfaced the error** — the agent loop hung on an "in-flight" LLM call for 13+ hours until manually restarted.

Two root causes, same fix window:

1. **Stale catalog**: the agent is looking at a list of model names baked into a binary, not at what cliproxy actually accepts.
2. **Silent non-2xx**: the provider client treats an error response as "keep waiting," not "fail this call."

### Infrastructure already in place

- `adi-cliproxy` exposes `/v1/models` today, serving the real current Anthropic catalog (verified 2026-04-23: returns `claude-opus-4-7`, `claude-opus-4-6`, `claude-sonnet-4-6`, `claude-sonnet-4-5-20250929`, `claude-haiku-4-5-20251001`, and older dated variants). **No proxy changes are required to get a live model list.**
- Zeroclaw's agent loop has an existing global `Mutex<Option<(provider, model)>>` (`crates/zeroclaw-runtime/src/agent/loop_.rs:93`) consumed on every turn. The switch-apply mechanism is already wired; this design only changes how the desired model is *resolved*.
- Shane and meg both route exclusively through `custom:http://adi-cliproxy.internal:8317/v1` (confirmed in live config). Tier resolution runs on whatever client talks to cliproxy; no per-app divergence.

## Design

### Component overview

```
┌─────────────────────────────────────────┐
│  zeroclaw agent                         │
│                                         │
│  model_switch tool                      │
│  ┌─────────────────────────────┐        │
│  │ action: list_tiers   ◄──────┼──── agent asks "what tiers exist?"
│  │ action: set_tier     ◄──────┼──── agent says "switch to thinking"
│  │ action: list_models  ◄──────┼──── agent wants raw catalog
│  │ action: set          ◄──────┼──── raw provider/model (unchanged)
│  └──────────────┬──────────────┘        │
│                 ▼                       │
│  ModelCatalogClient (new)               │
│    • GET /v1/models   (60s cache)       │
│    • GET /v1/tiers    (60s cache)       │
└─────────────────────┬───────────────────┘
                      │ HTTP over tailnet
                      ▼
┌─────────────────────────────────────────┐
│  adi-cliproxy                           │
│    GET /v1/models  (already exists)     │
│    GET /v1/tiers   (new: small shim)    │
└─────────────────────────────────────────┘
```

### Tier definitions

Three tiers, exposed to the agent via `list_tiers` and settable via `set_tier`:

| Tier | Default mapping (on cliproxy) | Intended use |
|---|---|---|
| `chat` | `claude-sonnet-4-6` | Default. Channel replies, short Q&A, routine tool use. Also the fallback when no tier is selected. |
| `thinking` | `claude-opus-4-7` | Planning, multi-step reasoning, code review, spec writing, tricky debugging. Agent chooses this when it judges the task needs deeper reasoning. |
| `fast` | `claude-haiku-4-5-20251001` | Classification, yes/no, short summarization, triage decisions. Agent chooses this for cost-sensitive or latency-sensitive internal calls. |

The **agent** chooses tiers. Tier names go into its system prompt so the judgment is explicit, not tool-layer-hidden. The router / channel layer does not pre-classify.

### Cliproxy changes

One new endpoint: `GET /v1/tiers`. Response shape:

```json
{
  "tiers": [
    {"name": "chat",     "model": "claude-sonnet-4-6",            "description": "default conversational model"},
    {"name": "thinking", "model": "claude-opus-4-7",              "description": "deep reasoning, planning, code review"},
    {"name": "fast",     "model": "claude-haiku-4-5-20251001",    "description": "classification, triage, cheap calls"}
  ]
}
```

Source of truth: a `tiers.yaml` on the cliproxy volume (`/data/tiers.yaml`). Seeded by `deploy/cliproxy/entrypoint.sh` on first boot if not present. Manually editable after that — updating the `thinking` tier when `claude-opus-4-8` ships is one line + a `flyctl machine restart`.

Implementation options for the endpoint itself, pick **whichever is smaller once we check cli-proxy-api's source**:

- **Option a (preferred):** eceasy/cli-proxy-api supports a "static responses" config hook or a `custom-endpoints:` section → add it there, no new process.
- **Option b:** tiny Go/Rust sidecar bound to `127.0.0.1:8318` reading `/data/tiers.yaml`, and cliproxy is taught to proxy `/v1/tiers` to it — probably requires (a) anyway.
- **Option c:** drop the whole `/v1/tiers` endpoint and instead bake the tier mapping into cliproxy's existing `/v1/models` response as a non-standard `x-tier` field per model. More OpenAI-compat-friendly but burns a spec-extension; not preferred.

If (a) turns out unsupported by cli-proxy-api v6.9.31, fall back to (c). **This decision is deferred to the implementation plan** — it does not change the zeroclaw-side design.

### Zeroclaw changes

#### 1. New: `ModelCatalogClient`

New module, probably `crates/zeroclaw-providers/src/catalog.rs`:

```rust
pub struct ModelCatalogClient {
    base_url: String,      // e.g. "http://adi-cliproxy.internal:8317/v1"
    api_key: String,       // bearer token for cliproxy auth
    http: reqwest::Client,
    cache: Mutex<CachedCatalog>,
}

struct CachedCatalog {
    models: Option<(Vec<ModelEntry>, Instant)>,
    tiers: Option<(Vec<TierEntry>, Instant)>,
}

pub struct ModelEntry { pub id: String, pub owned_by: String }
pub struct TierEntry  { pub name: String, pub model: String, pub description: String }

impl ModelCatalogClient {
    pub async fn list_models(&self) -> anyhow::Result<Vec<ModelEntry>>;
    pub async fn list_tiers(&self) -> anyhow::Result<Vec<TierEntry>>;
    pub async fn resolve_tier(&self, tier: &str) -> anyhow::Result<String>;
}
```

- TTL: **60 seconds**. Balances "agent switches tier mid-task without re-fetching" against "new model becomes visible quickly."
- Failure policy: if the HTTP call fails, surface the error to the caller — **do not** fall back to a hardcoded list. The tool will report "unable to reach cliproxy catalog" and the agent can retry or use raw `set`.
- Construction: the zeroclaw provider layer builds one instance per process using the same `base_url` + `api_key` already in the config's `[providers.models."custom:…"]` block.

#### 2. `model_switch.rs` changes

Add two new actions to the tool:

- `list_tiers` → calls `catalog.list_tiers()` and returns the three-tier list with descriptions.
- `set_tier` → takes `{"tier": "thinking"}`, calls `catalog.resolve_tier(tier)`, then reuses the existing `handle_set` path with `provider = "custom:http://adi-cliproxy.internal:8317/v1"` and the resolved model. Error if the tier doesn't exist in the catalog.

Modify the existing `list_models` action:

- When `provider` is the cliproxy provider (or omitted), delegate to `catalog.list_models()` instead of returning the hardcoded static list.
- Keep the hardcoded list only as a fallback for non-cliproxy providers that the agent might talk to in future — or delete it entirely. **Recommended: delete it.** Leaving stale data behind for "other providers" invites the same rot, and there are no other providers today. Future multi-provider support can re-introduce discovery per-provider.

Modify `handle_set`:

- For the cliproxy provider, validate the model against `catalog.list_models()` before accepting the switch. If not found, return an error with the available list. This closes the phantom-model loop permanently — the agent literally cannot request a name that cliproxy will reject.

#### 3. Silent-hang fix

Separate from the catalog work but bundled per user decision. Location: `crates/zeroclaw-providers/src/compatible.rs` (the `custom:` provider implementation — verified via `grep` as the layer that talks to cliproxy).

Current behavior (inferred from symptom): non-2xx HTTP responses from cliproxy are either swallowed or retried without a deadline, yielding an apparent hang.

Required behavior:

- Non-2xx response → parse `{"error": {"message": …}}` if present → return a structured error to the agent loop that includes the HTTP status, the upstream error text, and the model that was requested.
- Request-level read timeout: **60 seconds**. Anthropic streaming responses can legitimately take this long; a hung cliproxy should not.
- Total request deadline: **300 seconds** (covers the longest Opus reasoning call we've observed). Past this, the call fails with a timeout error.
- Every request logs at minimum: provider URL, model, HTTP status, elapsed ms. So the "Starting LLM call → silence" pattern becomes "Starting LLM call → Failed LLM call (status=502, model=…, elapsed=…)" — debuggable at a glance.

### Agent-facing surface

After the change, the agent sees these tool actions:

| Action | Params | Behavior |
|---|---|---|
| `get` | — | Returns current active model + any pending switch. Unchanged. |
| `list_tiers` | — | **New.** Returns the three tiers with descriptions. |
| `set_tier` | `tier` | **New.** Resolves tier → model via cliproxy, stages the switch. |
| `list_models` | `provider?` | Returns live catalog from cliproxy (was: hardcoded). |
| `list_providers` | — | Returns static provider list. Unchanged. |
| `set` | `provider`, `model` | Validates model against live catalog before staging. Was: unvalidated. |

And its system prompt gets a short line:

> You can switch models by tier with `model_switch set_tier`. Use `chat` for routine replies (default), `thinking` for deep reasoning or planning, `fast` for cheap classification. Switch back to `chat` when done.

## Trade-offs and rationale

**Why tiers on cliproxy, not in zeroclaw config?** Because "which Opus is the current deep-reasoning model" is an operational fact that changes as Anthropic ships new models, and it's the same answer for shane and meg. Putting it in cliproxy's volume means one edit covers both personas without a zeroclaw rebuild. Putting it in each zeroclaw's config.toml would duplicate the truth.

**Why not query Anthropic's `/v1/models` directly from zeroclaw?** It would require leaking the Anthropic API key to shane and meg, which today they don't have — cliproxy owns the upstream credential. Going through cliproxy preserves the credential boundary.

**Why a 60-second cache?** Zero cache means every `list_models` / `set_tier` round-trips over the tailnet. An agent doing rapid tier switching in a single task would waste latency. Longer TTL means slower visibility for a newly-added model. 60s is the sweet spot: one HTTP call per minute per process is negligible, and a human editing `tiers.yaml` is happy to wait 60s.

**Why keep the raw `set` action?** Debugging. Occasionally you want to pin a specific dated model (`claude-sonnet-4-5-20250929`) for reproducibility. Tiers are the 95% path; `set` remains the 5% escape hatch.

**Why include the silent-hang fix in scope?** Because without it, Option A (live validation) catches one class of error but any *other* cliproxy failure (rate limits, upstream outage, auth errors, network blips) still presents to the user as "Adi just stopped responding." Fixing the visibility is what turns this from "one bug" into "class of bugs closed."

## Risks

**Cache staleness masks a real outage.** If cliproxy goes down between two successful fetches, the 60-second cache lets the agent believe the catalog is live. Acceptable: the agent still hits cliproxy to actually issue the LLM request, so an outage surfaces on the *call* itself, not on `list_models`.

**Tier change on cliproxy requires a restart to take effect.** `tiers.yaml` is read on startup if we go with option (a). Mitigation: the endpoint can re-read on each request — at 60s client cache, that's one file-read per minute. Cheap enough.

**Agent thrashes between tiers.** With the switch being free and encouraged, the agent might toggle thinking ↔ chat every turn. Mitigation: the system prompt describes tier use explicitly ("switch back to chat when done"), and Opus is noticeably more expensive per token, so the feedback loop on cost ledgers should self-correct over time. If thrashing becomes a problem, add a cooldown (e.g. no more than one switch per N turns) later — out of scope for this spec.

**Silent-hang timeout is too aggressive.** 300s total deadline might cut off a legitimate long Opus reasoning call. Mitigation: 300s is conservative — observed LLM calls in the logs complete in 1.4–2.4s for chat messages, minutes for deep work but not hours. If we see real calls timing out, bump the deadline; the fix is a single constant.

**Breaking change surface.** `list_models` return shape changes (live catalog vs hardcoded). Any callers inside zeroclaw that depend on the old static list break. Mitigation: `grep` confirms the only caller is the agent via the tool interface, which receives JSON and adapts naturally.

## Testing

Per zeroclaw convention (`./dev/ci.sh all`), plus:

- **Unit:** `ModelCatalogClient` with a mocked HTTP response; verify cache TTL, error propagation, tier resolution.
- **Integration:** stand up a fake `/v1/models` + `/v1/tiers` server in a test, drive `model_switch` through all four new/changed actions, assert the expected model name is staged.
- **Silent-hang regression:** fake cliproxy returns 502 with an error body; assert the provider call returns an error (not a hang), that the error surfaces a usable message, and that the agent loop records a failure event rather than silently waiting.
- **Live smoke:** on a dev build against a real cliproxy, `model_switch list_tiers` returns three tiers; `set_tier thinking` stages `claude-opus-4-7`; `list_models` returns the live catalog.

## Rollout

1. Ship the cliproxy `/v1/tiers` endpoint + `tiers.yaml` seed. Verify from shane/meg via `curl`.
2. Ship the zeroclaw Rust change behind a feature flag? **No** — the change is additive (new tool actions), and the one modified existing action (`list_models`) degrades gracefully to "empty list" on any discovery failure. Direct deploy is fine.
3. Update the persona system prompts (adi-persona repo) to mention tier switching. Optional but makes the feature discoverable to the agent.

## Rollback

- **Zeroclaw side:** revert the Rust commits; redeploy. `model_switch` goes back to the hardcoded list. The silent-hang regression returns (acceptable short-term; we restarted once already).
- **Cliproxy side:** remove or stub the `/v1/tiers` endpoint. Zeroclaw's `set_tier` calls then fail with a clear error; agent falls back to `set`.
- Volume state (`tiers.yaml`) can be left behind harmlessly.
