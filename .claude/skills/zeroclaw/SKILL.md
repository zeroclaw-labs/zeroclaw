---
name: zeroclaw
description: "Help users operate and interact with their ZeroClaw agent instance — through both the CLI (`zeroclaw` commands) and the REST/WebSocket gateway API. Use this skill whenever the user wants to: send messages to ZeroClaw, manage memory or cron jobs, check system status, configure channels or providers, hit the gateway API, troubleshoot their ZeroClaw setup, build from source, or do anything involving the `zeroclaw` binary or its HTTP endpoints. Trigger this even if the user just says things like 'check my agent status', 'schedule a reminder', 'store this in memory', 'list my cron jobs', 'send a message to my bot', 'set up Telegram', 'build zeroclaw', or 'my bot is broken' — these are all ZeroClaw operations."
---

# ZeroClaw Skill

You are helping a user operate their ZeroClaw agent instance. ZeroClaw is an autonomous agent runtime with a CLI and an HTTP/WebSocket gateway.

Your job is to understand what the user wants to accomplish and then **execute it** — run the command, make the API call, report the result. Do not just show commands for the user to copy-paste. Actually run them via the Bash tool and tell the user what happened. The only exception is destructive operations (clearing all memory, estop kill-all) where you should confirm first.

## Adaptive Expertise

Pay attention to how the user talks. Someone who says "can you hit the webhook endpoint with a POST" is telling you they know what they're doing — be concise, skip explanations, just execute. Someone who says "how do I make my bot remember things" needs more context about what's happening under the hood.

Signals of technical comfort: mentions specific endpoints, HTTP methods, JSON fields, talks about tokens/auth, uses CLI flags fluently, references config files directly.

Signals of less familiarity: asks "what does X do", uses casual language about the bot/agent, describes goals rather than mechanisms ("I want it to check something every morning").

Default to a middle ground — brief explanation of what you're about to do, then do it. Dial up or down from there based on cues.

## Discovery — Before You Act

Before running any ZeroClaw operation, make sure you know where things are:

1. **Find the binary.** Search in this order:
   - `which zeroclaw` (PATH)
   - The current project's build output: `./target/release/zeroclaw` or `./target/debug/zeroclaw` — this is the right choice when the user is working inside the ZeroClaw source tree and may have local changes
   - Common install locations: `~/.cargo/bin/zeroclaw`, `~/Downloads/zeroclaw-bin/zeroclaw`

   If no binary is found anywhere, offer to build from source (see "Building from Source" below). If the user is a developer working on ZeroClaw itself, they'll likely want the local build — watch for cues like them editing source files, mentioning PRs, or being in the project directory.

2. **Check if the gateway is running** (only needed for REST/WebSocket operations). A quick `curl -sf http://127.0.0.1:42617/health` tells you. If it's not running and the user wants REST access, let them know and offer to start it (`zeroclaw gateway` or `zeroclaw daemon`).

3. **Check auth status.** If the gateway requires pairing (`require_pairing = true` is the default), REST calls need a bearer token. Run `zeroclaw status` to see the current state, or check `~/.zeroclaw/config.toml` for a stored token under `[gateway]`.

Cache these findings for the conversation — don't re-discover every time.

## Important: REPL Limitation

`zeroclaw agent` (interactive REPL) requires interactive stdin, which doesn't work through the Bash tool. When the user wants to chat with their agent, use single-message mode instead:

```bash
zeroclaw agent -m "the message"
```

Each `-m` invocation is independent (no conversation history between calls). If the user needs multi-turn conversation, let them know they can run `zeroclaw agent` directly in their terminal, or use the WebSocket endpoint for programmatic streaming.

## First-Time Setup

If the user hasn't set up ZeroClaw yet (no `~/.zeroclaw/config.toml` exists), guide them through onboarding:

```bash
zeroclaw onboard                          # Quick mode — defaults to OpenRouter
zeroclaw onboard --provider anthropic     # Use Anthropic directly
zeroclaw onboard --interactive            # Step-by-step wizard
```

After onboarding, verify everything works:
```bash
zeroclaw status
zeroclaw doctor
```

If they already have a config but something is broken, `zeroclaw onboard --channels-only` repairs just the channel configuration without overwriting everything else.

## Building from Source

If the user wants to build ZeroClaw (or no binary is installed):

```bash
cargo build --release
```

This produces `target/release/zeroclaw`. For faster iteration during development, `cargo build` (debug mode) is quicker but produces a slower binary at `target/debug/zeroclaw`.

You can also run directly without a separate build step:
```bash
cargo run --release -- <subcommand> [args]
```

Before building, `cargo check` gives a quick compile validation without the full build.

## Choosing CLI vs REST

Both surfaces can do most things. Rules of thumb:

- **CLI is simpler** for one-off operations from the terminal. It handles auth internally and formats output nicely. Prefer CLI when the user is working locally.
- **REST is needed** when the user is building an integration, scripting from another language, or accessing a remote ZeroClaw instance. Also needed for streaming (WebSocket, SSE).
- If unclear, **default to CLI** — it's less setup.

## Core Operations

### Sending Messages

**CLI:** `zeroclaw agent -m "your message here"` — remember, always use `-m` mode, not bare `zeroclaw agent`.

**REST:**
```bash
curl -X POST http://127.0.0.1:42617/webhook \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"message": "your message here"}'
```
Response: `{"response": "...", "model": "..."}`

**WebSocket** (for streaming): connect to `ws://127.0.0.1:42617/ws/chat?token=<token>`, send `{"type": "message", "content": "..."}`, receive `{"type": "done", "full_response": "..."}`.

### System Status

Run `zeroclaw status` to see provider, model, uptime, channels, memory backend. For deeper diagnostics: `zeroclaw doctor`.

**REST:** `GET /api/status` (same info as JSON), `GET /health` (no auth, quick ok/not-ok).

### Memory

The CLI can list, get, and clear memories but **cannot store** them directly. To store a memory:
- Via agent: `zeroclaw agent -m "remember that my favorite color is blue"`
- Via REST: `POST /api/memory` with `{"key": "...", "content": "...", "category": "core"}`

**CLI (read/delete):**
- `zeroclaw memory list` — list all entries
- `zeroclaw memory list --category core --limit 10` — filtered
- `zeroclaw memory get "key-name"` — get specific entry
- `zeroclaw memory stats` — usage statistics
- `zeroclaw memory clear --key "prefix" --yes` — delete entries (confirm with user first)

**REST (full CRUD):**
- `GET /api/memory` — list all (optional: `?query=search+text&category=core`)
- `POST /api/memory` — store: `{"key": "...", "content": "...", "category": "core"}`
- `DELETE /api/memory/{key}` — delete entry

Categories: `core`, `daily`, `conversation`, or any custom string.

### Cron / Scheduling

**CLI:**
- `zeroclaw cron list` — show all jobs
- `zeroclaw cron add '0 9 * * 1-5' 'Good morning' --tz America/New_York` — recurring
- `zeroclaw cron add-at '2026-03-11T10:00:00Z' 'Remind me'` — one-time at specific time
- `zeroclaw cron add-every 3600000 'Check health'` — interval in ms
- `zeroclaw cron once 30m 'Follow up'` — delay from now
- `zeroclaw cron pause <id>` / `zeroclaw cron resume <id>` / `zeroclaw cron remove <id>`

**REST:**
- `GET /api/cron` — list jobs
- `POST /api/cron` — add: `{"name": "...", "schedule": "0 9 * * *", "command": "..."}`
- `DELETE /api/cron/{id}` — remove job

### Tools

Tools are used automatically by the agent during conversations (shell, file ops, memory, browser, HTTP, web search, git, etc. — 30+ tools gated by security policy).

To see what's available: `GET /api/tools` (REST) lists all registered tools with descriptions and parameter schemas.

### Configuration

Edit `~/.zeroclaw/config.toml` directly, or re-run `zeroclaw onboard` to reconfigure.

**REST:**
- `GET /api/config` — get current config (secrets masked as `***MASKED***`)
- `PUT /api/config` — update config (send raw TOML as body, 1MB limit)

### Providers & Models

- `zeroclaw providers` — list all supported providers
- `zeroclaw models list` — cached model catalog
- `zeroclaw models refresh --all` — refresh from providers
- `zeroclaw models set anthropic/claude-sonnet-4-6` — set default model

Override per-message: `zeroclaw agent -p anthropic --model claude-sonnet-4-6 -m "hello"`

### Real-Time Events (SSE)

REST only — useful for building dashboards or monitoring:
```bash
curl -N -H "Authorization: Bearer <token>" http://127.0.0.1:42617/api/events
```
Streams JSON events: `llm_request`, `tool_call_start`, `tool_call`, `agent_start`, `agent_end`, `error`.

### Cost Tracking

`GET /api/cost` — returns session/daily/monthly costs, token counts, per-model breakdown.

### Emergency Stop

Confirm with the user before running any estop command — these are disruptive.

- `zeroclaw estop --level kill-all` — stop everything
- `zeroclaw estop --level network-kill` — block all network
- `zeroclaw estop --level tool-freeze --tool shell` — freeze specific tool
- `zeroclaw estop status` — check current estop state
- `zeroclaw estop resume --network` — resume

### Gateway Lifecycle

- `zeroclaw gateway` — start HTTP gateway (foreground)
- `zeroclaw gateway -p 8080 --host 127.0.0.1` — custom bind
- `zeroclaw daemon` — start gateway + channels + scheduler + heartbeat
- `zeroclaw service install/start/stop/status/uninstall` — OS service management

### Channels

ZeroClaw supports 21 messaging channels. To add one, you need to edit `~/.zeroclaw/config.toml`. For example, to set up Telegram:

```toml
[channels]
telegram = true

[channels_config.telegram]
bot_token = "your-bot-token-from-botfather"
allowed_users = [123456789]
```

Then restart the daemon. Check channel health with `zeroclaw channels doctor`.

For the full list of channels and their config fields, read `references/cli-reference.md` (Channels section).

### Pairing (Authentication Setup)

When `require_pairing = true` (default), REST clients need a bearer token:
```bash
curl -X POST http://127.0.0.1:42617/pair -H "X-Pairing-Code: <code>"
```
Response includes `{"token": "..."}` — save this for subsequent requests.

## Common Workflows

Here are multi-step sequences you're likely to need:

**"Is my agent healthy?"**
1. Run `zeroclaw status` — check provider, model, channels
2. Run `zeroclaw doctor` — check connectivity, diagnose issues
3. If gateway needed: `curl -sf http://127.0.0.1:42617/health`

**"Set up a new channel"**
1. Read the current config: `cat ~/.zeroclaw/config.toml`
2. Add the channel config (edit the TOML)
3. Restart: `zeroclaw service restart` (or restart daemon manually)
4. Verify: `zeroclaw channels doctor`

**"Switch to a different model"**
1. Check available: `zeroclaw models list`
2. Set it: `zeroclaw models set <provider/model>`
3. Verify: `zeroclaw status`
4. Test: `zeroclaw agent -m "hello, what model are you?"`

## Gateway Defaults

- **Port:** 42617
- **Host:** 127.0.0.1
- **Auth:** Pairing required (bearer token)
- **Rate limits:** 60 webhook requests/min, 10 pairing attempts/min
- **Body limit:** 64KB (1MB for config updates)
- **Timeout:** 30 seconds
- **Idempotency:** Optional `X-Idempotency-Key` header on `/webhook` (300s TTL)
- **Config location:** `~/.zeroclaw/config.toml`

## Reference Files

For the complete API specification with every endpoint, field, and edge case, read `references/rest-api.md`.

For the full CLI command tree with all flags and options, read `references/cli-reference.md`.

Only load these when you need precise details beyond what's in this file — for most operations, the quick references above are sufficient.

## Troubleshooting

**"zeroclaw: command not found"** — Binary not in PATH. Check `./target/release/zeroclaw`, `~/.cargo/bin/zeroclaw`, or build from source with `cargo build --release`.

**"Connection refused" on REST calls** — Gateway isn't running. Start it with `zeroclaw gateway` or `zeroclaw daemon`.

**"Unauthorized" (401/403)** — Bearer token is missing or invalid. Re-pair via `POST /pair` with the pairing code, or check `~/.zeroclaw/config.toml` for the stored token.

**"LLM request failed" (500)** — Provider issue. Run `zeroclaw doctor` to check connectivity. Common causes: expired API key, provider outage, rate limiting on the provider side.

**"Too many requests" (429)** — You're hitting ZeroClaw's rate limit. Back off — the response includes `retry_after` with the number of seconds to wait.

**Agent not using tools / acting limited** — Check autonomy settings in config.toml under `[autonomy]`. `level = "read_only"` disables most tools. Try `level = "supervised"` or `level = "full"`.

**Memory not persisting** — Check `[memory]` config. If `backend = "none"`, nothing is stored. Switch to `"sqlite"` or `"markdown"`. Also verify `auto_save = true`.

**Channel not responding** — Run `zeroclaw channels doctor` for the specific channel. Common issues: expired bot token, wrong allowed_users list, channel not enabled in `[channels]`.

Report errors to the user with context appropriate to their expertise level. For beginners, explain what went wrong and suggest the fix. For experts, just show the error and the fix.
