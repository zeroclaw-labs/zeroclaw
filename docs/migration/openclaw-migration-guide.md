# OpenClaw → ZeroClaw Migration Guide

This guide walks you through migrating an OpenClaw deployment to ZeroClaw. It covers configuration conversion, endpoint changes, and the architectural differences you need to know.

## Quick Start

```bash
# 1. Convert your OpenClaw config
python scripts/convert-openclaw-config.py ~/.openclaw/openclaw.json -o config.toml

# 2. The compatibility layer is built into ZeroClaw — no files to copy.
#    The endpoints are implemented in src/gateway/openclaw_compat.rs and
#    are already wired into the router in src/gateway/mod.rs.

# 3. Build and deploy
cargo build --release
```

---

## Architecture: What Changed and Why

OpenClaw was designed as an **OpenAI-compatible API server**. You called it like a remote LLM — send `messages[]`, get a completion back. The gateway was essentially a proxy that added system prompts and tool capabilities.

ZeroClaw is a **standalone messaging gateway**. It owns the full agent loop internally. Channels (WhatsApp, Linq, Nextcloud Talk) send a single message string, and ZeroClaw handles everything: system prompt construction, tool invocation, memory recall, context enrichment, and response generation.

This means there's no built-in `/v1/chat/completions` endpoint that runs the full agent loop. The one that exists in `openai_compat.rs` uses a simpler chat path without tools or memory.

### What This Toolkit Adds

Two new endpoints that bridge the gap:

| Endpoint | Format | Agent Loop | Use Case |
|----------|--------|------------|----------|
| `POST /api/chat` | ZeroClaw-native JSON | Full (with tools + memory) | **Recommended** for new integrations |
| `POST /v1/chat/completions` | OpenAI-compatible | Full (with tools + memory) | **Drop-in compat** for existing callers |

Both endpoints route through `run_gateway_chat_with_tools` → `agent::process_message`, which is the same code path used by Linq, WhatsApp, and all native channels.

---

## Endpoint Reference

### POST /api/chat (Recommended)

The clean, ZeroClaw-native endpoint.

**Request:**
```json
{
  "message": "What's on my schedule today?",
  "session_id": "optional-session-id",
  "context": [
    "User: Can you check my calendar?",
    "Assistant: Sure, let me look that up."
  ]
}
```

- `message` (required): The user's message.
- `session_id` (optional): Scopes memory operations to a session.
- `context` (optional): Recent conversation history lines. Use this to give the agent rolling context beyond what semantic memory surfaces.

**Response:**
```json
{
  "reply": "Here's what I found on your schedule...",
  "model": "us.anthropic.claude-sonnet-4-6",
  "session_id": "optional-session-id"
}
```

**Auth:** `Authorization: Bearer <gateway_token>`

### POST /v1/chat/completions (Compat Shim)

Drop-in replacement for OpenAI-compatible callers. Accepts standard OpenAI format, extracts the last user message plus conversation history, and routes through the full agent loop.

**Request:** Standard OpenAI chat completions format.
```json
{
  "messages": [
    {"role": "user", "content": "Hello"},
    {"role": "assistant", "content": "Hi! How can I help?"},
    {"role": "user", "content": "What's my email?"}
  ],
  "model": "claude-sonnet-4-6",
  "stream": false
}
```

**Response:** Standard OpenAI chat completions format.
```json
{
  "id": "chatcmpl-abc123",
  "object": "chat.completion",
  "created": 1709000000,
  "model": "claude-sonnet-4-6",
  "choices": [{
    "index": 0,
    "message": {"role": "assistant", "content": "Your email is..."},
    "finish_reason": "stop"
  }],
  "usage": {"prompt_tokens": 42, "completion_tokens": 15, "total_tokens": 57}
}
```

**Streaming:** Set `stream: true` for SSE response. Note: streaming is simulated — the full response generates first, then streams as chunks. This maintains API compatibility.

**Auth:** `Authorization: Bearer <gateway_token>`

---

## Configuration Mapping

### Provider & Model

| OpenClaw (JSON) | ZeroClaw (TOML) |
|-----------------|-----------------|
| `agent.model = "anthropic/claude-opus-4-6"` | `default_provider = "anthropic"` + `default_model = "claude-opus-4-6"` |
| `agent.model = "openai/gpt-4o"` | `default_provider = "openai"` + `default_model = "gpt-4o"` |
| `agent.temperature = 0.7` | `default_temperature = 0.7` |

For Bedrock:
```toml
default_provider = "bedrock"
default_model = "us.anthropic.claude-sonnet-4-6"
```

### Gateway

| OpenClaw | ZeroClaw |
|----------|----------|
| `gateway.port = 18789` | `[gateway]` `port = 42617` |
| `gateway.bind = "127.0.0.1"` | `[gateway]` `host = "127.0.0.1"` |
| `gateway.auth.mode = "token"` | `[gateway]` `require_pairing = true` |

### Memory

OpenClaw stores state in `~/.openclaw/`. ZeroClaw uses configurable backends:

```toml
[memory]
backend = "sqlite"              # sqlite | postgres | qdrant | markdown | none
auto_save = true
embedding_provider = "openai"   # openai | custom:URL | none
embedding_model = "text-embedding-3-small"
vector_weight = 0.7             # weight for semantic search
keyword_weight = 0.3            # weight for BM25 keyword search
```

### Channels

| OpenClaw Channel | ZeroClaw Status |
|------------------|-----------------|
| WhatsApp | ✅ Native (`/whatsapp`) |
| Telegram | ✅ Native (channels_config) |
| Discord | ✅ Native (channels_config) |
| Slack | ✅ Native (channels_config) |
| Matrix | ✅ Native (channels_config) |
| Lark/Feishu | ✅ Native |
| Nextcloud Talk | ✅ Native (`/nextcloud-talk`) |
| Linq | ✅ Native (`/linq`) |
| Signal | ❌ Use /api/chat bridge |
| iMessage | ❌ Use /api/chat bridge |
| Google Chat | ❌ Use /api/chat bridge |
| MS Teams | ❌ Use /api/chat bridge |
| WebChat | ❌ Use /api/chat or /v1/chat/completions |

For unsupported channels, point your existing integration at ZeroClaw's `/api/chat` endpoint instead of OpenClaw's `/v1/chat/completions`.

---

## Callsite Migration Examples

### Before (OpenClaw)

```typescript
const response = await fetch(`https://${host}/v1/chat/completions`, {
  method: "POST",
  headers: {
    "Authorization": `Bearer ${apiKey}`,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    model: "anthropic/claude-sonnet-4-6",
    messages: conversationHistory,
  }),
});
const data = await response.json();
const reply = data.choices[0].message.content;
```

### After — Option A: Use /api/chat (Recommended)

```typescript
const response = await fetch(`https://${host}/api/chat`, {
  method: "POST",
  headers: {
    "Authorization": `Bearer ${gatewayToken}`,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    message: userMessage,
    session_id: sessionId,
    context: recentMessages.map(m => `${m.role}: ${m.content}`),
  }),
});
const data = await response.json();
const reply = data.reply;
```

### After — Option B: Use /v1/chat/completions (Zero Code Changes)

```typescript
// Same code as before — just point to ZeroClaw host with gateway token.
// The compat shim handles the translation.
const response = await fetch(`https://${host}/v1/chat/completions`, {
  method: "POST",
  headers: {
    "Authorization": `Bearer ${gatewayToken}`,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    model: "claude-sonnet-4-6",
    messages: conversationHistory,
  }),
});
```

---

## Conversation Context: What to Know

ZeroClaw's `process_message` starts fresh on each call. It uses **semantic memory recall** (SQLite hybrid search with embeddings + BM25) to surface relevant past context, not ordered conversation history.

What this means in practice:

| Query Type | Works? | Why |
|------------|--------|-----|
| "What's my email?" | ✅ Usually | If previously discussed, semantic recall finds it |
| "What did you just say?" | ❌ No | No rolling history — previous turn isn't available |
| "Summarize our conversation" | ⚠️ Partial | Semantic recall surfaces fragments, not full history |

**Mitigation:** Both endpoints accept context injection. Pass recent conversation history in:
- `/api/chat`: Use the `context` array field
- `/v1/chat/completions`: The shim automatically extracts the last 10 messages from the `messages[]` array and prepends them as context

For full conversation history support, a follow-up change to ZeroClaw's agent loop would be needed to accept a `messages[]` parameter directly.

---

## Config Converter Usage

```bash
# Basic conversion
python scripts/convert-openclaw-config.py ~/.openclaw/openclaw.json

# Specify output path
python scripts/convert-openclaw-config.py ~/.openclaw/openclaw.json -o ~/.zeroclaw/config.toml

# Preview without writing
python scripts/convert-openclaw-config.py ~/.openclaw/openclaw.json --dry-run
```

The converter handles: provider/model parsing, gateway settings, memory defaults, agent configurations, and channel mapping. It generates migration notes highlighting anything that needs manual attention.

---

## Deployment Checklist

- [ ] Run config converter and review output
- [ ] Set API key: `export ZEROCLAW_API_KEY='...'`
- [ ] Build: `cargo build --release`
- [ ] Deploy (Docker or native)
- [ ] Pair: `curl -X POST http://host:port/pair -H 'X-Pairing-Code: ...'`
- [ ] Verify health: `curl http://host:port/health`
- [ ] Test /api/chat: `curl -X POST http://host:port/api/chat -H 'Authorization: Bearer ...' -d '{"message":"hello"}'`
- [ ] Test /v1/chat/completions: `curl -X POST http://host:port/v1/chat/completions -H 'Authorization: Bearer ...' -d '{"messages":[{"role":"user","content":"hello"}]}'`
- [ ] Update callers to point to new host
- [ ] Monitor logs for errors

---

## Troubleshooting

**405 on /v1/chat/completions:** The endpoint isn't registered. Make sure you're running a ZeroClaw build that includes the `openclaw_compat` module (check `src/gateway/mod.rs` for the route registration).

**401 Unauthorized:** Pairing is enabled but you're not sending a valid bearer token. Run the `/pair` flow first.

**Agent returns empty/generic response:** Check that `default_provider` and `default_model` are set correctly, and that the provider API key is available (via env var or config).

**"No user message found":** The compat shim expects at least one message with `role: "user"` in the messages array.

**Memory not working:** Ensure `[memory]` backend is set to something other than `"none"`, and that `embedding_provider` is configured with a valid API key for embedding generation.
