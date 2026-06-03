# OpenAI Codex over a ChatGPT subscription

Run an agent on the `openai` slot, paid by a ChatGPT subscription instead of
metered API billing. The agent is a GPT-5.x Codex model driving ZeroClaw's
tools: no `OPENAI_API_KEY`, no per-token spend — it draws the flat monthly
subscription you already have.

This page covers the slot config, the served model strings, the cost and
routing implications, and the OAuth wiring. For the universal provider fields
see [Configuration](./configuration.md); for the one-line catalog entry see the
[Provider Catalog](./catalog.md).

## Config

Codex subscription auth lives on the `openai` slot. Set `wire_api = "responses"`
to route through `POST /v1/responses` (the Codex backend, not the chat
completions API) and `requires_openai_auth = true` to pull credentials from
`~/.codex/auth.json` instead of an `api_key` field:

```toml
[providers.models.openai.coding]
model                = "gpt-5.4"
wire_api             = "responses"
requires_openai_auth = true

[providers.models.openai.review]
model                = "codex-auto-review"
wire_api             = "responses"
requires_openai_auth = true
```

There is no `api_key` field — `requires_openai_auth = true` is the switch that
reads the stored Codex login rather than a key on the entry. See
[Configuration → OAuth and subscription auth](./configuration.md#oauth-and-subscription-auth).

The alias half (`coding`, `review`) is operator-chosen — pick whatever fits.
Reference it from an agent with `model_provider = "openai.coding"`.

## Models

The `responses` wire API hits the Codex backend directly, so the `model` value
must be an **exact served ID** — the Codex CLI's client-side aliases
(`gpt-5`, `gpt-5.3`, `instant`, `gpt-5.5-instant`) are not resolved here and
fail with a `400`.

Treat the served catalog as volatile. Query it rather than trusting any
hardcoded list, including this one:

```bash
# Field names match the live ~/.codex/auth.json (verify against the file itself;
# the layout has shifted across Codex versions).
AT=$(jq -r .tokens.access_token ~/.codex/auth.json)
ACC=$(jq -r .tokens.account_id ~/.codex/auth.json)
curl -s "https://chatgpt.com/backend-api/codex/models?client_version=1.0.0" \
  -H "Authorization: Bearer ${AT}" \
  -H "chatgpt-account-id: ${ACC}" \
  -H "originator: pi" | jq -r '.models[].slug'
```

> `client_version` is required and gated: a stale or too-low value returns an
> empty `{"models": []}` with no error. Use a current client version (for
> example `1.0.0`) if the list comes back empty.

Served catalog (2026-06-02 — verify against the endpoint before pinning):

| Served ID | Role |
|---|---|
| `gpt-5.4` | everyday coding (default workhorse) |
| `gpt-5.5` | frontier — complex coding / reasoning |
| `gpt-5.4-mini` | small, fast, cheap; simpler tasks and subagents |
| `gpt-5.3-codex-spark` | ultra-fast coding iteration |
| `codex-auto-review` | automatic code-review model |

> `GPT-5.5 Instant` and `GPT-5.3` are **ChatGPT-app** models — a different
> namespace that is not served on the Codex backend, so they are not usable
> from this slot.

To avoid editing config on every model bump, resolve roles to current served
IDs dynamically (enumerate `codex/models`, pick the newest match per role)
rather than pinning a version.

## Cost and routing

In cost tracking the subscription is billed at `$0` per call — it is a
flat-rate provider, not a metered one. Keep the two classes separate in
accounting; see [Cost tracking](../ops/cost-tracking.md).

| Class | Billing | Budget |
|---|---|---|
| Subscription (`openai` slot, Codex auth) | flat $/mo, not per-token | rolling 5-hour Codex message allowance |
| Metered (api-key providers) | per-token | running $ balance |

The subscription is flat-rate but **finite**: `$0` per call, but a 5-hour
rolling Codex allowance you can exhaust. So routing is about spending that
allowance deliberately and keeping a paid path for when it runs out, not about
saving per-token money.

Routing is per-agent (see [Routing](./routing.md)): define one agent alias per
role, each pointing at an `openai` Codex entry, and point channels at the agent
that should handle their traffic.

| Role | Served model |
|---|---|
| everyday coding (default) | `gpt-5.4` |
| code review / adversarial | `codex-auto-review` |
| heavy / frontier reasoning | `gpt-5.5` |
| light / narrow / subagent | `gpt-5.4-mini` |

Keep a metered fallback for when the subscription can't serve — allowance
exhausted (`429`), token refresh in backoff (see below), or an unavailable
model string. The fallback is per-token, so it should be the exception. Which
providers sit in that fallback set is environment-specific; configure it in
your own routing, not here.

## Subscription tiers and limits

ChatGPT tiers relevant to this slot (as of 2026-06):

| Tier | Price | Codex allowance |
|---|---|---|
| Plus | $20/mo  | baseline |
| Pro  | $100/mo | 5× Plus limits |
| Pro  | $200/mo | 20× Plus limits |

Both Pro tiers expose the same model suite and features; they differ only in
rate-limit volume.

> **The $100 tier stepped down on 2026-06-01.** Through 2026-05-31 it ran a
> launch promotion at 10× Plus, then reverted to the standard 5×. Per-model
> message counts captured before that date included a temporary 2× boost and
> are no longer accurate.

OpenAI does not publish hard per-tier Codex message counts, and does not pin
"unlimited" to specific model names — the public pricing page shows one "Pro"
card ("From $100", headline "5x or 20x more usage") with the catch-all
"unlimited, subject to abuse guardrails." Treat any specific per-model count,
from older docs or other sources, as non-authoritative. The Pro reasoning
flagship is GPT-5.5 Pro.

> The pricing page's `128K` / `400K` context-window and `~680 pages` figures
> describe the ChatGPT-app GPT Instant / GPT Reasoning models — a different
> namespace than the Codex `responses` backend this slot uses. Do not read
> them as Codex-backend limits.

## Importing the token

Import the existing Codex-CLI token non-interactively rather than starting a
browser flow:

```bash
zeroclaw auth login --model-provider openai-codex --import ~/.codex/auth.json
zeroclaw auth status   # openai-codex:default kind=OAuth account=... expires=...
```

(Interactive alternatives: `zeroclaw auth login` without `--import`, or
`--device-code`.)

Run the daemon from the default config-dir (`~/.zeroclaw`). The auth profile is
stored there natively and the `zeroclaw auth` commands default there; pointing
the daemon at a custom dir means the profile has to be placed there too, and
because it is encrypted per-config-dir (below), that is where the pain starts.

### Two things that bite

**Auth profiles are not portable.** `auth-profiles.json` is encrypted (`enc2:`)
with the config-dir's `.secret_key`. You cannot copy one host's profile to
another — the target cannot decrypt it; the runtime logs
`` enc2: decryption failed (wrong `.secret_key` or tampered ciphertext) `` (the
`or tampered ciphertext` clause shares this error path, so the message alone does
not distinguish a foreign profile from a corrupt blob). Each host imports its own
profile from a raw `~/.codex/auth.json`.
If a foreign `auth-profiles.json` is already present, move it aside first or the
import fails trying to load it:

```bash
mv ~/.zeroclaw/auth-profiles.json ~/.zeroclaw/auth-profiles.json.foreign 2>/dev/null
zeroclaw auth login --model-provider openai-codex --import ~/.codex/auth.json
```

**Refresh tokens rotate — one owner only.** Each successful refresh invalidates
the previous refresh token. If two hosts refresh the same account
independently, they invalidate each other:

```text
error=OpenAI token refresh is in backoff for 9s due to previous failures
```

The pattern that works across more than one host:

1. One host owns the refresh (e.g. the one running the Codex CLI's background
   refresh) and keeps `~/.codex/auth.json` current.
2. That host publishes the raw `~/.codex/auth.json` to a pull point.
3. Every other host pulls the raw `auth.json` (portable — it is just the token)
   and re-imports it locally, which re-encrypts it under that host's own
   `.secret_key`.
4. Other hosts do not refresh independently.

The artifact you distribute is the raw `~/.codex/auth.json`, never the encrypted
`auth-profiles.json`.

## Verifying

```bash
zeroclaw auth status   # present and unexpired
# then drive the agent once against the local gateway
```

A healthy run returns model output with `exit_code=0`. Two failure signatures:

- `... token refresh in backoff` — stale or rotated token; re-pull the raw
  `auth.json` and re-import.
- `model=<x> ... 400` — unsupported model string; use an exact served ID.

## New-host checklist

1. `~/.codex/auth.json` present and current (pulled from the refresh owner).
2. `zeroclaw auth login --model-provider openai-codex --import ~/.codex/auth.json`
   (move aside any foreign `auth-profiles.json` first).
3. `zeroclaw auth status` shows `openai-codex:default ... kind=OAuth ...
   expires=<future>`.
4. An `openai` entry with `wire_api = "responses"`, `requires_openai_auth =
   true`, and an exact served model ID.
5. Daemon on `--config-dir ~/.zeroclaw` (the default).
6. Drive the agent once → `exit_code=0` with real output.
7. Router maps roles to current served IDs (don't pin a version you will have
   to chase).
