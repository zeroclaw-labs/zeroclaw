# OpenAI Codex over a ChatGPT subscription

Run an agent on the `openai` slot, paid through a ChatGPT subscription instead
of metered `OPENAI_API_KEY` billing. The agent is a GPT-5.x Codex model driving
ZeroClaw's tools, authenticated by your Codex login rather than an API key.
Billing follows your ChatGPT plan: the usage included with your subscription is
consumed first, and Codex usage beyond that included allowance draws on your
account's flexible credits at OpenAI's per-model token rates. It is not a flat
per-call `$0` path once you are past the included allowance.

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

There is no `api_key` field; `requires_openai_auth = true` is the switch that
reads the stored Codex login rather than a key on the entry. See
[Configuration → OAuth and subscription auth](./configuration.md#oauth-and-subscription-auth).

The alias half (`coding`, `review`) is operator-chosen; pick whatever fits.
Reference it from an agent with `model_provider = "openai.coding"`.

## Models

The `responses` wire API hits the Codex backend directly, so the `model` value
must be an **exact served ID**: the Codex CLI's client-side aliases
(`gpt-5`, `gpt-5.3`, `instant`, `gpt-5.5-instant`) are not resolved here and
fail with a `400`.

Treat the served catalog as volatile. Query it rather than trusting any
hardcoded list, including this one:

```bash
# Field names match the live ~/.codex/auth.json (verify against the file itself;
# the layout has shifted across Codex versions).
AT=$(jq -r .tokens.access_token ~/.codex/auth.json)
# account_id is OPTIONAL in auth.json; ZeroClaw falls back to the OAuth JWT when
# it is absent. `// empty` keeps jq from emitting the literal string "null", and
# the header is sent only when the field is actually present. After an import you
# can also read the resolved id from `zeroclaw auth status`.
ACC=$(jq -r '.tokens.account_id // empty' ~/.codex/auth.json)
curl -s "https://chatgpt.com/backend-api/codex/models?client_version=1.0.0" \
  -H "Authorization: Bearer ${AT}" \
  ${ACC:+-H "chatgpt-account-id: ${ACC}"} \
  -H "originator: pi" | jq -r '.models[].slug'
```

> `client_version` is required and gated: a stale or too-low value returns an
> empty `{"models": []}` with no error. Use a current client version (for
> example `1.0.0`) if the list comes back empty.

Served catalog (2026-06-02; verify against the endpoint before pinning):

| Served ID | Role |
|---|---|
| `gpt-5.4` | everyday coding (default workhorse) |
| `gpt-5.5` | frontier: complex coding / reasoning |
| `gpt-5.4-mini` | small, fast, cheap; simpler tasks and subagents |
| `gpt-5.3-codex-spark` | ultra-fast coding iteration |
| `codex-auto-review` | automatic code-review model |

> `GPT-5.5 Instant` and `GPT-5.3` are **ChatGPT-app** models, a different
> namespace that is not served on the Codex backend, so they are not usable
> from this slot.

To avoid editing config on every model bump, resolve roles to current served
IDs dynamically (enumerate `codex/models`, pick the newest match per role)
rather than pinning a version.

## Cost and routing

How OpenAI bills this path (follow OpenAI's current Codex / ChatGPT-plan billing
docs, which supersede any figure pinned here):

1. **Included plan usage first.** Each ChatGPT plan includes a Codex usage
   allowance that refreshes on a rolling window. While you are inside it, Codex
   requests do not draw extra charges.
2. **Flexible credits after the included allowance.** Once the included usage is
   exhausted, Codex usage draws from your account's credit balance where the plan
   supports it. Input, cached input, and output are priced as credits per 1M
   tokens, so what a task consumes depends on its token mix and the model used.
3. **Over-limit options.** When the included allowance and any credits are gone,
   OpenAI's paths are add credits, upgrade the plan, or wait for the window to
   reset.

The plan tiers below are therefore **usage-allowance multipliers**, not a
guarantee of per-call zero cost.

### ZeroClaw cost tracking

ZeroClaw records this slot at `$0` per call. **That is a local accounting
limitation, not an OpenAI billing fact:** ZeroClaw cannot see your ChatGPT plan's
included-usage meter or credit balance, so it cannot attribute per-call token
cost to a subscription request. Read the `$0` as "not metered by ZeroClaw", and
watch the real allowance / credit state in your OpenAI account. Keep subscription
and metered (api-key) classes separate in accounting; see
[Cost tracking](../ops/cost-tracking.md).

| Class | ZeroClaw budget signal | Real billing |
|---|---|---|
| Subscription (`openai` slot, Codex auth) | rolling Codex usage allowance | included plan usage, then per-token flexible credits |
| Metered (api-key providers) | running $ balance | per-token |

So routing is about spending the included allowance deliberately and keeping a
fallback for when you are past it, both to avoid credit burn at per-model token
rates and to survive a hard stop. It is not "free per token" once you are
outside the included usage.

Routing is per-agent (see [Routing](./routing.md)): define one agent alias per
role, each pointing at an `openai` Codex entry, and point channels at the agent
that should handle their traffic.

| Role | Served model |
|---|---|
| everyday coding (default) | `gpt-5.4` |
| code review / adversarial | `codex-auto-review` |
| heavy / frontier reasoning | `gpt-5.5` |
| light / narrow / subagent | `gpt-5.4-mini` |

Keep a metered fallback for when the subscription can't serve: allowance
exhausted (`429`), token refresh in backoff (see below), or an unavailable
model string. The fallback is per-token, so it should be the exception. Which
providers sit in that fallback set is environment-specific; configure it in
your own routing, not here.

## Subscription tiers and limits

ChatGPT tiers relevant to this slot (as of 2026-06). The "allowance" column is
the **included-usage multiplier**, not a free-call ceiling: beyond the included
allowance every tier falls back to per-token flexible credits at OpenAI's
published Codex rates. A higher tier raises the included multiplier; it does not
make usage free.

| Tier | Price | Codex included-usage allowance |
|---|---|---|
| Plus | $20/mo  | baseline |
| Pro  | $100/mo | 5× Plus limits |
| Pro  | $200/mo | 20× Plus limits |

Both Pro tiers expose the same model suite and features; they differ only in
included-allowance volume.

> **The $100 tier stepped down on 2026-06-01.** Through 2026-05-31 it ran a
> launch promotion at 10× Plus, then reverted to the standard 5×. Per-model
> message counts captured before that date included a temporary 2× boost and
> are no longer accurate.

OpenAI does not publish hard per-tier Codex message counts, and does not pin
"unlimited" to specific model names; the public pricing page shows one "Pro"
card ("From $100", headline "5x or 20x more usage") with the catch-all
"unlimited, subject to abuse guardrails." Treat any specific per-model count,
from older docs or other sources, as non-authoritative. The Pro reasoning
flagship is GPT-5.5 Pro.

> The pricing page's `128K` / `400K` context-window and `~680 pages` figures
> describe the ChatGPT-app GPT Instant / GPT Reasoning models, a different
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
another, because the target cannot decrypt it; the runtime logs
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

**Refresh tokens rotate, one owner only.** Each successful refresh invalidates
the previous refresh token. If two hosts refresh the same account
independently, they invalidate each other:

```text
error=OpenAI token refresh is in backoff for 9s due to previous failures
```

The pattern that works across more than one host, strictly limited to machines
**you own under the same OpenAI account**:

> ⚠️ **Credential boundary.** `~/.codex/auth.json` holds live bearer and refresh
> credentials for your OpenAI account. Distribute it **only** to your own hosts,
> over a **private, encrypted channel**: a secret manager, an encrypted
> transport, or an SSH-only pull. **Never** commit it to a repo, publish it,
> paste it into chat or a ticket, or share it with another user or a team.
> OpenAI's terms prohibit sharing account credentials or making an account
> available to someone else, and a raw `auth.json` pull point is a high-value
> secret on its own. This is operator-facing credential-handling guidance; the
> runtime code does not change it.

1. One host owns the refresh (e.g. the one running the Codex CLI's background
   refresh) and keeps `~/.codex/auth.json` current.
2. That host publishes the raw `~/.codex/auth.json` to a **private** pull point
   (secret manager or encrypted/SSH-only channel), reachable only by your own
   hosts.
3. Every other host pulls the raw `auth.json` (portable, it is just the token)
   and re-imports it locally, which re-encrypts it under that host's own
   `.secret_key`.
4. Other hosts do not refresh independently.

The artifact you distribute is the raw `~/.codex/auth.json`, never the encrypted
`auth-profiles.json`, and only ever to your own machines through a private,
encrypted channel.

## Verifying

```bash
zeroclaw auth status   # present and unexpired
# then drive the agent once against the local gateway
```

A healthy run returns model output with `exit_code=0`. Two failure signatures:

- `... token refresh in backoff`: stale or rotated token; re-pull the raw
  `auth.json` and re-import.
- `model=<x> ... 400`: unsupported model string; use an exact served ID.

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
