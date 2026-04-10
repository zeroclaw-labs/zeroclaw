# ACP-over-HTTP Server Support for Zeroclaw

## Goal

Make zeroclaw a standards-compliant ACP (Agent Communication Protocol) server so one zeroclaw instance (Sam) can delegate tasks to another zeroclaw instance (the k8s agent) using the same `acp-client` tool currently used for Goose. This replaces Goose, which suffers from stateless sessions that waste tokens rediscovering context every turn.

## Architecture Decision

**Approach: ACP protocol endpoint in the gateway** — Add a `/acp` route to zeroclaw's existing gateway HTTP server that speaks JSON-RPC 2.0 over HTTP+SSE, matching the protocol spec that `acp-client.py` already implements. This means:

- Sam's existing `acp-client send/poll/result` workflow works unchanged
- The new zeroclaw gets persistent memory (Serena), conversation history, and the full tool loop
- No new binaries or sidecars needed — it's a gateway feature toggled by config

### Why not simpler alternatives?

| Alternative | Problem |
|---|---|
| Just use `/api/chat` | Synchronous — blocks until completion, same timeout issue as Goose |
| Add async mode to `/api/chat` | Custom protocol; `acp-client` would need rewriting |
| Stdio ACP channel (existing) | Requires spawning a subprocess; doesn't work for pod-to-pod HTTP |

## Protocol Summary (from acp-client.py)

The ACP protocol uses JSON-RPC 2.0 messages over HTTP. Responses stream via SSE (Server-Sent Events).

### Transport

- **Endpoint**: `POST /acp`
- **Session binding**: `Acp-Session-Id` header (returned by `initialize`, sent on all subsequent requests)
- **Response format**: SSE stream — `data: {json}\n\n` lines, each containing a JSON-RPC message
- **Final message**: Contains `"result"` key (vs notifications which have `"params"`)

### Lifecycle

```
initialize          → returns protocol info + Acp-Session-Id header
session/new         → creates agent session, returns sessionId
session/prompt      → sends user message, streams tool activity + response via SSE
(session can receive multiple prompts sequentially)
DELETE /acp         → tears down transport session
```

### Request Shape

```json
{
  "jsonrpc": "2.0",
  "method": "initialize|session/new|session/prompt",
  "id": 1,
  "params": { ... }
}
```

### Notification Types (streamed during session/prompt)

- `{"params": {"update": {"content": {"text": "..."}}}}`  — LLM text output
- `{"params": {"update": {"tool_name": "...", "status": "running|complete"}}}`  — tool execution
- `{"params": {"update": {"chunk": "..."}}}`  — streaming text delta

---

## Implementation Plan

### Phase 1: ACP Config & Routing 🟩

**Files**: `src/config/schema.rs`, `src/gateway/mod.rs`

- [ ] Add `AcpServerConfig` to config schema:
  ```rust
  pub struct AcpServerConfig {
      pub enabled: bool,           // default: false
      pub allowed_senders: Vec<String>,  // optional allowlist
  }
  ```
  Add as `pub acp_server: AcpServerConfig` under `GatewayConfig`.

- [ ] Register `/acp` route in gateway router:
  ```rust
  .route("/acp", post(acp_server::handle_acp).delete(acp_server::handle_acp_delete))
  ```
  Use a generous body limit (same as `/v1/chat/completions`) and long timeout.

- [ ] Gate the route behind `config.gateway.acp_server.enabled`.

### Phase 2: Transport Session Management 🟩

**Files**: `src/gateway/acp_server.rs` (new)

- [ ] Create `AcpTransportSession` struct:
  ```rust
  struct AcpTransportSession {
      id: String,                                    // UUID
      agent_session_id: Option<String>,              // conversation history key
      created_at: Instant,
  }
  ```

- [ ] Create `AcpSessionStore` (shared state in gateway):
  ```rust
  struct AcpSessionStore {
      sessions: Mutex<HashMap<String, AcpTransportSession>>,
  }
  ```
  Add to `GatewayState`. Include TTL-based cleanup (reuse idempotency store pattern).

- [ ] Implement `initialize` handler:
  - Validate `protocolVersion`
  - Create transport session, store in `AcpSessionStore`
  - Return `Acp-Session-Id` header + JSON-RPC result with server info

- [ ] Implement `session/new` handler:
  - Require valid `Acp-Session-Id`
  - Generate a conversation history key (e.g., `acp:{transport_session_id}`)
  - Store in transport session
  - Return `{"sessionId": "..."}`

- [ ] Implement `DELETE /acp` handler:
  - Remove transport session from store
  - Optionally clear conversation history

### Phase 3: Prompt Execution (Core) 🟩

**Files**: `src/gateway/acp_server.rs`

This is the main feature — `session/prompt` runs the full agent tool loop and streams results.

- [ ] Implement `session/prompt` handler:
  1. Look up transport session by `Acp-Session-Id` header
  2. Look up or create conversation history by `agent_session_id`
  3. Build system prompt (reuse `build_system_prompt()` from channels)
  4. Append user message to history
  5. Create SSE response body (axum `Sse` type)
  6. Spawn agent loop task:
     - Call `run_tool_call_loop()` with:
       - `on_delta: Some(tx)` for streaming text
       - `injection_rx: None` (or wire up if desired)
       - Full tools registry
     - Stream tool notifications and text deltas as SSE events
     - On completion, send final `result` SSE event
  7. Return SSE stream immediately (non-blocking for caller)

- [ ] Map tool loop events to ACP notification format:
  ```rust
  // Text delta → content notification
  {"jsonrpc":"2.0","method":"notifications/update","params":{"update":{"content":{"text":"..."}}}}

  // Tool start → tool notification
  {"jsonrpc":"2.0","method":"notifications/update","params":{"update":{"tool_name":"shell","status":"running"}}}

  // Final response → result
  {"jsonrpc":"2.0","id":N,"result":{"content":[{"type":"text","text":"full response"}]}}
  ```

- [ ] Handle conversation history persistence:
  - Use `ConversationHistoryMap` from `ChannelRuntimeContext` (or a parallel map for ACP sessions)
  - History persists across multiple `session/prompt` calls within the same agent session
  - This is the key advantage over Goose: the second prompt sees the first prompt's context

### Phase 4: Auth & Rate Limiting 🟩

**Files**: `src/gateway/acp_server.rs`, `src/gateway/mod.rs`

- [x] Reuse gateway's existing Bearer token auth (pairing system)
- [x] Apply `rate_limiter.allow_webhook()` to `/acp` endpoint
- [ ] Optional: `allowed_senders` config restricts which Bearer tokens can use ACP

### Phase 5: Integration with acp-client 🟥

**Files**: acp-client.py ConfigMap (scrapyard-applications repo)

- [ ] Verify `acp-client` works against zeroclaw's `/acp` endpoint by:
  - Pointing `ACP_BASE_URL` to the new zeroclaw instance's service
  - Running `acp-client health`, `send`, `poll`, `result` sequence
- [ ] No changes to acp-client should be needed if the protocol is implemented correctly
- [ ] Update Sam's TOOLS.md to point acp-client at the new endpoint

### Phase 6: Testing 🟨

- [x] Unit tests for `AcpSessionStore` (create, lookup, TTL expiry, delete)
- [x] Unit tests for JSON-RPC message parsing and serialization
- [ ] Integration test: `initialize` → `session/new` → `session/prompt` → verify SSE stream contains expected notification types and final result
- [ ] Integration test: multiple prompts on same session share conversation history
- [ ] Integration test: auth rejection without Bearer token
- [ ] Integration test: transport session cleanup on DELETE

### Phase 7: Deployment 🟨

**Files**: scrapyard-applications repo

- [ ] Create new Sandbox manifest for the k8s agent zeroclaw (separate from Sam's)
  - Different ConfigMap with k8s-focused identity files
  - Serena sidecar for persistent memory
  - kubectl + kubeconfig available (unlike Sam's container)
  - Gateway with `acp_server.enabled = true`
- [ ] Create k8s Service for the new zeroclaw's gateway port
- [ ] Update Sam's `acp-client` `ACP_BASE_URL` to point to the new service
- [ ] Remove or retire the Goose ACP deployment

---

## Key Design Decisions

### Conversation History Scoping

Each ACP transport session gets its own conversation history key (`acp:{session_id}`). Multiple `session/prompt` calls within the same transport session build on the same history. This directly solves the Goose problem — Sam can send a task, get a result, then send a follow-up that builds on context.

For even longer persistence, the k8s agent uses Serena memories (cross-session) just like Sam does.

### SSE vs Polling

The ACP protocol uses SSE for streaming, but `acp-client` also supports a polling mode (writes to local files, polls status). The SSE stream from `/acp` satisfies both:
- **Blocking mode** (`acp-client send-sync`): reads SSE stream directly
- **Async mode** (`acp-client send` + `poll`): acp-client spawns a background process that reads SSE and writes to status files

### Tool Access

The k8s agent zeroclaw has the full tool registry. For the k8s use case, the important tools are:
- `shell` (for kubectl, helm, git)
- `file_read/write/edit` (for manifest files)
- Serena tools (for code intelligence on scrapyard-applications repo + persistent memory)
- `glob_search`, `content_search` (for finding manifests)

### What We're NOT Building

- No custom transport protocol — we're implementing the existing ACP spec
- No multi-agent mesh — this is point-to-point (Sam → k8s agent)
- No bidirectional communication — the k8s agent doesn't initiate conversations with Sam
- No shared memory between agents — each has its own Serena instance

---

## Progress

🟥 Not started | 🟨 In progress | 🟩 Complete

| Phase | Status | Notes |
|-------|--------|-------|
| 1. Config & Routing | 🟩 | `AcpServerConfig` in schema, `/acp` route registered |
| 2. Transport Sessions | 🟩 | `AcpSessionStore` with TTL, create/get/update/remove |
| 3. Prompt Execution | 🟩 | `process_message_with_history` + SSE streaming |
| 4. Auth & Rate Limiting | 🟩 | Pairing auth + rate limiting reused from gateway |
| 5. acp-client Integration | 🟨 | acp-client defaults updated, needs live test |
| 6. Testing | 🟩 | 18 unit tests pass (store, handlers, lifecycle, isolation) |
| 7. Deployment | 🟨 | Manifests created, Docker build in progress, Vault secret pending |

**Overall: ~85%**

## Manual Steps Required

Before deploying, the user must:

1. **Create Vault secret** at `kvv2/zeroclaw/zeroclaw-k8s-agent` with keys:
   - `api_key` — LiteLLM proxy API key (same as Sam's)
   - `gateway-paired-tokens` — pre-shared token for ACP auth (generate new)
   - `git-token` — Gitea access token for cloning scrapyard-applications

2. **Push Docker image** `citizendaniel/zeroclaw:feat-proactive-messaging` to registry

3. **Apply manifests** in order:
   ```bash
   kubectl apply -f 04_zeroclaw_k8s_agent/00_vso_vault.yaml
   kubectl apply -f 04_zeroclaw_k8s_agent/01_configmap.yaml
   kubectl apply -f 04_zeroclaw_k8s_agent/02_identity_configmap.yaml
   kubectl apply -f 04_zeroclaw_k8s_agent/03_sandbox.yaml
   kubectl apply -f 04_zeroclaw_k8s_agent/04_service.yaml
   kubectl apply -f 04_zeroclaw_k8s_agent/05_networkpolicy.yaml
   kubectl apply -f 04_zeroclaw_k8s_agent/06_cilium_kube_api_policy.yaml
   ```

4. **Update Sam's acp-client** — apply updated ConfigMap:
   ```bash
   kubectl apply -f 03_goose_acp/04_acp_client_configmap.yaml
   ```

5. **Update Sam's identity** — apply updated TOOLS.md:
   ```bash
   kubectl apply -f ../32_zeroclaw/05_zeroclaw_identity_configmap.yaml
   ```

6. **Pair Sam with the new agent** — Sam needs to obtain a Bearer token from the k8s agent gateway via the pairing flow, then configure it as `ACP_AUTH_TOKEN` env var or pass in acp-client calls.

7. **Retire Goose** (optional) — once verified, scale down goose-acp Sandbox
