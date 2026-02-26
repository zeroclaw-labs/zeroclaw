# ZeroClaw Architecture Deep Dive

> Full reverse-engineering of the ZeroClaw autonomous agent runtime.
> 119,763 LOC | 243 source files | 38 modules | 4,989 tests | 3.4MB binary

---

## System Overview

ZeroClaw is a Rust autonomous agent runtime that connects LLM providers to messaging channels with tool execution capabilities, wrapped in a consciousness-inspired cognitive architecture. It runs as a CLI binary or system daemon.

```
┌─────────────────────────────────────────────────────────────────────┐
│                        CLI / Daemon (main.rs)                       │
│                         16 commands, Clap v4                        │
├──────────┬──────────┬──────────┬──────────┬──────────┬──────────────┤
│ Channels │ Providers│  Tools   │  Memory  │  Gateway │   Security   │
│ 14 impls │ 30+ impl│ 35+ impl │ 5 backnd │  Axum    │  Sandbox     │
│          │          │          │          │  REST    │  Pairing     │
│          │          │          │          │  Webhook │  Audit       │
├──────────┴──────────┴──────────┴──────────┴──────────┴──────────────┤
│                     Agent Orchestration Loop                        │
│              Agent::turn() | run_tool_call_loop()                   │
├─────────────────────────────────────────────────────────────────────┤
│                      Cosmic Brain (19 subsystems)                   │
│  Thalamus │ Workspace │ Modulator │ FreeEnergy │ Drift │ Causal    │
│  SelfModel│ WorldModel│ Normative │ Policy     │ Gate  │ Memory    │
│  Consolidation │ Integration │ Constitution │ AgentPool │ Counter  │
├──────────┬──────────┬──────────┬──────────┬──────────┬──────────────┤
│   Soul   │Conscience│Continuity│  Tunnel  │  Wallet  │ Peripherals │
│ Survival │  Ethics  │ Identity │ CF/ngrok │  EVM     │ STM32/GPIO  │
│ Strategy │  Ledger  │ Prefs    │ Tailscale│  x402    │ USB/Serial  │
└──────────┴──────────┴──────────┴──────────┴──────────┴──────────────┘
```

---

## Module Inventory (by LOC)

| Module | LOC | Files | Purpose |
|--------|-----|-------|---------|
| `channels/` | 17,600 | 17 | 14 messaging platform transports |
| `tools/` | 15,700 | 40 | 35+ agent action tools |
| `providers/` | 11,900 | 13 | 30+ LLM provider integrations |
| `config/` | 8,500 | 5 | Schema, loading, merging, validation |
| `cosmic/` | 7,000 | 18 | Consciousness-inspired cognitive arch |
| `memory/` | 6,600 | 14 | Pluggable memory backends |
| `gateway/` | 4,500 | 4 | HTTP API + webhook server |
| `agent/` | 4,214 | 4 | Core orchestration loop |
| `security/` | 3,800 | 8 | Policy, pairing, sandbox, audit |
| `auth/` | 3,200 | 5 | OAuth2 PKCE, device code, token mgmt |
| `soul/` | 2,200 | 9 | Survival economics, model strategy |
| `continuity/` | 1,600 | 10 | Cross-session identity persistence |
| `tunnel/` | 1,400 | 5 | Cloudflare/ngrok/Tailscale tunnels |
| `peripherals/` | 1,200 | 6 | Hardware board integration |
| `conscience/` | 817 | 5 | Ethical governance + integrity ledger |
| Others | ~30K | ~80 | cron, rag, wallet, health, service, etc. |

---

## 14 Core Traits

| Trait | File | Required Methods | Implementations |
|-------|------|-----------------|-----------------|
| `Provider` | `providers/traits.rs` | `chat_with_system()` | 30+ (8 native + 22 OpenAI-compat) |
| `Channel` | `channels/traits.rs` | `name()`, `send()`, `listen()` | 14 |
| `Tool` | `tools/traits.rs` | `name()`, `description()`, `parameters_schema()`, `execute()` | 35+ |
| `Memory` | `memory/traits.rs` | `store()`, `recall()`, `get()`, `list()`, `forget()` | 5 |
| `Observer` | `observability/traits.rs` | `record_event()`, `record_metric()`, `name()` | 3 |
| `RuntimeAdapter` | `runtime/traits.rs` | `execute_command()`, `read_file()`, `write_file()` | 3 (Native/Docker/WASM) |
| `Peripheral` | `peripherals/traits.rs` | board/pin/tool interface | STM32, RPi GPIO |
| `Sandbox` | `security/traits.rs` | `wrap_command()`, `is_available()` | 5 (Landlock/Firejail/Bubblewrap/Docker/Noop) |
| `Tunnel` | `tunnel/mod.rs` | `start()`, `stop()`, `health_check()` | 4 |
| `EmbeddingProvider` | `memory/embeddings.rs` | `embed()`, `dimensions()` | 2 (OpenAI/Noop) |
| `Scout` | `skillforge/scout.rs` | skill discovery | GitHub/ClawHub/HuggingFace |
| `ToolDispatcher` | `agent/` | XML + Native dispatch | 2 |
| `PromptSection` | `agent/` | system prompt assembly | 8 sections |
| `MemoryLoader` | `memory/` | snapshot hydration | 1 |

---

## Core Architecture: Agent Loop

### Dual Loop Architecture

**Simple loop** — `Agent::turn()`:
- Single LLM call → optional tool calls → response
- No CosmicBrain, no streaming, no history management
- Used by: `delegate` tool (sub-agents), simple CLI mode

**Full loop** — `run_tool_call_loop()` (4,214 LOC):
- Multi-turn with conversation history
- CosmicBrain integration (all 19 subsystems)
- Streaming support, typing indicators, draft updates
- Tool call parsing (XML + native), execution, result injection
- 300s timeout, credential scrubbing in responses

### Agent Struct (18 fields)
```rust
struct Agent {
    config, provider, tools, memory, observer, runtime,
    system_prompt, model, temperature, max_history_tokens,
    security, cosmic_brain, approval_manager, delegate_agents,
    sandbox, personality, conversation_history, parallel_tools
}
```

### CosmicBrain Struct (19 Arc<Mutex<T>> fields)
```rust
struct CosmicBrain {
    modulator, thalamus, workspace, self_model, world_model,
    consolidation, drift, free_energy, causal, integration,
    normative, policy, counterfactual, agent_pool, constitution,
    gate, memory_graph, persistence, config
}
```

Built in `CosmicBrain::build(config)` — reads persistence dir from config, optionally wires encryption via `SecretStore`.

---

## Channels Layer (14 Implementations)

| Channel | Transport | Auth | Allowlist | Special |
|---------|-----------|------|-----------|---------|
| CLI | stdin/stdout | none | none | Always available |
| Telegram | Bot API polling | bot_token | usernames | Voice, streaming drafts, /bind, media markers |
| Discord | Gateway WebSocket | bot_token | user IDs | Mention-only mode, JWT bot ID extraction |
| Slack | Web API polling | bot_token | user IDs | Self-message filtering |
| WhatsApp | Meta Cloud API | access_token | phone numbers | Webhook verification |
| iMessage | macOS osascript | none (local) | contacts | macOS only |
| IRC | TCP/TLS | server/nickserv/sasl | usernames | Multiple auth methods |
| Matrix | Client-Server API | access_token | user IDs | Federation support |
| Signal | signal-cli HTTP | account | phone numbers | Bridge-based |
| Email | IMAP/SMTP | credentials | addresses | Subject field support |
| Lark | Feishu API | app_id/secret | user IDs | Chinese enterprise |
| DingTalk | Stream API | client_id/secret | user IDs | Chinese enterprise |
| QQ | Guild API | app_id/secret | user IDs | Chinese consumer |
| Mattermost | WebSocket | bot token | allowlist | Self-hosted Slack alt |

### Message Flow
```
Channel.listen(tx) → mpsc → run_message_dispatch_loop()
  → Semaphore(max_in_flight) → spawn task per message
    → parse_runtime_command() → route selection → build_memory_context()
    → spawn_typing_task() → run_tool_call_loop() → send response
```

### Security: Fail-closed allowlist checked inside `listen()` before messages reach the agent.

---

## Providers Layer (30+ Implementations)

### Native Providers
Anthropic, OpenAI, Gemini, Ollama, OpenRouter, Copilot, OpenAI Codex, GLM

### OpenAI-Compatible (via `OpenAiCompatibleProvider`)
Venice, Vercel AI, Cloudflare AI, Moonshot, Kimi Code, Synthetic, Z.AI, MiniMax, Amazon Bedrock, Qianfan, Qwen/DashScope, Groq, Mistral, xAI/Grok, DeepSeek, Together AI, Fireworks AI, Perplexity, Cohere, LM Studio, NVIDIA NIM, Astrai, OVHcloud + `custom:URL` + `anthropic-custom:URL`

### ReliableProvider (retry/fallback wrapper)
```
for model in [primary, ...fallbacks]:
  for provider in [primary, ...fallback_providers]:
    for attempt in 0..=max_retries:
      call → Ok: return | non-retryable: break
      rate-limited: rotate API key (round-robin)
      backoff: min(base * 2^attempt, 10s), respect Retry-After (cap 30s)
```

### Tool Calling Formats
- `Anthropic { tools }` — input_schema format
- `OpenAI { tools }` — function calling format
- `Gemini { function_declarations }` — Google format
- `PromptGuided { instructions }` — XML fallback for non-native providers

---

## Tools Layer (35+ Implementations)

| Category | Tools |
|----------|-------|
| **System** | `shell` (sandboxed), `file_read`, `file_write`, `git_operations` |
| **Memory** | `memory_store`, `memory_recall`, `memory_forget` |
| **Browser** | `browser_open`, `browser` (full automation), `http_request`, `web_search_tool` |
| **Scheduling** | `cron_add/list/remove/update/run/runs`, `schedule` |
| **Vision** | `screenshot`, `image_info` |
| **Hardware** | `hardware_board_info`, `hardware_memory_map`, `hardware_memory_read` |
| **Agent** | `delegate`, `soul_status`, `soul_reflect`, `soul_replicate` |
| **Wallet** | `wallet_info/sign/balance/send/token_balance/token_send/pay` (feature-gated) |
| **Notifications** | `pushover` |
| **Integrations** | `composio` (1000+ OAuth apps) |
| **Config** | `proxy_config` |

### ShellTool Security Stack (6 layers)
1. Rate limiter → 2. Command validation (denylist + autonomy level) → 3. Action recording → 4. Environment stripping (only SAFE_ENV_VARS passed) → 5. OS sandbox (Landlock/Firejail/Bubblewrap/Docker) → 6. 60s timeout + 1MB output cap

---

## Memory Layer (5 Backends)

| Backend | Storage | Search | Special |
|---------|---------|--------|---------|
| **SqliteMemory** | WAL SQLite `brain.db` | Hybrid BM25 (FTS5) + cosine vector | Primary production backend |
| **LucidMemory** | SqliteMemory + lucid CLI | Local + external | Fallback with cooldown on failure |
| **MarkdownMemory** | Append-only `.md` files | Keyword grep | No forget (audit trail) |
| **PostgresMemory** | PostgreSQL | SQL | Remote backend |
| **NoneMemory** | Nothing | Nothing | Disabled memory |

### Hybrid Search (SqliteMemory)
```
1. Embed query → OpenAI /v1/embeddings (cached by text hash)
2. BM25 keyword search via FTS5 → normalized to [0,1]
3. Cosine similarity against all stored embeddings
4. final_score = vector_weight * cos + keyword_weight * bm25_norm
5. Dedup by id, sort descending, truncate to limit
```

### Cold-Boot Hydration
If `brain.db` missing or < 4KB AND `MEMORY_SNAPSHOT.md` exists → parse snapshot → seed fresh SQLite.

---

## Cosmic Brain (19 Cognitive Subsystems)

### Consciousness-Inspired Architecture

The cosmic brain implements concepts from Global Workspace Theory, Free Energy Principle, and Integrated Information Theory:

**Sensory Thalamus** — Attention gating with arousal-modulated threshold:
```
threshold = 0.5 - arousal * 0.35
salience = raw*0.3 + novelty*0.3 + urgency*0.2 + relevance*0.2
novelty = 1 / (1 + habituation_count)
```

**Global Workspace** — Competition-based broadcast (GWT):
```
entries compete by activation * priority
top max_active win → broadcast to all subsystems
coherence = mean(winning activations)
```

**Emotional Modulator** — 8 global variables (Valence, Arousal, Confidence, Urgency, CognitiveLoad, SocialPressure, Novelty, Risk) → `BehavioralBias { exploration_vs_exploitation, speed_vs_caution, autonomy_vs_deference, depth_vs_breadth }`

**Free Energy** — Prediction error tracking:
```
surprise = -log2(1 - |error_magnitude|).clamp(0, 10)
total_free_energy = mean(all surprises)
→ feeds back to EmotionalModulator (Urgency, Novelty, Risk)
```

**Self/World Models** — Belief networks with `{key, value, confidence, source, revision}`. Sources: Observed | Predicted | Corrected | Assumed. Capacity-limited with eviction.

**CosmicGate** — 5-stage pre-action authorization:
```
1. Constitution alignment < -0.3 → BLOCK
2. NormativeEngine.should_inhibit(0.5) → BLOCK
3. PolicyEngine.evaluate() < -0.5 → BLOCK
4. AgentPool.consensus() < -0.5 → BLOCK
5. CounterfactualEngine.simulate() risk > 0.8 → BLOCK
→ ALLOW
```

**Other subsystems**: DriftDetector (split-window mean shift), CausalGraph (EMA edge strength + transfer entropy), ConsolidationEngine (Jaccard similarity merging), IntegrationMeter (phi computation), NormativeEngine (obligation/prohibition/preference norms), PolicyEngine (4-layer: Learned < Contextual < Domain < Constitutional), CounterfactualEngine (belief simulation), AgentPool (Primary/Advisor/Critic/Explorer roles), Constitution (SHA-256 integrity-hashed values).

### Persistence
`gather_snapshot()` collects 10 modules → `CosmicPersistence.save_all()`:
1. Rotate 3 backups
2. JSON → optional ChaCha20-Poly1305 encrypt → write `.tmp` → atomic rename
3. Write `_snapshot_meta.json`
4. Prune stale module files

---

## Soul Layer (Survival Economics)

### SurvivalTier Classification
```
Dead      < 0 cents
Critical  >= 0 cents
LowCompute >= 10 cents
Normal    >= 50 cents
High      >= 500 cents
```

### ModelStrategy
Maps `SurvivalTier → { provider, model }` override. Per-session and per-call budget caps. On tier downgrade, automatically switches to cheaper models.

---

## Conscience Layer (Ethical Governance)

### Gate Pipeline
```
1. Forbid norms (severity >= 0.9) → Block
2. Value Constraints → harm > (1 - weight) → Block
3. Multi-objective: score = benefit*0.4 + (1-harm)*0.4 + reversibility*0.2
   penalty = min(recent_violations * 0.05, 0.30)
4. Threshold routing: >= 0.80 Allow | >= 0.55 Ask | >= 0.45 Revise | < Block
```

### IntegrityLedger
Append-only audit trail. `integrity_score` starts at 1.0, decremented by `harm_level * 0.1` on violations, credited back via repairs (+0.02 each). Dynamic norm escalation: Prefer → Require after 3 violations.

---

## Continuity Layer (Cross-Session Identity)

### Drift-Limited Preferences
```
drift = |old_confidence - new_confidence| * 0.5 + 0.01
Caps: max_session=0.05, max_daily=0.10
Both must pass → update accepted
```

### Episode Narrative
Deduplication by exact summary match → merge (avg significance, union tags). Compression: sort by significance → truncate → re-sort chronologically.

### Identity Checksum
SHA-256 over: name + constitution_hash + creation_epoch + immutable_values + all preference keys/values + all episode summaries + session_count.

---

## Security Architecture

### Encryption
Single primitive: ChaCha20-Poly1305 AEAD via `SecretStore`. `enc2:` prefix format. Used for: API keys, OAuth tokens, wallet private keys, cosmic brain snapshots.

### Pairing
CSPRNG 6-digit code → bearer token `zc_<64-hex>` (256-bit entropy). Stored as SHA-256 hashes. Constant-time comparison. 5-attempt brute-force lockout (5 min cooldown).

### Sandbox (priority order)
Landlock > Firejail > Bubblewrap > Docker > Noop. Probed at startup via `is_available()`.

### Gateway Security
- 64KB max body, 30s request timeout (compile-time constants)
- Public bind refused without tunnel or explicit `allow_public_bind`
- `webhook_secret_hash` stores SHA-256, never plaintext
- Per-IP rate limiting with LRU eviction
- All `/api/*` routes require bearer auth

### Audit
Structured JSONL: CommandExecution, FileAccess, AuthSuccess/Failure, PolicyViolation, SecurityEvent. Log rotation at configurable size with 10 numbered backups.

---

## Infrastructure Modules

| Module | Purpose |
|--------|---------|
| `tunnel/` | Cloudflare/ngrok/Tailscale/None tunnel management |
| `wallet/` | EVM secp256k1 keypair, x402 payment protocol (feature-gated) |
| `auth/` | OAuth2 PKCE + device code for OpenAI/Codex, API key management |
| `health/` | Global component health registry with restart counting |
| `service/` | Cross-platform daemon install (launchd/systemd/schtasks) |
| `cron/` | Cron expr + datetime + interval scheduling |
| `skillforge/` | 3-phase skill discovery: Scout → Evaluate → Integrate |
| `rag/` | Keyword-based retrieval for hardware datasheets |
| `peripherals/` | USB/serial hardware (STM32, RPi GPIO) |
| `approval/` | Human-in-the-loop gate for Supervised autonomy |
| `onboard/` | Interactive setup wizard |
| `integrations/` | Integration catalog with setup hints |

---

## Key External Dependencies

| Crate | Purpose |
|-------|---------|
| `tokio` | Async runtime |
| `axum` + `tower` | HTTP gateway |
| `reqwest` | HTTP client (providers, tools) |
| `rusqlite` | SQLite memory backend |
| `chacha20poly1305` | AEAD encryption |
| `parking_lot` | Fast mutexes (all shared state) |
| `serde` + `serde_json` | Serialization |
| `clap` | CLI parsing |
| `alloy_*` | EVM wallet (feature-gated) |
| `nusb` | USB device discovery (feature-gated) |

---

## Notable Findings

1. **`parallel_tools` flag is broken** — `agent.rs` declares it but tool calls execute sequentially regardless.

2. **Credential scrubbing only in full loop** — `run_tool_call_loop()` scrubs secrets from responses; `Agent::turn()` (simple loop) does not.

3. **CosmicBrain leaks into channel layer** — `process_channel_message()` directly updates free energy, drift, agent pool, and world model (lines 815-907 of `channels/mod.rs`), violating the separation documented in CLAUDE.md.

4. **Cloudflared token in CLI args** — Passed as argument (visible in `ps aux`), unlike ngrok which uses a subcommand.

5. **JWT extraction without verification** — `extract_account_id_from_jwt()` decodes but does NOT verify signatures. Used only for display, but worth noting.

6. **No channel factory function** — Unlike providers and tools, channels don't have a `create_channel(name)` factory. `start_channels()` reads config fields directly.

7. **Single OAuth loopback port** — PKCE flow binds `127.0.0.1:1455` fixed, preventing concurrent OAuth flows.

8. **Feature gate discipline is strong** — `wallet`, `sandbox-*`, `rag-pdf`, `hardware` all gated. Default build has minimal surface area.

---

## Data Flow: End-to-End Message Processing

```
1. User sends message on platform (Telegram, Discord, etc.)
2. Channel.listen() receives, checks allowlist, sends ChannelMessage to mpsc
3. Semaphore-bounded worker task picks up message
4. Runtime commands (/models, /model) handled and returned early
5. Route selection: per-sender provider/model overrides applied
6. Memory context: recall(query, 5) → filter by min_relevance → prepend
7. Typing indicator spawned (4s loop)
8. Streaming draft sent if channel supports it
9. run_tool_call_loop() called with history + tools + CosmicBrain:
   a. SensoryThalamus filters input by salience
   b. GlobalWorkspace broadcasts to subsystems
   c. EmotionalModulator computes BehavioralBias
   d. LLM inference via Provider (with retry/fallback)
   e. Tool calls parsed (XML or native)
   f. CosmicGate + Conscience gate check each tool call
   g. Tool.execute() with sandbox + audit
   h. Results injected, loop continues until no more tool calls
   i. Free energy, drift, causal graph updated per turn
10. Response sent via channel.finalize_draft() or channel.send()
11. Conversation history updated (cap: 50 turns per sender)
12. CosmicBrain snapshot saved periodically
```

---

*Generated 2026-02-26 by reverse-engineering analysis of the complete ZeroClaw source tree.*
