# ZeroClaw Chat Completions API Guide

> **Document Version**: v1.0
> **Created**: 2026-05-15
> **Updated**: 2026-06-30
> **Project**: ZeroClaw
> **Endpoint**: `POST /v1/chat/completions`
> **Compatibility**: OpenAI Chat Completions API compatible

---

## 0. Parameter Quick Reference

### Request Parameters

#### HTTP Header Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `Authorization` | string | ❌ | - | Bearer Token auth (`Bearer <token>`), required when `require_pairing=true` |
| `x-session-key` | string | ❌ | - | Session ID. Recommended for multi-turn context continuity |

#### Request Body Parameters (JSON)

| Parameter | Type | Required | Default | Range/Enum | Description |
|-----------|------|----------|---------|------------|-------------|
| **`model`** | string | ❌ | default agent | `zeroclaw/<alias>` or plain label | **Agent routing target** (not provider model name). `"zeroclaw"` / `""` / plain label → default agent |
| **`messages`** | array | ✅ | - | ≥ 1 message | Message list. Multi-turn supported (history messages are split into conversation context) |
| `stream` | boolean | ❌ | `false` | `true`/`false` | SSE streaming response |
| `temperature` | float | ❌ | `0.7` | `0.0`~`2.0` | Sampling temperature |
| ~~`max_tokens`~~ | - | ❌ | - | - | ⛔ **Not supported** -- returns 400 if provided. Configure in provider settings instead |
| ~~`top_p`~~ | - | ❌ | - | - | ⛔ **Not supported** -- returns 400 if provided |
| ~~`stop`~~ | - | ❌ | - | - | ⛔ **Not supported** -- returns 400 if provided |
| ~~`presence_penalty`~~ | - | ❌ | - | - | ⛔ **Not supported** -- returns 400 if provided |
| ~~`frequency_penalty`~~ | - | ❌ | - | - | ⛔ **Not supported** -- returns 400 if provided |
| `tools` | array | ❌ | - | Tool object list | Available tool definitions for this request (filtered against agent's configured tools) |
| `tool_choice` | string/object | ❌ | `auto` | `"auto"` / `"none"` | Tool selection strategy. `"none"` disables all tools. `"required"` and `{"type":"function",...}` are **not yet supported** -- they return 400 |
| `stream_options` | object | ❌ | - | `{"include_usage": true}` | Stream options (controls usage reporting) |

**Message object structure** (`messages[]`):

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `role` | string | ✅ | `system` / `developer` / `user` / `assistant` / `tool` / `function` |
| `content` | string | ✅ | Message content (string only) |
| `name` | string | ❌ | Optional name |
| `tool_calls` | array | ❌ | Tool calls for assistant role |
| `tool_call_id` | string | ❌ | Tool call ID for tool role |

---

### Response Parameters

#### Non-streaming Response (JSON)

| Field | Type | Description |
|-------|------|-------------|
| **`id`** | string | Unique completion identifier (e.g. `chatcmpl-<uuid>`) |
| **`object`** | string | Object type, always `"chat.completion"` |
| **`created`** | integer | Unix timestamp (seconds) |
| **`model`** | string | Echo of request model (empty string normalized to `"zeroclaw"`) |
| **`choices`** | array | Generated candidate answers |
| **`usage`** | object | Token usage stats (via cost tracking; may be all zeros if unconfigured) |

**`choices[]` object structure**:

| Field | Type | Description |
|-------|------|-------------|
| `index` | integer | Choice index (typically 0) |
| `message` | object | Assistant message |
| `finish_reason` | string | Stop reason (always `"stop"`; tool execution is transparent) |

**`message` object structure**:

| Field | Type | Description |
|-------|------|-------------|
| `role` | string | Always `"assistant"` |
| `content` | string\|null | Assistant response text |
| `tool_calls` | array\|null | Always `null` in non-streaming mode (transparent execution) |

**`usage` object structure**:

| Field | Type | Description |
|-------|------|-------------|
| `prompt_tokens` | integer | Input token count (via cost tracking; 0 if unconfigured) |
| `completion_tokens` | integer | Output token count (via cost tracking; 0 if unconfigured) |
| `total_tokens` | integer | Total tokens consumed |

---

#### Streaming Response (SSE)

**SSE event stream format**:

```
data: {"id":"chatcmpl-<uuid>","object":"chat.completion.chunk","created":...,"model":"zeroclaw","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}
data: {"id":"chatcmpl-<uuid>","object":"chat.completion.chunk","created":...,"model":"zeroclaw","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}
data: {"id":"chatcmpl-<uuid>","object":"chat.completion.chunk","created":...,"model":"zeroclaw","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}
data: {"id":"chatcmpl-<uuid>","object":"chat.completion.chunk","created":...,"model":"zeroclaw","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}
data: [DONE]
```

### Error and Cancellation Events

When a streaming request fails or is cancelled, the SSE stream emits:

1. An in-band error event
2. An optional usage chunk (if `stream_options.include_usage` was requested)
3. The `[DONE]` terminator

**Wire format:**

```
data: {"error":{"message":"...","type":"internal_error","code":null,"param":null,"status":500}}
data: {"id":"...","object":"chat.completion.chunk","choices":[],"usage":{...}}
data: [DONE]
```

The error `type` is always `internal_error`. Cancellation and server errors produce the same wire shape.

**Streaming response field descriptions**:

| Field | Type | Description |
|-------|------|-------------|
| `id` | string | Completion ID (same as non-streaming) |
| `object` | string | Always `"chat.completion.chunk"` |
| `created` | integer | Unix timestamp |
| `model` | string | Echo of request model (empty string normalized to `"zeroclaw"`) |
| `choices[]` | array | Streaming chunks |
| `choices[].delta` | object | Incremental content (`role` / `content`) |
| `choices[].finish_reason` | string | Stop reason (`"stop"`) |
| `usage` | object | Token stats (only when `stream_options.include_usage: true`) |

---

### Response Headers

| Header | Description | Condition |
|--------|-------------|-----------|
| `Content-Type` | `application/json` or `text/event-stream` | Always |
| `X-Request-ID` | Server-generated unique request identifier (UUID). The client-supplied `x-request-id` header is not read or echoed. | Always |
| `x-session-key` | Session ID (without `gw_` prefix) | **Always returned** (generated server-side if not provided by client) |
| `X-RateLimit-Limit` | Max requests per minute | On success & 429 |
| `X-RateLimit-Remaining` | Remaining request allowance | On success & 429 |
| `X-RateLimit-Reset` | Rate limit reset timestamp | On success & 429 |
| `Retry-After` | Retry wait seconds on 429 | Only on 429 responses |

---

### Error Responses

| HTTP Status | error.type | Description | Common Cause |
|-------------|------------|-------------|--------------|
| **400** | `invalid_request_error` | Invalid request | Empty messages, JSON parse error, unknown agent, malformed agent target |
| **400** | `unsupported_parameter` | Unsupported parameter | `max_tokens`, `top_p`, `stop`, `presence_penalty`, or `frequency_penalty` provided |
| **401** | `authentication_error` | Auth failure | Missing or invalid token (only when `require_pairing=true`) |
| **408** | `timeout_error` | Request timeout | Request exceeded the configured timeout (default 600s) |
| **409** | `cross_transport_session_in_use` | Session conflict | Session currently owned by an active WebSocket connection |
| **413** | `payload_too_large` | Request body too large | Request body exceeds 64 KB limit |
| **429** | `rate_limit_error` | Rate limited | Exceeded `chat_rate_limit_per_minute` config |
| **500** | `internal_error` | Internal server error | Provider API exception |
| **503** | `server_error` | Agent not configured | Agent missing model config (complete onboarding) |

**Error response format**:
```json
{
  "error": {
    "message": "Error description",
    "type": "error_type",
    "code": null,
    "param": null,
    "status": 400
  }
}
```

---

## 1. Quick Start

### 1.1 Basic Request

```bash
# Simplest non-streaming call (no model = default agent)
curl -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "",
    "messages": [
      {"role": "user", "content": "Hello!"}
    ]
  }'

# Specify agent (zeroclaw/<alias> format)
curl -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "zeroclaw/coding",
    "messages": [
      {"role": "user", "content": "Write a quicksort"}
    ]
  }'

# Any model name works (compatible with standard clients, routes to default agent)
curl -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "gpt-4",
    "messages": [
      {"role": "user", "content": "Hello!"}
    ]
  }'
```

### 1.2 Streaming Request

```bash
# Streaming call (real-time output, recommended with x-session-key)
curl -N -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -H 'x-session-key: my-session-123' \
  -d '{
    "model": "zeroclaw/default",
    "stream": true,
    "messages": [
      {"role": "user", "content": "Hello!"}
    ]
  }'
```

---

## 2. Endpoint

| Attribute | Value |
|-----------|-------|
| **URL** | `POST /v1/chat/completions` |
| **Content-Type** | `application/json` |
| **Accept** | `text/event-stream` (streaming) or `application/json` (non-streaming) |
| **Auth** | Bearer Token (required when `require_pairing=true`) |
| **Timeout** | Long-running timeout (600s, shared with cron sub-route) |
| **Enabled** | Disabled by default -- set `chat_completions_enabled = true` in `[gateway]` config, then reload (daemon) or restart (standalone). See [Config lifecycle](../architecture/config-lifecycle.md) for reload semantics. |

> **Deployment mode note:** The `chat_completions_enabled` toggle takes effect
> differently depending on how the gateway is run:
>
> - **Daemon-supervised** (recommended deployment): Config changes + `/admin/reload`
>   cause the daemon to re-spawn the gateway with the updated route surface.
>   Toggling `chat_completions_enabled` on or off takes effect on reload.
> - **Standalone** (`zeroclaw gateway start`): Routes are fixed at process start.
>   Changing `chat_completions_enabled` in the config file has **no effect** on the
>   running process -- restart the gateway to apply the change. `/admin/reload`
>   returns 503 in this mode.

---

## 3. Request Parameters

### 3.1 HTTP Header Parameters

#### 3.1.1 Authorization

**Format**:
```
Authorization: Bearer <your-token>
```

**Details**:
- **Required when**: `require_pairing=true` in ZeroClaw config
- **Optional when**: `require_pairing=false` (debug mode)
- **Validation**: If enabled, invalid/missing token returns 401

---

#### 3.1.2 x-session-key

**Format**:
```
x-session-key: <session-id>
```

**Details**:
- **Purpose**: Maintain multi-turn conversation context
- **Generation**:
  - **Client-provided**: Uses the client-supplied session_id
  - **Client omitted**: Server generates a UUID, returned in `x-session-key` response header
- **Persistence**: Session history stored in session backend (auto-prefixed with `gw_`)
- **Single vs multi-turn behavior**:
  - `messages` has only 1 entry: auto-loads backend session history (if `x-session-key` present)
  - `messages` has > 1 entry: request messages are authoritative context, **does not** load backend history

**Important notes**:
```
📌 x-session-key is always returned in the HTTP Response Header (both streaming and non-streaming)
📌 The x-session-key header value does NOT include the "gw_" prefix
📌 Server storage auto-adds the "gw_" prefix to form the internal session key
📌 Recommendation: Generate and send x-session-key on first request, then reuse it
```

**Client generation example**:
```javascript
// JavaScript/Node.js
const sessionId = crypto.randomUUID();

// Python
import uuid
session_id = str(uuid.uuid4())

// Bash
session_id=$(cat /proc/sys/kernel/random/uuid)
```

---

### 3.2 Request Body Parameters (JSON)

#### 3.2.1 model (Agent Routing Target)

**Type**: `string`
**Required**: ❌ No (defaults to default agent)

**Core semantics**: The `model` field is an **agent routing target**, not a provider model name. `zeroclaw/<alias>` selects the target agent.

**Parsing rules**:

| `model` value | Routing result | Notes |
|---------------|---------------|-------|
| Missing / `""` | Default agent | `#[serde(default)]` converts to empty string; response normalizes to `"zeroclaw"` |
| `"zeroclaw"` | Default agent | Explicit default |
| `"zeroclaw/default"` | Default agent | Explicit default |
| `"zeroclaw/<alias>"` | `<alias>` | Routes to specified agent |
| `"zeroclaw:<alias>"` | `<alias>` | Compatible alias format |
| `"agent:<alias>"` | `<alias>` | Compatible alias format |
| `"zeroclaw/"` / `"zeroclaw:"` / `"agent:"` | 400 error | Empty alias, malformed |
| Plain label (e.g. `"gpt-4"`) | Default agent | **Standard client compatibility fallback** |

**Error handling**:

| Scenario | HTTP Status | Error Message |
|----------|------------|---------------|
| Agent doesn't exist | 400 | `Unknown agent 'xxx' -- no [agents.xxx] entry configured.` |
| Malformed agent target | 400 | `Invalid agent target 'zeroclaw/': missing agent alias` |
| Agent missing model_provider | 503 | `Agent not configured -- complete onboarding at /onboard` |

---

#### 3.2.2 messages (Message List) 🔑 Required

**Type**: `array[object]`
**Required**: ✅ Yes
**Constraint**: Array must not be empty

**Supported role types**:

| role | Description | Example |
|------|-------------|---------|
| `system` | System instruction (extracted as current-turn prefix, not persisted) | `"You are a helpful assistant"` |
| `developer` | Developer instruction (same as system) | `"You are a code assistant"` |
| `user` | User message | `"Hello!"` |
| `assistant` | Assistant response | `"Hi, how can I help?"` |
| `tool` | Tool call result | `{"role": "tool", "tool_call_id": "call_123", "content": "Sunny"}` |
| `function` | Function call result (auto-normalized to `tool`) | `{"role": "function", "content": "..."}` |

**Multi-turn message processing**:

1. Find the last `user`/`tool`/`function` message as the **active turn**
2. Remaining non-`system`/`developer` messages become **request history** (injected via `agent.seed_history()`)
3. `system`/`developer` messages extracted as **current-turn prefix** (prepended to user message, not persisted)
4. `function` role auto-normalized to `tool`

**Authoritative context rules**:

| `messages` length | Load backend history? | Use request message history? |
|-------------------|----------------------|------------------------------|
| `len() > 1` | No (request messages are authoritative) | Yes |
| `len() == 1` | Yes (if `x-session-key` present) | No |

---

#### 3.2.3 stream (Streaming Toggle)

**Type**: `boolean`
**Default**: `false`
**Required**: ❌ No

- `true` → SSE (Server-Sent Events) streaming response
- `false` → JSON non-streaming response (complete result at once)

---

#### 3.2.4 temperature (Sampling Temperature)

**Type**: `float`
**Default**: `0.7`
**Required**: ❌ No

- Controls output randomness; higher values = more random
- Range: `0.0` (deterministic) ~ `2.0` (highly random)

---

#### 3.2.5 Unsupported Parameters

The following parameters are **not supported** per-request and will return a **400 error** if provided:

| Parameter | Type | Error Message |
|-----------|------|---------------|
| `max_tokens` | integer | `max_tokens is not supported per-request; configure it in provider settings` |
| `top_p` | float | `top_p is not supported per-request` |
| `stop` | string/array | `stop is not supported per-request` |
| `presence_penalty` | float | `presence_penalty is not supported per-request` |
| `frequency_penalty` | float | `frequency_penalty is not supported per-request` |

**Why are these rejected?** These parameters are parsed from the request but the ZeroClaw runtime does not forward them to the underlying LLM provider. Rather than silently ignoring them (which could mislead callers into thinking they take effect), the endpoint returns an explicit 400 error with `error.type: "unsupported_parameter"`.

**Alternative**: To configure `max_tokens` for your agent, set it in the ZeroClaw provider configuration (e.g. `[providers.<type>.<alias>]` section).

---

#### 3.2.6 stream_options (Streaming Options)

**Type**: `object`
**Required**: ❌ No

- `include_usage: true` → Returns a `usage` chunk at the end of the SSE stream (with token statistics)
- Not set → No usage information in stream

---

#### 3.2.7 tools (Dynamic Tool Definitions)

**Type**: `array[object]`
**Required**: ❌ No

**Details**:
- **Purpose**: Dynamically specify the subset of tools available for this request (filtered from the agent's configured tools)
- **Transparent execution**: ZeroClaw uses automatic tool execution mode -- tool calls are transparent to the client
- **Filtering logic**: Only tools whose names match the agent's configured tools take effect. Any unknown tool name returns a 400 error (fail-closed); typo'd or unavailable tools are never silently ignored
- **Security**: Tools excluded by the agent's risk profile are automatically blocked, even if requested
- **Execution enforcement**: A tool outside the requested subset will never execute. If the model returns a call for a tool that is not available in this request, the call is rejected (the model receives an "Unknown tool"/"Tool not available" result and can self-correct on the next turn). Only the requested tools can actually run.
- **Model compatibility**: When the underlying model uses **native tool calling** (recommended), only the requested tool specs are sent as structured API parameters, and the system prompt tool catalog is suppressed. When the model uses **text-based tool protocol** (prompt-protocol), the system prompt may include descriptions for all agent-configured tools -- only the structured API parameters are narrowed. Tool execution is always restricted to the requested subset regardless of model type.

**Tool object structure**:
```json
{
  "type": "function",
  "function": {
    "name": "tool_name",
    "description": "Tool description",
    "parameters": {
      "type": "object",
      "properties": { ... },
      "required": [...]
    }
  }
}
```

---

#### 3.2.8 tool_choice (Tool Selection Strategy)

**Type**: `string` or `object`
**Default**: `auto`
**Required**: ❌ No

**Supported values**:

| Value | Description | ZeroClaw Behavior |
|-------|-------------|-------------------|
| `"auto"` | Model decides whether to call tools | Default behavior -- model judges based on the query |
| `"none"` | Disable all tools | No tool definitions are sent to the model and the system prompt excludes tool protocol. Any tool call in the response is ignored -- pure text conversation |

**Unsupported values** (return 400 `unsupported_parameter`):

| Value | Description |
|-------|-------------|
| `"required"` | Not yet wired through to providers -- the choice mode is never carried into the LLM request |
| `{"type":"function","function":{"name":"..."}}` | Not yet wired through to providers -- only the available-tool list would be narrowed |

**Key behavior notes**:
- `tool_choice: "none"` disables tools entirely: the system prompt excludes tool protocol, the LLM request carries no tool definitions, and any tool calls in the response are ignored -- the model produces a pure text answer
- When `tools` is provided with `"auto"`, only tools matching the agent's configured tool names are activated. If none of the requested tools are configured, a 400 error is returned
- **Out-of-scope tool calls**: If the model returns a call for a tool that is not available in this request (e.g. a tool not in the requested subset), the call is rejected and never executes -- the model receives a failure result it can use to self-correct on the next turn
- **Model compatibility**: For native-tool models, only the requested tool specs are sent as structured API parameters. For text-protocol (prompt) models, the system prompt may still include descriptions for all agent-configured tools -- tool execution is always restricted to the requested subset

---

## 4. Response Format

### 4.1 Non-streaming Response (JSON)

**Response structure**:
```json
{
  "id": "chatcmpl-a1b2c3d4-e5f6-7890-abcd-ef1234567890",
  "object": "chat.completion",
  "created": 1749560000,
  "model": "zeroclaw",
  "choices": [
    {
      "index": 0,
      "message": {
        "role": "assistant",
        "content": "Hello! How can I help?"
      },
      "finish_reason": "stop"
    }
  ],
  "usage": {
    "prompt_tokens": 10,
    "completion_tokens": 20,
    "total_tokens": 30
  }
}
```

**finish_reason**: Always `"stop"`. ZeroClaw uses **transparent tool execution** -- any tool calls triggered by the model are executed internally and only the final text answer is returned.

**tool_calls in non-streaming mode**: Always `null`. Tool execution is transparent to the client.

---

### 4.2 Streaming Response (SSE)

**SSE event types**:
1. **First frame**: `delta.role: "assistant"` + `delta.content: ""`
2. **Content chunks**: `delta.content` contains incremental text
3. **Final frame**: `delta: {}` + `finish_reason: "stop"`
4. **Usage chunk** (optional): Contains `usage` stats (requires `stream_options.include_usage: true`)
5. **End marker**: `data: [DONE]`

---

## 5. Error Handling

### 5.1 Error Response Format

```json
{
  "error": {
    "message": "Error description",
    "type": "error_type",
    "code": null,
    "param": null,
    "status": 400
  }
}
```

### 5.2 Common Error Types

| HTTP Status | error.type | Description | Solution |
|-------------|------------|-------------|----------|
| **400** | `invalid_request_error` | Invalid request | Check request parameter format |
| **400** | `invalid_request_error` | Unknown agent | Use an agent alias that exists in config |
| **400** | `invalid_request_error` | Malformed agent target | Use correct format (e.g. `zeroclaw/<alias>`, not empty alias) |
| **400** | `invalid_request_error` | Empty messages | Provide at least one message |
| **400** | `unsupported_parameter` | Unsupported per-request parameter | Remove `max_tokens`, `top_p`, `stop`, `presence_penalty`, or `frequency_penalty` from request |
| **401** | `authentication_error` | Auth failure | Provide a valid Bearer Token |
| **408** | `timeout_error` | Request timeout | The request took longer than the configured timeout. Reduce message complexity or increase the timeout in `[gateway] long_running_request_timeout_secs`. |
| **409** | `cross_transport_session_in_use` | Session conflict | An active WebSocket connection owns this session. Disconnect the WebSocket or use a different session key. |
| **413** | `payload_too_large` | Request body too large | Keep the request body under 64 KB. Large message histories should use `x-session-key` for multi-turn context instead. |
| **429** | `rate_limit_error` | Rate limited | Retry after wait (see `Retry-After` header) |
| **500** | `internal_error` | Internal server error | Contact admin, provide `X-Request-ID` |
| **503** | `server_error` | Agent not configured | Complete onboarding or check agent's model_provider config |

### 5.3 Error Examples

**400 -- unsupported parameter**:
```json
{
  "error": {
    "message": "max_tokens is not supported per-request; configure it in provider settings",
    "type": "unsupported_parameter",
    "code": null,
    "param": null,
    "status": 400
  }
}
```

**400 -- unknown agent**:
```json
{
  "error": {
    "message": "Unknown agent `nonexistent` -- no [agents.nonexistent] entry configured.",
    "type": "invalid_request_error",
    "code": null,
    "param": null,
    "status": 400
  }
}
```

**401 -- authentication failure**:
```json
{
  "error": {
    "message": "Invalid or missing authentication token",
    "type": "authentication_error",
    "code": null,
    "param": null,
    "status": 401
  }
}
```

**503 -- agent not configured**:
```json
{
  "error": {
    "message": "Agent not configured -- complete onboarding at /onboard",
    "type": "server_error",
    "code": null,
    "param": null,
    "status": 503
  }
}
```

**408 -- request timeout**:
```json
{
  "error": {
    "message": "Request exceeded the 600s timeout",
    "type": "timeout_error",
    "code": null,
    "param": null,
    "status": 408
  }
}
```

**409 -- session in use**:
```json
{
  "error": {
    "message": "This session is currently owned by an active WebSocket connection. Disconnect the WebSocket or use a different session key.",
    "type": "cross_transport_session_in_use",
    "code": null,
    "param": null,
    "status": 409
  }
}
```

**413 -- payload too large**:
```json
{
  "error": {
    "message": "Request body exceeds the 65536 byte limit",
    "type": "payload_too_large",
    "code": null,
    "param": null,
    "status": 413
  }
}
```

---

## 6. Response Headers

| Header | Description | Condition |
|--------|-------------|-----------|
| `Content-Type` | Response content type | Always (`application/json` or `text/event-stream`) |
| `X-Request-ID` | Server-generated unique request identifier (UUID). The client-supplied `x-request-id` header is not read or echoed. | Always |
| `x-session-key` | Session ID (without `gw_` prefix) | **Always returned** (generated server-side if not provided by client) |
| `X-RateLimit-Limit` | Max requests per minute (config: `chat_rate_limit_per_minute`) | On success & 429 |
| `X-RateLimit-Remaining` | Remaining request allowance | On success & 429 |
| `X-RateLimit-Reset` | Rate limit reset Unix timestamp | On success & 429 |
| `Retry-After` | Retry wait seconds on 429 (fixed 60s window) | Only on 429 |

---

## 7. Practical Scenarios

### Scenario 1: Single-turn Conversation (Non-streaming)

```bash
curl -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "",
    "messages": [
      {"role": "user", "content": "Hello, please introduce yourself"}
    ]
  }'
```

---

### Scenario 2: Multi-turn with Session (Single message auto-loads history)

```bash
#!/bin/bash

SESSION_ID=$(cat /proc/sys/kernel/random/uuid)
echo "Session ID: $SESSION_ID"

# Turn 1: User introduces themselves
echo "=== Turn 1 ==="
curl -sS -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "x-session-key: $SESSION_ID" \
  -d '{
    "model": "",
    "messages": [
      {"role": "user", "content": "The zeroclaw_project codename is comet."}
    ]
  }' | jq '.choices[0].message.content'

# Turn 2: Ask about stored state (single message auto-loads backend history)
echo "=== Turn 2 ==="
curl -sS -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "x-session-key: $SESSION_ID" \
  -d '{
    "model": "",
    "messages": [
      {"role": "user", "content": "What is the project codename?"}
    ]
  }' | jq '.choices[0].message.content'
# Response will include "comet"
```

---

### Scenario 3: Multi-turn with Full History (Request messages as authoritative context)

```bash
curl -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "",
    "messages": [
      {"role": "system", "content": "You are a friendly assistant."},
      {"role": "user", "content": "The zeroclaw_project codename is comet."},
      {"role": "assistant", "content": "Noted. The codename is comet."},
      {"role": "user", "content": "What is the project codename?"}
    ]
  }'
# system message auto-extracted as current-turn prefix (not persisted)
# Request messages used as authoritative context, no backend history loaded
```

---

### Scenario 4: Streaming Output

```bash
#!/bin/bash

SESSION_ID=$(cat /proc/sys/kernel/random/uuid)

curl -N -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -H "x-session-key: $SESSION_ID" \
  -d '{
    "model": "",
    "stream": true,
    "messages": [{"role": "user", "content": "Write a poem about spring"}]
  }' | while read -r line; do
    if [[ $line == data:* ]]; then
      content=$(echo "$line" | sed 's/^data: //' | jq -r '.choices[0].delta.content // empty')
      if [[ -n $content ]]; then
        echo -n "$content"
      fi
    fi
  done
```

---

### Scenario 5: Specify Agent Routing

```bash
# Use model field to specify agent (zeroclaw/<alias> format)
curl -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "zeroclaw/coding",
    "messages": [{"role": "user", "content": "Write a quicksort"}]
  }'

# Standard client compatibility: any model name routes to default agent
curl -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "gpt-4",
    "messages": [{"role": "user", "content": "Hello"}]
  }'
```

---

### Scenario 6: Streaming + Usage Statistics

```bash
curl -N -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "",
    "stream": true,
    "stream_options": {"include_usage": true},
    "messages": [{"role": "user", "content": "Hello"}]
  }'
# A usage chunk appears at the end:
# data: {"id":"chatcmpl-...","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}
```

---

### Scenario 8: Disable All Tools

```bash
# tool_choice: "none" disables all tools for this request
# The LLM receives no tool definitions and no tool protocol in the system prompt
curl -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "",
    "messages": [{"role": "user", "content": "Hello, please introduce yourself"}],
    "tool_choice": "none"
  }'
```

---

### Scenario 9: Restrict to Specific Tools

```bash
# Only expose a subset of tools for this request
curl -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "",
    "messages": [{"role": "user", "content": "What is the weather in Beijing?"}],
    "tools": [
      {
        "type": "function",
        "function": {
          "name": "weather_query",
          "description": "Query weather for a city",
          "parameters": {
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"]
          }
        }
      }
    ]
  }'
# Only "weather_query" is available (must match agent's configured tools)
# Tools not in the agent's configuration return a 400 error (fail-closed)
```

---

### Scenario 10: Force a Specific Tool

`tool_choice` with a specific function object is **not yet supported** -- it returns a 400 error. Use `"auto"` with a `tools` list to restrict available tools instead (see Scenario 9).

---

## 8. FAQ

### Q1: Why are tool_calls always null in non-streaming mode?

**Answer**: ZeroClaw uses **transparent tool execution** -- tool calls are executed automatically by the backend, and the client only receives the final text response. In non-streaming mode, `tool_calls` is always `null` and `finish_reason` is always `"stop"`.

- **Non-streaming**: `finish_reason` is always `"stop"`, `tool_calls` is always `null`
- **Streaming**: Tool execution is transparent; no client-actionable `tool_calls` chunks are emitted (see Q2)

---

### Q2: Why doesn't finish_reason return "tool_calls"?

**Answer**: ZeroClaw tools are transparently executed server-side. Unlike the OpenAI API -- where `finish_reason: "tool_calls"` means the *client* must execute the tool and call back -- ZeroClaw has already completed the tool loop before returning. The client always receives the final text answer with `finish_reason: "stop"`.

---

### Q3: What is the model parameter routing logic?

**Answer**: The `model` field is an **agent routing target**, not a provider model name:

```
model = "zeroclaw/<alias>"  → Routes to specified agent
model = "" / "zeroclaw" / plain label (e.g. "gpt-4") → Routes to default agent
model = "zeroclaw/" (empty alias) → 400 error
```

---

### Q4: Are system prompts persisted in multi-turn conversations?

**Answer**: No. `system` and `developer` messages are extracted as a prefix for the current turn (prepended to the user message), but are **not persisted to the session backend**. This prevents subsequent turns from being polluted by repeated system prompts.

---

### Q5: What is the difference between providing multiple messages vs using x-session-key?

**Answer**:

| Approach | Behavior |
|----------|----------|
| `messages` length > 1 | Request messages are **authoritative context**; backend history is not loaded; non-system/developer history injected via `seed_history()` |
| `messages` length == 1 + `x-session-key` | Backend session history is auto-loaded; suitable for turn-by-turn conversation |

Both approaches can be mixed depending on the use case.

---

### Q6: Why are usage stats sometimes all zeros?

**Answer**: Token usage is collected via **cost tracking**. If:
- No cost tracker is configured (`cost_tracker` is empty)
- The provider did not return token counts

Then all `usage` fields will be 0. This does not affect conversation functionality.

---

### Q7: Why do max_tokens, top_p, stop, presence_penalty, frequency_penalty return 400?

**Answer**: These parameters are not supported per-request by the ZeroClaw runtime. Rather than silently ignoring them (which would mislead callers into thinking they take effect), the endpoint returns an explicit 400 error with `error.type: "unsupported_parameter"`.

**Alternative**: Configure `max_tokens` in your ZeroClaw provider settings (e.g. `[providers.<type>.<alias>]`).

---

### Q8: How does tool_choice: "none" work?

**Answer**: `tool_choice: "none"` disables tools entirely:
1. **System prompt**: Tool protocol descriptions are excluded from the system prompt
2. **LLM request**: No tool definitions are sent to the model
3. **Response**: No tool definitions were sent, so the model has nothing to call -- any tool call in the response is ignored and a pure text answer is returned

This ensures the LLM behaves as a pure text model with no tool capability for that request.

---

### Q9: How does the tools parameter filter work?

**Answer**: The `tools` parameter filters against the agent's currently configured tools:
- Only tool names present in `agent.get_configured_tool_names()` are activated
- Tools excluded by the agent's risk profile are automatically blocked (they are removed from `configured_tools` at agent build time)
- Any unknown tool name returns a 400 error (fail-closed)
- If none of the requested tools are configured, a 400 error is returned (fail-closed)

**Model compatibility note**: For **native-tool models**, only the requested tool specs are sent as structured API parameters, and the system prompt tool catalog is suppressed. For **text-protocol models**, the system prompt may still include descriptions for all agent-configured tools -- the prompt-level catalog is not scoped. Tool execution is always restricted to the requested subset regardless of model type.

---

### Q10: Where is the rate limit configured?

**Answer**: In the ZeroClaw config file:

```toml
[gateway]
chat_rate_limit_per_minute = 10  # Max requests per minute, default 60
```

Exceeding the limit returns 429 with a `Retry-After` header indicating the wait time (60-second window).

---

### Q11: Does the tools parameter scope the system prompt tool descriptions?

**Answer**: It depends on the model type:
- **Native-tool models** (recommended): Yes -- only the requested tool specs are sent as structured API parameters, and the system prompt tool catalog is suppressed
- **Text-protocol (prompt) models**: No -- the system prompt may still include descriptions for all agent-configured tools. The `tools` parameter only narrows the structured API parameters and the response-parsing allow-set. Tool execution is always restricted to the requested subset

---

### Q12: What is the overall tool control strategy?

**Answer**: Tool control is enforced at three layers that work together:

1. **Request validation** -- The handler validates `tool_choice` and `tools` combinations before the turn starts. Invalid or unavailable tool configurations return a 400 error (fail-closed), never silently falling back to the full tool set.
2. **Provider-visible specs** -- Only the requested tool specs are sent to the LLM as structured API parameters. For `tool_choice: "none"`, no tool definitions are sent at all. For native-tool models, this is the primary lever: only the requested tools are sent as structured API parameters. For text-protocol models, the system prompt may still include descriptions of all agent-configured tools; see the model-compatibility notes in §3.2.7.
3. **Execution enforcement** -- If the model still returns a call for a tool outside the requested subset, that call is rejected and never executes. The model receives a failure result ("Unknown tool" / "Tool not available") it can use to self-correct on the next turn.

This ensures a tool outside the requested subset can never run, regardless of what the model returns. Only the requested tools can actually execute.

---

## 9. Custom Session Backend Compatibility

ZeroClaw supports pluggable session backends via the `SessionBackend` trait
(`crates/zeroclaw-infra/src/session_backend.rs`). If you implement a custom
backend, be aware of the following compatibility requirements introduced
by cross-agent session isolation:

| Method | Default behavior | Impact if not implemented |
|--------|-----------------|--------------------------|
| `set_session_agent_alias` | `Err(Unsupported)` | **Non-empty sessions:** HTTP 400 (`"Cannot resume session: backend does not track agent ownership"`). **Empty sessions:** Turn proceeds without ownership tracking -- cross-agent isolation is absent. |
| `get_session_agent_alias` | `Err(Unsupported)` | Same as above -- the handler calls this at history-load time and rejects non-empty sessions if unsupported. |

**To support multi-agent deployments**, implement both methods to persist
and retrieve the agent alias associated with each session key. The JSONL
backend (`SessionStore`) uses a `.meta.json` sidecar file for this purpose
as a reference implementation.

**For single-agent deployments** that don't need ownership enforcement,
implement both methods as no-ops:

```rust
fn set_session_agent_alias(&self, _key: &str, _alias: &str) -> std::io::Result<()> {
    Ok(())
}
fn get_session_agent_alias(&self, _key: &str) -> std::io::Result<Option<String>> {
    Ok(None)
}
```

Returning `Ok(None)` from `get_session_agent_alias` tells the handler the
session has no recorded owner, which is accepted for **empty** sessions
but rejected for **non-empty** sessions (to prevent cross-agent history
leakage). See the `SessionBackend` trait documentation in
`crates/zeroclaw-infra/src/session_backend.rs` for the full contract,
including migration guidance for backends with pre-existing session data.

---

## 10. Best Practices

### 1. Session Management

- **Client-generated session ID**: Especially for streaming, generate a UUID as the `x-session-key` header value
- **Reuse sessions**: Use the same `x-session-key` for multi-turn conversations (with single-message mode)
- **Full history for stateless clients**: Provide complete message history in `messages` (> 1 entry), no session needed
- **Avoid excessive history**: Long histories increase token consumption and response latency

### 2. Agent and Model Usage

- **Route via model**: Use `"zeroclaw/<alias>"` format to specify target agent
- **Standard client compatibility**: Front-ends like Open WebUI / LobeChat can pass any model name (auto-routes to default agent)
- **No need to know provider model names**: Clients only need agent aliases

### 3. Streaming vs Non-streaming

- **Long text / real-time**: Use `stream: true` for live output
- **Short Q&A / API integration**: Use `stream: false` for complete responses
- **Need usage stats**: Set `stream_options.include_usage: true` in streaming mode

### 4. Multi-turn Conversation Options

- **Simple conversations**: Use `x-session-key` + single message, rely on backend auto-load
- **Precise context control**: Provide full `messages` array (> 1 entry) for consistent context
- **System prompts**: Pass via `system` role messages; they don't affect session persistence
- **Cross-transport sessions**: A WebSocket connection owns the session transcript for its lifetime. The agent's in-memory history, including structured tool-call and tool-result entries, is preserved across turns within a single connection. HTTP chat completions requests on a session key held by an active WebSocket return 409 Conflict. After the WebSocket disconnects, the backend retains the last persisted state, which a new connection can load as its starting point.

### 5. Tool Control

- **Disable tools**: Use `tool_choice: "none"` for pure text conversations
- **Restrict tools**: Use `tools` array to limit available tools for a request
- **Restrict available tools**: Use `tools` array + `tool_choice: "auto"` to narrow the tool set for a request
- **Security**: Tools excluded by the agent's risk profile are automatically blocked

### 6. Performance Optimization

- **Limit tool count**: Only provide necessary tools per request (reduces token consumption)
- **Set temperature appropriately**: `0.7` for general chat, `1.0` for creative writing, `0.2` for code generation
- **Respect rate limits**: Control request frequency per `chat_rate_limit_per_minute`

### 7. Security

- **Authentication**: Enable `require_pairing=true` in production
- **Principle of least privilege**: Only enable necessary tools
- **Audit logging**: Enable observability to record tool call history
- **Unsupported parameters**: Do not send `max_tokens`, `top_p`, etc. -- they will cause 400 errors

---

## 11. Changelog

### v1.0 (2026-06-30) -- Initial Stable Release

**Core features**:
- ✅ OpenAI Chat Completions API compatible endpoint (`POST /v1/chat/completions`)
- ✅ `model` field as **agent routing target** (`"zeroclaw/<alias>"`), not provider model name
- ✅ Plain label model names auto-route to default agent (standard client compatibility)
- ✅ Multi-turn message splitting (system/developer prefix, history injection, active turn extraction)
- ✅ Session management via `x-session-key` with auto-load for single-message requests
- ✅ Streaming (SSE) and non-streaming (JSON) response modes
- ✅ `temperature` parameter supported per-request

**Tool control** (newly effective in v1.0):
- ✅ `tool_choice: "none"` -- Fully disables tools: system prompt excludes tool protocol, LLM request omits tool definitions, model tool calls discarded
- ✅ `tools: [...]` -- Restricts available tools to the requested subset (filtered against agent's configured tools; security policy enforced)
- ✅ `tool_choice: "auto"` -- Standard OpenAI semantics (default)
- ⛔ `tool_choice: "required"` -- Returns 400 (not yet wired through to providers)
- ⛔ `tool_choice: {"type":"function",...}` -- Returns 400 (not yet wired through to providers)

**Unsupported parameter rejection**:
- ⛔ `max_tokens`, `top_p`, `stop`, `presence_penalty`, `frequency_penalty` return **400 `unsupported_parameter`** errors
- These are rejected explicitly rather than accepted and ignored, preventing caller confusion

**Tool control fail-closed**:
- ✅ Unknown tools, missing required tool lists, and empty filtered tool sets return 400 errors (fail-closed, never silently falling back to the full tool set)
- ✅ Native tool calls outside the request's allowed set reach the execution layer and are rejected there ("Unknown tool" / "Tool not available")
