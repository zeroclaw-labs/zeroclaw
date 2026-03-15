# MoA — Architecture & Product Vision

> **Date**: 2026-03-01
> **Status**: Living document — updated with each major feature milestone
> **Audience**: AI reviewers (Gemini, Claude), human contributors, future maintainers

---

## 1. Product Vision

### What is MoA?

**MoA (Mixture of Agents)** is a cross-platform AI personal assistant
application that runs **independently on each user's device** — desktop
(Windows, macOS, Linux via Tauri) and mobile (iOS, Android). Each MoA app
instance contains a full **ZeroClaw autonomous agent runtime** with its own
local SQLite database for long-term memory. Multiple devices owned by the
same user **synchronize their long-term memories in real-time** via a
lightweight relay server, without ever persistently storing memory on the
server (patent: server-non-storage E2E encrypted memory sync).

MoA combines multiple AI models collaboratively to deliver results across
seven task categories — with particular emphasis on **real-time simultaneous
interpretation** and **AI-collaborative coding**.

### Core Thesis

> Single-model AI is limited. The best results come from multiple
> specialized AI models **collaborating, reviewing, and refining each
> other's work** — much like a team of human experts.

This "mixture of agents" philosophy applies everywhere:
- **Coding**: Claude Opus 4.6 writes code → Gemini 3.1 Pro reviews
  architecture → Claude validates Gemini's feedback → consensus-driven
  quality
- **Interpretation**: Gemini Live processes audio in real-time →
  segmentation engine commits phrase-level chunks → translation streams
  continuously
- **General tasks**: Local SLM (gatekeeper) handles simple queries → cloud
  LLM handles complex ones → routing optimizes cost/latency
- **Memory**: Each device runs independently but all memories converge via
  delta-based E2E encrypted sync

---

## 2. Deployment Architecture

### Per-User, Per-Device, Independent App

```
┌─────────────────────────────────────────────────────────────────┐
│                        User "Alice"                             │
│                                                                 │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────────┐  │
│  │  Desktop App  │  │  Mobile App  │  │  Mobile App          │  │
│  │  (Tauri/Win)  │  │  (Android)   │  │  (iOS)               │  │
│  │              │  │              │  │                      │  │
│  │  ZeroClaw    │  │  ZeroClaw    │  │  ZeroClaw            │  │
│  │  + SQLite    │  │  + SQLite    │  │  + SQLite            │  │
│  │  + sqlite-vec│  │  + sqlite-vec│  │  + sqlite-vec        │  │
│  │  + FTS5      │  │  + FTS5      │  │  + FTS5              │  │
│  └──────┬───────┘  └──────┬───────┘  └──────────┬───────────┘  │
│         │                 │                      │              │
│         └────────┬────────┴──────────────────────┘              │
│                  │ E2E encrypted delta sync                     │
│                  ▼                                              │
│         ┌────────────────┐                                     │
│         │ Railway Relay   │  ← 5-minute TTL buffer only        │
│         │ Server          │  ← no persistent memory storage    │
│         └────────────────┘                                     │
└─────────────────────────────────────────────────────────────────┘
```

**Key principles:**
1. Each MoA app instance **works independently** — no server required for
   normal AI operations
2. Each device has its **own SQLite with long-term memory** (sqlite-vec for
   embeddings, FTS5 for full-text search)
3. Memory sync happens **peer-to-peer via relay** — the relay server holds
   data for at most **5 minutes** then deletes it
4. A user can install MoA on **multiple devices** — all share the same
   memory through real-time sync
5. **Normal AI operations do NOT go through the relay server** — the app
   calls LLM APIs directly from the device
6. **MoA = one GUI app** — the ZeroClaw runtime is bundled inside every MoA
   installer as a sidecar binary. Users download and install one file.
   There is no separate "ZeroClaw" install step. See "Unified App
   Experience" section below for the full contract.

### LLM API Key Model — 3-Tier Provider Access

MoA uses a **3-tier provider access model** that determines how LLM calls
are routed, billed, and which models are used.

#### Tier Overview

```
┌─────────────────────────────────────────────────────────────────────┐
│  3-Tier Provider Access Model                                       │
│                                                                     │
│  ① UserKey Mode (유저 자체 키 모드)                                 │
│     Condition: User has provided their own API key(s)               │
│     → App calls LLM provider directly from the device               │
│     → User selects which model to use (latest top-tier available)   │
│     → NO credit deduction (user pays provider directly)             │
│     → NO Railway relay involvement for LLM calls                    │
│                                                                     │
│  ② Platform Selected Mode (플랫폼 모델 선택 모드)                   │
│     Condition: No API key + user manually selected a model          │
│     → LLM call routed through Railway relay (operator's API key)    │
│     → User's selected model is used                                 │
│     → Credits deducted at 2.2× actual API cost (2× + VAT)          │
│                                                                     │
│  ③ Platform Default Mode (플랫폼 기본 모드)                         │
│     Condition: No API key + no model selection (new users)          │
│     → LLM call routed through Railway relay (operator's API key)    │
│     → Task-based automatic model routing (see table below)          │
│     → Credits deducted at 2.2× actual API cost (2× + VAT)          │
│     → New users receive signup bonus credits upon registration      │
└─────────────────────────────────────────────────────────────────────┘
```

#### Access Mode Decision Table

| Mode | Condition | LLM Call Route | Model Selection | Billing |
|------|-----------|---------------|-----------------|---------|
| **UserKey** | User provided API key | Direct from device to provider | User chooses (top-tier available) | Free (user pays provider) |
| **Platform (Selected)** | No API key + model chosen | Railway relay (operator key) | User's chosen model | 2.2× actual API cost in credits |
| **Platform (Default)** | No API key + no selection | Railway relay (operator key) | Auto-routed by task type | 2.2× actual API cost in credits |

#### Task-Based Default Model Routing (Platform Default Mode)

When a user has no API key and has not selected a specific model, the
system automatically routes to the most appropriate model per task type:

| Task Category | Provider | Default Model | Rationale |
|---------------|----------|---------------|-----------|
| **일반 채팅 (General Chat)** | Gemini | `gemini-3.1-flash-lite-preview` | Most cost-effective for casual conversation |
| **추론/문서 (Reasoning/Document)** | Gemini | `gemini-3.1-pro-preview` | High-quality reasoning and document analysis |
| **코딩 (Coding)** | Anthropic | `claude-opus-4-20250514` | Best-in-class code generation |
| **코드 리뷰 (Code Review)** | Gemini | `gemini-3.1-pro-preview` | Architecture-aware review |
| **이미지 (Image)** | Gemini | `gemini-3.1-flash-lite-preview` | Cost-effective vision tasks |
| **음악 (Music)** | Gemini | `gemini-3.1-flash-lite-preview` | Lightweight orchestration |
| **비디오 (Video)** | Gemini | `gemini-3.1-flash-lite-preview` | Lightweight orchestration |
| **통역 (Interpretation)** | Gemini | Gemini 2.5 Flash Live API | Real-time voice streaming |

#### Credit System & Billing Logic

```
┌─────────────────────────────────────────────────────────────────────┐
│  Credit Billing Flow (Platform modes only)                          │
│                                                                     │
│  1. New user registers → receives signup bonus credits              │
│     (e.g., equivalent to several dollars of usage)                  │
│                                                                     │
│  2. Each LLM API call:                                              │
│     actual_api_cost_usd = (input_tokens × input_price/1M)          │
│                         + (output_tokens × output_price/1M)         │
│     credits_to_deduct = actual_api_cost_usd × 2.2                  │
│     (2.0× operator margin + 10% VAT = 2.2×)                        │
│                                                                     │
│  3. Before every deduction, check remaining balance:                │
│     ├─ balance > warning_threshold  → proceed silently              │
│     ├─ balance ≤ warning_threshold  → show warning alert:           │
│     │   "크레딧이 부족합니다. 충전하시거나 직접 API 키를 입력하세요" │
│     │   → Option A: Purchase more credits (결제)                    │
│     │   → Option B: Enter own API keys (설정 → API 키)              │
│     │     Supported: Claude, OpenAI, Gemini (3 providers)           │
│     └─ balance = 0  → block request, require recharge or API key    │
│                                                                     │
│  4. Users can enter their own API keys at any time:                 │
│     → Claude (Anthropic) API key                                    │
│     → OpenAI API key                                                │
│     → Gemini (Google) API key                                       │
│     Once a key is entered, that provider's calls switch to          │
│     UserKey mode (no credit deduction, direct device→provider)      │
└─────────────────────────────────────────────────────────────────────┘
```

#### Railway Relay vs Direct API Call

```
┌─────────────────────────────────────────────────────────────────────┐
│  When is Railway relay used for LLM calls?                          │
│                                                                     │
│  Railway relay (operator API key):                                  │
│  ├─ User has NO API key for the requested provider                  │
│  ├─ LLM request is proxied through Railway server                   │
│  ├─ Operator's API key (ADMIN_*_API_KEY env vars) is used           │
│  ├─ Credits are deducted at 2.2× from user's balance                │
│  └─ Operator's keys NEVER leave the server                          │
│                                                                     │
│  Direct device→provider (user's own key):                           │
│  ├─ User has entered their own API key for that provider            │
│  ├─ App calls the LLM API directly from the user's device           │
│  ├─ NO Railway relay involvement                                    │
│  ├─ NO credit deduction                                             │
│  └─ User pays the provider directly at standard API rates           │
│                                                                     │
│  Important: Railway relay is ALWAYS used for:                       │
│  ├─ Memory sync (E2E encrypted delta exchange) — regardless of key  │
│  └─ Remote channel routing (KakaoTalk, Telegram, etc.)              │
│  These are NOT LLM calls and do not consume credits.                │
└─────────────────────────────────────────────────────────────────────┘
```

| Scenario | API Key Source | Route | Model Used | Billing |
|----------|---------------|-------|------------|---------|
| User has key for provider | User's own | Device → Provider directly | User's choice (top-tier) | Free (user pays provider) |
| User has no key (default) | Operator's (Railway env) | Device → Railway relay → Provider | Task-based auto-routing | 2.2× actual API cost in credits |
| User has no key (selected model) | Operator's (Railway env) | Device → Railway relay → Provider | User's selected model | 2.2× actual API cost in credits |
| Voice interpretation | User's or operator's | Same rules as above | Gemini 2.5 Flash Live API | Same rules as above |

### Remote Access via Channels

Users can interact with their MoA app from **any device** (even without
MoA installed) through messaging channels:

```
┌────────────────┐     ┌────────────┐     ┌──────────────────┐
│ Any device     │────▸│  Channel   │────▸│  User's MoA app  │
│ (no MoA app)  │◂────│  (relay)   │◂────│  (on home device)│
└────────────────┘     └────────────┘     └──────────────────┘
```

**Supported channels:**
- **KakaoTalk** (MoA addition — not in upstream ZeroClaw)
- Telegram
- Discord
- Slack
- LINE
- Web chat (homepage)

Users send messages through these channels to their remote MoA device,
which processes the request and sends back the response through the same
channel.

### Web Chat Access

A web-based chat interface on the MoA homepage allows users to:
- Send commands to their remote MoA app instance
- Receive responses in real-time
- No MoA app installation required on the browsing device
- Authenticated connection to the user's registered MoA devices

### Unified App Experience (MoA + ZeroClaw = One App)

> **MANDATORY REQUIREMENT**: MoA and ZeroClaw MUST appear as a **single,
> inseparable application** to end users. The sidecar architecture is an
> internal implementation detail that is never exposed in the user
> experience.

#### Principles

1. **One download, one install, one app** — The user downloads one
   installer file (`.dmg`, `.msi`, `.AppImage`, `.apk`, `.ipa`). This
   single package contains both the MoA frontend (Tauri webview) and the
   ZeroClaw runtime (Rust sidecar binary). There is no separate "ZeroClaw
   installer" visible to the user.
2. **Third parties cannot separate them** — The sidecar binary is bundled
   inside the app package (Tauri's `externalBin` mechanism). It is not a
   user-serviceable part. The MoA app refuses to function without its
   embedded ZeroClaw runtime.
3. **Automatic lifecycle management** — On app launch, MoA silently starts
   the ZeroClaw gateway process in the background. On app exit, the
   ZeroClaw process is terminated. On crash, the app recovers both
   components together. The user never sees "Starting ZeroClaw…" or any
   indication that two processes exist.
4. **Unified updates** — When a new version is available, the Tauri updater
   downloads one update package containing both the frontend and the
   ZeroClaw binary. The update is atomic — both components update together,
   never out of sync.
5. **Single configuration flow** — All ZeroClaw settings (API keys, model
   selection, channel config, memory preferences) are configured through
   the MoA GUI during first-run setup. There is no separate configuration
   file that users need to edit manually.

#### Installation Flow

```
User downloads MoA-1.0.0-x86_64.msi (or .dmg / .AppImage / .apk)
    │
    ▼
Standard OS installer runs
    │
    ├── Installs MoA app (Tauri frontend)
    ├── Installs ZeroClaw binary (sidecar, bundled inside app)
    ├── Creates desktop shortcut / Start menu entry (one icon: "MoA")
    └── First-run setup wizard:
         ├── Language selection
         ├── API key entry (or "Use credits" option)
         ├── Channel configuration (KakaoTalk, Telegram, etc.)
         └── Memory sync pairing (scan QR on second device)
    │
    ▼
App is ready. Single "MoA" icon in system tray / dock.
ZeroClaw runs as invisible background process.
```

#### Sidecar Architecture (Internal Implementation)

```
┌───────────────────────────────────────────────────┐
│  MoA App Process (Tauri)                          │
│  ┌─────────────────────────────────────────────┐  │
│  │  WebView (UI)                               │  │
│  │  ┌─────────────────────────────────────┐    │  │
│  │  │  React / TypeScript Frontend        │    │  │
│  │  │  Chat, Voice, Document, Settings    │    │  │
│  │  └───────────────┬─────────────────────┘    │  │
│  │                  │ Tauri IPC commands        │  │
│  │                  ▼                          │  │
│  │  Tauri Rust Host (lib.rs)                   │  │
│  │  ┌─────────────────────────────────────┐    │  │
│  │  │ spawn_zeroclaw_gateway()            │    │  │
│  │  │ health_check() / graceful_shutdown()│    │  │
│  │  └───────────────┬─────────────────────┘    │  │
│  └──────────────────┼──────────────────────────┘  │
│                     │ WebSocket (127.0.0.1:PORT)   │
│                     ▼                              │
│  ┌─────────────────────────────────────────────┐  │
│  │  ZeroClaw Sidecar Process                   │  │
│  │  (binaries/zeroclaw-{target-triple})        │  │
│  │                                             │  │
│  │  Gateway + Agent + Memory + Channels + ...  │  │
│  │  Full autonomous runtime                    │  │
│  └─────────────────────────────────────────────┘  │
└───────────────────────────────────────────────────┘
```

#### Latency Contract (Sidecar IPC Performance)

> **MANDATORY**: The sidecar (separate process) architecture must NOT
> introduce perceptible latency compared to in-process library embedding.

| Communication Method | Round-Trip Latency | Status |
|---------------------|-------------------|--------|
| In-process (cdylib) | ~0 (nanoseconds) | Baseline |
| Unix Domain Socket | 0.05–0.2ms | Acceptable |
| **WebSocket (localhost, persistent)** | **0.1–0.5ms** | **Chosen approach** |
| HTTP POST (localhost, per-request) | 1–3ms | Fallback only |

**Why this is acceptable**: The actual bottleneck is the LLM API call
(500ms–30s round-trip to cloud providers). Local IPC overhead of 0.1–0.5ms
is **<0.1% of total response time** and physically imperceptible to users.

**Implementation guarantees**:
1. MoA connects to ZeroClaw via a **persistent WebSocket** at startup —
   no connection setup overhead per message
2. Messages are serialized as JSON over the WebSocket — minimal framing
3. The WebSocket connection is over `127.0.0.1` (loopback) — no network
   stack involved, kernel memory copy only
4. For time-critical operations (voice streaming, typing indicators),
   binary WebSocket frames are used instead of JSON
5. Measured end-to-end: from MoA sending a user message to ZeroClaw
   returning the first LLM token, the IPC overhead is **<1ms** on all
   supported platforms

**Latency budget breakdown (typical chat message)**:
```
User types message ──▸ MoA frontend processes ──▸  ~5ms
MoA → ZeroClaw IPC                              ──▸  ~0.3ms  ← sidecar overhead
ZeroClaw processes (routing, memory recall)      ──▸  ~20ms
ZeroClaw → LLM API (network round-trip)          ──▸  ~500ms–30s  ← dominant
LLM → ZeroClaw (streaming tokens)               ──▸  continuous
ZeroClaw → MoA IPC (per token)                   ──▸  ~0.1ms  ← sidecar overhead
MoA frontend renders token                       ──▸  ~1ms
───────────────────────────────────────────────────
Total sidecar overhead: ~0.4ms out of 500ms+ total = <0.1%
```

---

## 3. Patent: Server-Non-Storage E2E Encrypted Memory Sync

### Title (발명의 명칭)

**서버 비저장 방식의 다중 기기 간 종단간 암호화 메모리 동기화 시스템 및 방법**

(Server-Non-Storage Multi-Device End-to-End Encrypted Memory
Synchronization System and Method)

### Problem Statement

Conventional cloud-sync approaches store user data persistently on a
central server, creating:
- Privacy risk (server breach exposes all user data)
- Single point of failure
- Regulatory compliance burden (GDPR, data residency)
- Server storage cost scaling with user count

### Invention Summary

A system where **each user device maintains its own authoritative copy**
of long-term memory in a local SQLite database, and **synchronizes changes
(deltas) with other devices via a relay server that never persistently
stores the data**.

### Architecture

```
Device A                    Relay Server              Device B
┌──────────┐               ┌──────────────┐          ┌──────────┐
│ SQLite   │               │              │          │ SQLite   │
│ (full    │──encrypt──▸   │  TTL buffer  │   ◂──────│ (full    │
│  memory) │  delta        │  (5 min max) │  fetch   │  memory) │
│          │               │              │  + apply │          │
│ vec+FTS5 │               │  No persist  │          │ vec+FTS5 │
└──────────┘               └──────────────┘          └──────────┘
```

### Core Mechanisms

#### 1. Delta-Based Sync (델타 기반 동기화)

- When a memory entry is created/updated/deleted on any device, only the
  **delta (change)** is transmitted — not the entire memory store
- Deltas include: operation type (insert/update/delete), entry ID, content
  hash, timestamp, vector embedding diff
- This minimizes bandwidth and enables efficient sync even on slow
  mobile networks

#### 2. End-to-End Encryption (종단간 암호화)

- All deltas are encrypted on the **sending device** before transmission
- The relay server **cannot read** the content — it only stores opaque
  encrypted blobs
- Decryption happens only on the **receiving device**
- Key derivation: device-specific keys derived from user's master secret
  via HKDF (see `src/security/device_binding.rs`)

#### 3. Server TTL Buffer (서버 임시 보관 — 5분 TTL)

- The relay server (Railway) holds encrypted deltas for a **maximum of
  5 minutes**
- If the receiving device is online, it fetches and applies deltas
  immediately
- If the receiving device comes online within 5 minutes, it picks up
  buffered deltas
- After 5 minutes, undelivered deltas are **permanently deleted** from
  the server
- The server **never has persistent storage of any user memory**

#### 4. Offline Reconciliation (오프라인 기기 동기화)

When a device comes online after being offline for more than 5 minutes:
- It cannot rely on the relay server buffer (TTL expired)
- Instead, it performs **peer-to-peer full reconciliation** with another
  online device of the same user
- Reconciliation uses vector clock / timestamp comparison to resolve
  conflicts
- Last-write-wins with semantic merge for non-conflicting concurrent edits

#### 5. Conflict Resolution (충돌 해결)

| Scenario | Resolution Strategy |
|----------|-------------------|
| Same entry edited on two devices | Last-write-wins (by timestamp) |
| Entry deleted on A, edited on B | Delete wins (tombstone preserved) |
| New entries on both devices | Both kept (no conflict) |
| Embedding vectors diverged | Re-compute from merged text content |

### Implementation in MoA

| Component | Module | Description |
|-----------|--------|-------------|
| Local memory store | `src/memory/` | SQLite + sqlite-vec + FTS5 per device |
| Sync engine | `src/sync/` | Delta generation, encryption, relay communication |
| E2E encryption | `src/security/` | HKDF key derivation, ChaCha20-Poly1305 encryption |
| Relay client | `src/sync/` | WebSocket connection to Railway relay server |
| Conflict resolver | `src/sync/coordinator.rs` | Vector clock comparison, merge strategies |
| Device binding | `src/security/device_binding.rs` | Device identity, key pairing |

### Security Properties

1. **Zero-knowledge relay**: Server cannot decrypt any data
2. **Forward secrecy**: Key rotation per sync session
3. **Device compromise isolation**: Compromising one device does not
   expose keys of other devices
4. **Deletion guarantee**: Server data is ephemeral (5-minute TTL)
5. **No server-side backup**: There is no "cloud copy" of user data

### Patent Full Text (특허출원서 전문)

The complete patent specification is maintained in
[`docs/ephemeral-relay-sync-patent.md`](./ephemeral-relay-sync-patent.md).

This includes:
- **발명의 명칭**: 서버 비저장 방식의 다중 기기 간 종단간 암호화 메모리 동기화 시스템 및 방법
- **기술분야**: Multi-device memory synchronization without persistent server storage
- **배경기술**: Analysis of prior art (cloud-sync vs P2P) and their limitations
- **발명의 내용**: 3-tier hierarchical sync (Layer 1: TTL relay, Layer 2: delta journal + version vectors + order buffer, Layer 3: manifest-based full sync)
- **실시예 1-7**: Detailed implementation examples with sequence diagrams
  - System architecture block diagram
  - Layer 1 real-time relay sequence
  - Layer 2 order guarantee mechanism
  - Layer 2 offline reconnection auto-resync
  - Layer 3 manual full sync via manifest comparison
  - 3-tier integrated decision flowchart
  - Data structure specifications (SyncDelta, VersionVector, FullSyncManifest, BroadcastMessage, ReconcilerState)
- **청구범위**: 13 claims (3 independent + 10 dependent)
  - Claim 1: Method for multi-device sync without persistent server storage
  - Claim 2: Sequence ordering with order buffer
  - Claim 3: Idempotency via duplicate detection
  - Claim 4: Manual full sync for long-offline devices
  - Claim 8: AES-256-GCM + PBKDF2 key derivation
  - Claim 11: System claim (device module + relay server)
  - Claim 13: Computer-readable recording medium
- **요약서**: Summary with representative diagram (Figure 6: 3-tier decision flow)

---

## 4. Target Users

| User type | Primary use case |
|-----------|-----------------|
| **Korean business professionals** | Real-time Korean ↔ English/Japanese/Chinese interpretation for meetings, calls |
| **Developers** | AI-assisted coding with Claude + Gemini self-checking review |
| **Content creators** | Document drafting, image/video/music generation |
| **General users** | Web search, Q&A, daily tasks with multi-model intelligence |
| **Multi-device users** | Seamless AI assistant across desktop + mobile with synced memory |
| **Channel users** | Interact with MoA via KakaoTalk, Telegram, Discord, web chat without installing the app |

---

## 5. Task Categories

MoA organizes all user interactions into **7 top-bar categories** and
**3 sidebar navigation items**:

### Top-Bar (Task Modes)

| Category | Korean | UI Mode | Tool Scope |
|----------|--------|---------|------------|
| **WebGeneral** | 웹/일반 | default chat | BASE + VISION |
| **Document** | 문서 | `document` editor (2-layer viewer+Tiptap) | BASE + DOCUMENT |
| **Coding** | 코딩 | `sandbox` | ALL tools (unrestricted) |
| **Image** | 이미지 | default chat | BASE + VISION |
| **Music** | 음악 | default chat | BASE |
| **Video** | 비디오 | default chat | BASE + VISION |
| **Translation** | 통역 | `voice_interpret` | MINIMAL (memory + browser + file I/O) |

### Sidebar (Navigation)

| Item | Korean | Purpose |
|------|--------|---------|
| **Channels** | 채널 | KakaoTalk, Telegram, Discord, Slack, LINE, Web chat management |
| **Billing** | 결제 | Credits, usage, payment |
| **MyPage** | 마이페이지 | User profile, API key settings, device management |

---

## 6. System Architecture

### High-Level Module Map

```
src/
├── main.rs              # CLI entrypoint, command routing
├── lib.rs               # Module exports, shared enums
├── config/              # Schema + config loading/merging
├── agent/               # Orchestration loop
├── gateway/             # Webhook/gateway server
├── security/            # Policy, pairing, secret store, E2E encryption
├── memory/              # SQLite + sqlite-vec + FTS5 long-term memory
├── providers/           # Model providers (Gemini, Claude, OpenAI, Ollama, etc.)
├── channels/            # KakaoTalk, Telegram, Discord, Slack, LINE, Web chat
├── tools/               # Tool execution (shell, file, memory, browser)
├── coding/              # Multi-model code review pipeline ← MoA addition
├── voice/               # Real-time voice interpretation  ← MoA addition
├── sandbox/             # Coding sandbox (run→observe→fix loop)
├── task_category.rs     # Category definitions + tool routing ← MoA addition
├── gatekeeper/          # Local SLM intent classification  ← MoA addition
├── billing/             # Credit-based billing system      ← MoA addition
├── ontology/            # Structured relational memory — digital twin graph ← MoA addition
├── sync/                # E2E encrypted memory sync engine (patent impl)
├── peripherals/         # Hardware peripherals (STM32, RPi GPIO)
├── runtime/             # Runtime adapters
├── observability/       # Tracing, metrics
├── telemetry/           # Telemetry collection
├── plugins/             # Plugin loader
└── ...                  # (auth, hooks, rag, etc.)

clients/tauri/               # Native desktop/mobile app (Tauri 2.x + React + TypeScript) ← MoA primary
├── src/App.tsx              # Main app shell — page routing, sidebar, auth flow
├── src/components/
│   ├── Chat.tsx             # AI chat interface
│   ├── DocumentEditor.tsx   # 2-layer document editor orchestrator ← NEW
│   ├── DocumentViewer.tsx   # Read-only iframe viewer (pdf2htmlEX/PyMuPDF HTML) ← NEW
│   ├── TiptapEditor.tsx     # Tiptap WYSIWYG Markdown editor (Layer 2) ← NEW
│   ├── Sidebar.tsx          # Navigation sidebar (chat list, document editor entry)
│   ├── Interpreter.tsx      # Real-time simultaneous interpretation
│   ├── Login.tsx / SignUp.tsx / Settings.tsx
│   └── ...
├── src/lib/
│   ├── api.ts               # API client (ZeroClaw gateway + Railway relay)
│   ├── i18n.ts              # Locale support (ko, en)
│   └── storage.ts           # Chat session persistence (localStorage)
├── src-tauri/src/lib.rs     # Tauri Rust host — IPC commands, PDF conversion pipeline
└── src-tauri/Cargo.toml

web/                     # Web dashboard UI (Vite + React + TypeScript)  ← MoA addition
├── src/pages/           # AgentChat, Config, Cost, Cron, Dashboard, Devices, …
├── src/components/      # Shared React components
└── vite.config.ts

site/                    # Main website / homepage (Vite + React + TypeScript) ← MoA addition
├── src/pages/           # Landing, pricing, docs, web-chat entry
└── vite.config.ts
```

### Platform Targets

| Platform | Technology | ZeroClaw Runtime | SQLite |
|----------|-----------|-----------------|--------|
| **Windows** | Tauri 2.x | Native Rust binary | Local file |
| **macOS** | Tauri 2.x | Native Rust binary | Local file |
| **Linux** | Tauri 2.x | Native Rust binary | Local file |
| **Android** | Tauri 2.x Mobile | Native Rust (NDK) | Local file |
| **iOS** | Tauri 2.x Mobile | Native Rust (static lib) | Local file |

Every platform runs the **same ZeroClaw Rust core** — the app is not a
thin client. Each device is a fully autonomous AI agent. ZeroClaw is
bundled inside the MoA app package as a sidecar binary (desktop) or
static library (mobile). Users see and interact with one app: **MoA**.
The ZeroClaw runtime is invisible to end users.

### Trait-Driven Extension Points

| Trait | Location | Purpose |
|-------|----------|---------|
| `Provider` | `src/providers/traits.rs` | Model API abstraction |
| `Channel` | `src/channels/traits.rs` | Messaging platform abstraction |
| `Tool` | `src/tools/traits.rs` | Tool execution interface |
| `Memory` | `src/memory/traits.rs` | Memory backend abstraction |
| `Observer` | `src/observability/traits.rs` | Observability sink |
| `RuntimeAdapter` | `src/runtime/traits.rs` | Runtime environment abstraction |
| `Peripheral` | `src/peripherals/traits.rs` | Hardware board abstraction |
| `VoiceProvider` | `src/voice/pipeline.rs` | Voice API streaming |
| `CodeReviewer` | `src/coding/traits.rs` | AI code review agent |
| `OntologyRepo` | `src/ontology/repo.rs` | Structured relational memory CRUD |

**Rule**: New capabilities are added by implementing traits + factory
registration, NOT by cross-module rewrites.

---

## 6A. Structured Relational Memory — Digital Twin Graph Layer

### Goal

Elevate MoA's memory from a flat text store to a **structured knowledge
graph** that models the user's real world as a digital twin. Objects
(nouns), Links (relationships), and Actions (verbs) form a graph that the
LLM agent queries and mutates through dedicated tools — enabling
contextual reasoning, preference persistence, and automated graph
maintenance.

### Why This Matters

MoA's existing episodic memory (SQLite FTS5 + vector embeddings) stores
raw text chunks. It is powerful for recall, but it cannot answer
structural questions like "which contacts belong to Project X?" or
"what did I tell 김부장 last week?". The ontology layer sits **above**
the existing memory and provides a typed, relational view of the user's
world without replacing the episodic layer.

### Layer Stack

```
┌──────────────────────────────────────────────────┐
│  LLM Agent (brain)                               │
│  ┌────────────────────────────────────────────┐  │
│  │ Ontology Tools:                            │  │
│  │  ontology_get_context                      │  │
│  │  ontology_search_objects                   │  │
│  │  ontology_execute_action                   │  │
│  └────────────────┬───────────────────────────┘  │
│                   │                              │
│  ┌────────────────▼───────────────────────────┐  │
│  │ Ontology Layer (src/ontology/)             │  │
│  │  OntologyRepo   — CRUD on objects/links    │  │
│  │  ActionDispatcher — route → ZeroClaw tools │  │
│  │  RuleEngine     — post-action automation   │  │
│  │  ContextBuilder — snapshot for LLM prompt  │  │
│  └────────────────┬───────────────────────────┘  │
│                   │                              │
│  ┌────────────────▼───────────────────────────┐  │
│  │ Existing Memory Layer                      │  │
│  │  brain.db (SQLite + FTS5 + vec embeddings) │  │
│  │  + ontology tables coexist in same DB      │  │
│  └────────────────────────────────────────────┘  │
│                   │                              │
│  ┌────────────────▼───────────────────────────┐  │
│  │ ZeroClaw Tool Layer (70+ tools)            │  │
│  │  shell, http, kakao, browser, cron, ...    │  │
│  └────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────┘
```

### Core Triple: Object / Link / Action

| Concept | Table | Example |
|---------|-------|---------|
| **Object** (noun) | `ontology_objects` | User, Contact, Task, Document, Project, Preference |
| **Link** (relationship) | `ontology_links` | User → owns → Task, Contact → belongs_to → Project |
| **Action** (verb) | `ontology_actions` | SendMessage, CreateTask, FetchResource, SavePreference |

Each concept has a **meta-type** table (`ontology_object_types`,
`ontology_link_types`, `ontology_action_types`) that defines the schema,
and an **instance** table that stores actual data. All tables coexist in
`brain.db` alongside the existing memory tables — no separate database
file is needed.

### Module Structure (`src/ontology/`)

| File | Component | Responsibility |
|------|-----------|----------------|
| `types.rs` | Data types | `ObjectType`, `LinkType`, `ActionType`, `OntologyObject`, `OntologyLink`, `OntologyAction`, `ActionStatus`, `ActorKind`, request/response types |
| `schema.rs` | Schema init | `init_ontology_schema()` — 6 tables + FTS5 index; `seed_default_types()` — default object/link/action types |
| `repo.rs` | Repository | `OntologyRepo` with `Arc<Mutex<Connection>>` — CRUD operations, FTS5 search, `ensure_object()` upsert, `list_objects_by_type()` |
| `dispatcher.rs` | Action routing | `ActionDispatcher` — 4-step execute flow: log pending → route to tool → update result → run rules |
| `rules.rs` | Rule engine | `RuleEngine` — type-specific rules (SendMessage, CreateTask, etc.) + cross-cutting rules (auto-tag clients, group tasks, channel profiling) |
| `context.rs` | Context builder | `ContextBuilder` — builds `ContextSnapshot` (user, contacts, tasks, projects, recent actions) for LLM prompt injection |
| `tools.rs` | LLM tools | `OntologyGetContextTool`, `OntologySearchObjectsTool`, `OntologyExecuteActionTool` — implement `Tool` trait |
| `mod.rs` | Entry point | Module re-exports |

### ActionDispatcher: 4-Step Execution Flow

```
1. Log action as "pending" in ontology_actions
         │
         ▼
2. Route to handler:
   ├── Internal ontology operation (CreateObject, CreateLink, SavePreference, …)
   └── ZeroClaw tool execution (SendMessage→kakao_send, FetchResource→http_fetch, …)
         │
         ▼
3. Update action log with result + status (success/error)
         │
         ▼
4. Trigger RuleEngine.apply_post_action_rules()
   ├── Type-specific rules (SendMessage → link Contact↔Task)
   └── Cross-cutting rules (auto-tag important clients, group tasks into projects)
```

### RuleEngine Design

Rules are **deterministic**, **additive** (create/strengthen links, never
delete), and **non-fatal** (failures log warnings but don't roll back the
action). Current rules:

| Rule | Trigger | Effect |
|------|---------|--------|
| `rule_send_message` | `SendMessage` succeeds | Link the Contact to the related Task/Document |
| `rule_create_task` | `CreateTask` succeeds | Auto-link Task to Project if project name present in params |
| `rule_fetch_resource` | `FetchResource` succeeds | Upsert Document object for fetched URL |
| `rule_summarize_document` | `SummarizeDocument` succeeds | Store summary in Document properties |
| `rule_save_preference` | `SavePreference` succeeds | Upsert Preference object for user |
| `rule_auto_tag_important_client` | Any action | Promote Contact to "important" if interaction count ≥ threshold |
| `rule_auto_group_tasks_into_project` | Any action | Auto-create Project↔Task links based on keyword matching |
| `rule_channel_profiling` | Any action | Record per-channel interaction frequency in User properties |

### ContextBuilder: LLM Prompt Injection

The `ContextBuilder` produces a `ContextSnapshot` — a compact JSON
object injected into the LLM system prompt so the agent understands the
user's current world state:

```json
{
  "user": { "title": "Alice", "properties": { "preferred_language": "ko", … } },
  "current_context": { "title": "Office - morning", … },
  "recent_contacts": [ … ],
  "recent_tasks": [ … ],
  "recent_projects": [ … ],
  "recent_actions": [ { "action_type": "SendMessage", "status": "success", … } ]
}
```

This is triggered via `SystemPromptBuilder` in `src/agent/prompt.rs`,
which loads the ontology section including auto-injected user preferences
from `brain.db`.

### Ontology Tools (LLM Interface)

Three tools are registered in `src/tools/mod.rs` and exposed to the LLM:

| Tool Name | Purpose |
|-----------|---------|
| `ontology_get_context` | Retrieve structured snapshot of user's world state |
| `ontology_search_objects` | Search objects by type and FTS5 query |
| `ontology_execute_action` | Execute a named action (routes internally to ZeroClaw tools or ontology operations) |

### Multi-Device Sync Integration

Ontology data participates in the existing E2E encrypted sync protocol.
Three new `DeltaOperation` variants in `src/memory/sync.rs`:

| Variant | Synced Data |
|---------|------------|
| `OntologyObjectUpsert` | Object create/update deltas |
| `OntologyLinkCreate` | New link relationships |
| `OntologyActionLog` | Action execution records |

The patent's `SyncDelta.entityType` is extended with
`"structured_object"`, `"structured_link"`, and `"action_log"`.
Deduplication keys are generated in `src/sync/protocol.rs` for
idempotent replay on receiving devices.

### SQLite Schema (6 Tables + FTS5)

```sql
-- Meta-type tables
ontology_object_types (id, name, description)
ontology_link_types   (id, name, description, from_type_id, to_type_id)
ontology_action_types (id, name, description, params_schema)

-- Instance tables
ontology_objects (id, type_id, title, properties, owner_user_id, created_at, updated_at)
ontology_links   (id, link_type_id, from_object_id, to_object_id, properties, created_at)
ontology_actions (id, action_type_id, actor_user_id, actor_kind, primary_object_id,
                  related_object_ids, params, result, channel, context_id,
                  status, error_message, created_at, updated_at)

-- Full-text search on object titles + properties
ontology_objects_fts (FTS5 virtual table)
```

All tables use `IF NOT EXISTS` and coexist safely with existing memory
tables in `brain.db`.

---

## 6B. Web Chat & Homepage Integration Architecture

### Overview

MoA provides two web-based frontends in addition to the native Tauri app:

1. **Web Dashboard** (`web/`) — A full-featured management UI for
   agent chat, configuration, cost monitoring, cron jobs, device
   management, and more.
2. **Main Website / Homepage** (`site/`) — Public landing page with
   product information, pricing, and a web-chat entry point for
   authenticated users.

Both are Vite + React + TypeScript applications served independently.
They connect to the user's MoA gateway over WebSocket for real-time
communication.

### Web Dashboard (`web/`)

```
web/
├── src/
│   ├── pages/
│   │   ├── AgentChat.tsx      # Primary chat interface
│   │   ├── Config.tsx         # Agent configuration
│   │   ├── Cost.tsx           # Usage & billing dashboard
│   │   ├── Cron.tsx           # Scheduled tasks
│   │   ├── Dashboard.tsx      # Overview / home
│   │   ├── Devices.tsx        # Multi-device management & sync status
│   │   └── ...
│   ├── components/            # Shared React components
│   └── App.tsx                # Route definitions
├── vite.config.ts
└── package.json
```

### Main Website (`site/`)

```
site/
├── src/
│   ├── pages/
│   │   ├── Landing.tsx        # Homepage with product overview
│   │   ├── Pricing.tsx        # Credit packages & API key model
│   │   ├── WebChat.tsx        # Authenticated web-chat widget
│   │   └── ...
│   ├── components/
│   └── App.tsx
├── vite.config.ts
└── package.json
```

### Gateway WebSocket Endpoints (`src/gateway/`)

The ZeroClaw gateway (Axum HTTP/WebSocket server) exposes endpoints that
both the Tauri app and web frontends connect to:

| Endpoint | Module | Purpose |
|----------|--------|---------|
| `/ws/chat` | `src/gateway/ws.rs` | Real-time chat streaming (text messages, tool results) |
| `/ws/voice` | `src/gateway/ws.rs` | Voice interpretation audio streaming |
| `/api/*` | `src/gateway/api.rs` | REST API for config, memory, device management |
| `/remote/*` | `src/gateway/remote.rs` | Remote access relay for cross-device channel routing |

### Web Chat Data Flow

```
Browser (site/ or web/)
    │
    │  WebSocket connect to /ws/chat
    │  (authenticated with device token)
    ▼
Gateway (src/gateway/ws.rs)
    │
    │  Route to Agent orchestration loop
    ▼
Agent (src/agent/loop_.rs)
    │
    ├── Recall from memory (SQLite + ontology context)
    ├── Call LLM provider
    ├── Execute tools as needed
    └── Stream response tokens back via WebSocket
    │
    ▼
Browser renders streaming response
```

Users on the homepage can chat with their MoA agent without installing
the native app — the gateway handles WebSocket connections from any
authenticated browser session. Memory, ontology state, and sync all work
identically regardless of whether the client is the Tauri app or a web
browser.

---

## 6C. Document Processing & 2-Layer Editor Architecture

### Overview

MoA provides a document processing pipeline that converts PDF and Office
files into viewable and editable formats. The architecture uses a **2-layer
split-pane design** that separates the original document view from
structural editing.

### 2-Layer Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│  DocumentEditor (orchestrator)                                   │
│                                                                  │
│  ┌─────────── Left Pane (50%) ───────────┐ ┌── Right Pane (50%) ─┐
│  │  Layer 1: DocumentViewer              │ │  Layer 2: TiptapEditor│
│  │  ┌──────────────────────────────────┐ │ │  ┌──────────────────┐│
│  │  │  Sandboxed <iframe>              │ │ │  │  Tiptap WYSIWYG  ││
│  │  │  sandbox="allow-same-origin"     │ │ │  │  (Markdown-based)││
│  │  │                                  │ │ │  │                  ││
│  │  │  Original HTML (read-only)       │ │ │  │  Structural edit ││
│  │  │  from pdf2htmlEX / PyMuPDF       │ │ │  │  Bold, Heading,  ││
│  │  │                                  │ │ │  │  Table, List,    ││
│  │  │  Never modified after upload     │ │ │  │  Code, Align...  ││
│  │  └──────────────────────────────────┘ │ │  └──────────────────┘│
│  └───────────────────────────────────────┘ └─────────────────────┘│
└──────────────────────────────────────────────────────────────────┘
```

**Key design decision**: `viewer.html` is always "원본 전용" (original-only).
Edits happen exclusively in the Tiptap editor and are persisted as
Markdown + JSON. This avoids layout-breaking issues with
absolute-positioned pdf2htmlEX CSS.

### PDF Conversion Pipeline

```
                        ┌─────────────────────┐
   User uploads PDF ──▸ │  write_temp_file     │
                        │  (base64 → temp .pdf)│
                        └──────────┬──────────┘
                                   │
                        ┌──────────▼──────────┐
                        │  convert_pdf_dual    │
                        │                      │
                        │  ┌────────────────┐  │
                        │  │ pdf2htmlEX     │  │──▸ viewer_html (Layer 1)
                        │  │ (layout HTML)  │  │    absolute CSS, fonts embedded
                        │  └────────────────┘  │
                        │                      │
                        │  ┌────────────────┐  │
                        │  │ PyMuPDF        │  │──▸ markdown (Layer 2)
                        │  │ (pymupdf4llm)  │  │    structural text extraction
                        │  └────────────────┘  │
                        └──────────────────────┘

   Fallback chain:
   1. pdf2htmlEX + PyMuPDF (best quality)
   2. PyMuPDF only (convert_pdf_local — HTML + Markdown from PyMuPDF)
   3. R2 upload → Upstage OCR (image PDF / no local tools)
```

### Supported File Types

| Format | Converter | Pipeline |
|--------|-----------|----------|
| **Digital PDF** | pdf2htmlEX + PyMuPDF | Local Tauri command |
| **Image PDF** | Upstage Document OCR | Server (R2 → Railway) |
| **HWP / HWPX** | Hancom converter API | Server |
| **DOC / DOCX** | Hancom converter API | Server |
| **XLS / XLSX** | Hancom converter API | Server |
| **PPT / PPTX** | Hancom converter API | Server |

### Data Flow

```
Upload → pdf2htmlEX produces viewer.html (Layer 1)
       → PyMuPDF produces content.md    (Layer 2)

Edit   → Tiptap modifies content.md + content.json in memory
       → viewer.html stays as original (never re-rendered)

Save   → ~/.moa/documents/<filename>/
           content.md      — Markdown (primary editable content)
           content.json    — Tiptap JSON (structured document tree)
           editor.html     — HTML rendered by Tiptap (for export)

Export → .md download (Markdown from Tiptap)
       → .html download (HTML from Tiptap)
```

### Component Map

| Component | File | Responsibility |
|-----------|------|----------------|
| `DocumentEditor` | `clients/tauri/src/components/DocumentEditor.tsx` | Orchestrator: upload routing, state management, split-pane layout, save/export |
| `DocumentViewer` | `clients/tauri/src/components/DocumentViewer.tsx` | Read-only iframe renderer for original HTML output |
| `TiptapEditor` | `clients/tauri/src/components/TiptapEditor.tsx` | WYSIWYG editor with Markdown bridge (tiptap-markdown) |
| Tauri commands | `clients/tauri/src-tauri/src/lib.rs` | `write_temp_file`, `convert_pdf_dual`, `convert_pdf_local`, `save_document`, `load_document` |
| PyMuPDF script | `scripts/pymupdf_convert.py` | PDF → HTML + Markdown extraction |

### Tiptap Editor Extensions

| Extension | Purpose |
|-----------|---------|
| `StarterKit` | Paragraphs, headings (H1–H4), bold, italic, lists, blockquote, code, horizontal rule |
| `Table` (resizable) | Table insertion and editing |
| `Underline` | Underline formatting |
| `TextAlign` | Left / center / right alignment |
| `Placeholder` | Empty-state placeholder text |
| `Markdown` (tiptap-markdown) | Bidirectional Markdown ↔ ProseMirror bridge: `setContent()` parses MD, `getMarkdown()` serializes |

### AI Integration

When a document is saved, the Markdown content (up to 2000 chars) is
automatically sent to the active chat session as `[Document updated]`
context. This allows the AI agent to reference and discuss the document
content during conversation.

---

## 7. Voice / Simultaneous Interpretation

### Goal

Deliver **real-time simultaneous interpretation** that translates speech
*while the speaker is still talking*, at phrase-level granularity — not
waiting for complete sentences.

### Why This Matters

Traditional interpretation apps wait for the speaker to finish a sentence
before translating. This creates unnatural pauses and loses the speaker's
pacing and intent. MoA's simultaneous interpretation:

- Translates **phrase by phrase** as the speaker talks
- Preserves the speaker's **deliberate pauses and pacing**
- Handles **25 languages** with bidirectional auto-detection
- Supports **domain specialization** (business, medical, legal, technical)

### Architecture

```
Client mic ─▸ audio_chunk ─▸ SimulSession ─▸ Gemini 2.5 Flash Live API
                                   │
                                   ├─ InputTranscript ─▸ SegmentationEngine
                                   │                         │
                                   │            commit_src / partial_src
                                   │                         │
                                   ├─ Audio (translated) ──▸ audio_out ──▸ Client speaker
                                   └─ OutputTranscript ────▸ commit_tgt ──▸ Client subtitles
```

### Commit-Point Segmentation Engine (`src/voice/simul.rs`)

The core innovation: a **three-pointer segmentation** architecture.

```
|---committed---|---stable-uncommitted---|---unstable (may change)---|
0        last_committed      stable_end              partial_end
```

- **Committed**: Text already sent for translation. Never re-sent.
- **Stable-uncommitted**: High confidence text, not yet committed.
- **Unstable**: Trailing N characters that ASR may still revise.

#### Commit Decision Strategy (hybrid)

| Strategy | Trigger | Purpose |
|----------|---------|---------|
| **Boundary** | Punctuation (`.` `!` `?` `。` `,` `、`) | Natural language breaks |
| **Silence** | No input for `silence_commit_ms` | Speaker pauses |
| **Length cap** | Stable text > `max_uncommitted_chars` | Prevent unbounded buffering |

### WebSocket Event Protocol (`src/voice/events.rs`)

Client ↔ Server messages use JSON text frames:

**Client → Server**: `SessionStart`, `SessionStop`, `AudioChunk`,
`ActivitySignal`

**Server → Client**: `SessionReady`, `PartialSrc`, `CommitSrc`,
`PartialTgt`, `CommitTgt`, `AudioOut`, `TurnComplete`, `Interrupted`,
`Error`, `SessionEnded`

### Interpretation Modes

| Mode | Description |
|------|-------------|
| `simul` | Simultaneous: translate while speaker talks |
| `consecutive` | Wait for speaker to finish, then translate |
| `bidirectional` | Auto-detect language and interpret both ways |

### Supported Languages (25)

Korean, Japanese, Chinese (Simplified & Traditional), Thai, Vietnamese,
Indonesian, Malay, Filipino, Hindi, English, Spanish, French, German,
Italian, Portuguese, Dutch, Polish, Czech, Swedish, Danish, Russian,
Ukrainian, Turkish, Arabic

---

## 8. Coding / Multi-Model Review Pipeline

### Goal

Create an autonomous coding assistant where **Claude Opus 4.6 writes code**
and **Gemini 3.1 Pro reviews it for architecture alignment**, then Claude
validates Gemini's findings — producing self-checked, high-quality code
through AI-to-AI collaboration.

### The Pipeline

```
Code diff ──┬─▸ GeminiReviewer ─▸ ReviewReport ─┐
            │   (Architecture Gatekeeper)        │
            │   Gemini 3.1 Pro                   ▼
            └─▸ ClaudeReviewer ──────────────────┼─▸ ConsensusReport
                (Sees Gemini's findings,         │
                 validates or refutes them)       │
                Claude Opus 4.6                  ▼
                               merge findings + consensus verdict
```

### Reviewer Roles

| Reviewer | Model | Role |
|----------|-------|------|
| **GeminiReviewer** | Gemini 3.1 Pro | Architecture gatekeeper: design alignment, structural issues, efficiency |
| **ClaudeReviewer** | Claude Opus 4.6 | Implementation quality: correctness, efficiency, validates/refutes Gemini's findings |

### How It Works

1. Claude Opus 4.6 writes code and self-reviews for errors
2. Code is pushed as a PR
3. GitHub Actions triggers Gemini review automatically
4. Gemini 3.1 Pro reviews against `docs/ARCHITECTURE.md` and `CLAUDE.md`
5. Gemini posts structured findings on the PR as a comment
6. Claude reads Gemini's review → accepts valid points → pushes fixes
7. Cycle repeats until consensus is reached

### Consensus Rules

- If **any** reviewer says `REQUEST_CHANGES` → overall verdict =
  `REQUEST_CHANGES`
- If **all** reviewers say `APPROVE` → overall verdict = `APPROVE`
- Otherwise → `COMMENT`

### Severity Levels

| Level | Meaning | Example |
|-------|---------|---------|
| `CRITICAL` | Must fix: correctness/security/architecture violation | SQL injection, unsafe unwrap |
| `HIGH` | Should fix before merge | Missing error handling, SRP violation |
| `MEDIUM` | Good to fix, not blocking | Inefficient algorithm |
| `LOW` | Informational suggestion | Minor style preference |

### GitHub Actions Integration

`.github/workflows/gemini-pr-review.yml`:

1. PR opened/updated → workflow triggers
2. Extracts diff + reads `CLAUDE.md`, `docs/ARCHITECTURE.md`
3. Calls Gemini API with architecture-aware review prompt
4. Posts structured review comment on the PR
5. Comment is idempotent (updates existing, doesn't duplicate)

**Required secret**: `GEMINI_API_KEY` in repository Actions secrets.

### Coding Long-Term Memory (MoA Advantage)

**Key differentiator**: Unlike Claude Code, Cursor, or other AI coding tools
that **forget everything between sessions** due to context window limits, MoA
**persists all coding activity to long-term memory** — and **synchronizes it
in real-time** across all of the user's devices.

#### What Gets Remembered

Every coding interaction is stored in MoA's local SQLite long-term memory:

| Memory Category | Content | Example |
|----------------|---------|---------|
| `coding:session` | Full coding session transcript (prompts + responses + tool calls + results) | "User asked to refactor auth module → Claude wrote code → Gemini reviewed → 3 iterations → final commit" |
| `coding:file_change` | File diffs and change rationale | "Modified src/auth/jwt.rs: added token refresh, reason: session expiry bug #142" |
| `coding:architecture_decision` | Design decisions and trade-offs discussed | "Chose SQLite over Postgres for memory backend because: local-first, no server dependency, mobile-compatible" |
| `coding:error_pattern` | Errors encountered and how they were resolved | "Borrow checker error in sync.rs → resolved by Arc<Mutex<>> wrapping" |
| `coding:review_finding` | Code review findings from Gemini/Claude | "Gemini flagged: missing error handling in gateway webhook → Claude fixed with proper bail!()" |
| `coding:project_context` | Project structure, conventions, patterns learned | "This project uses trait+factory pattern, snake_case modules, PascalCase types" |

#### How It Works

```
1. User gives coding instruction to MoA
   ↓
2. MoA (ZeroClaw agent) executes coding pipeline:
   Claude writes → Gemini reviews → consensus → commit
   ↓
3. EVERY step is auto-saved to local SQLite long-term memory:
   - The original instruction
   - All code generated/modified (full diffs)
   - Tool calls (shell commands, file reads/writes)
   - Review feedback from Gemini/Claude
   - Final commit message and files changed
   - Errors encountered and resolutions
   ↓
4. Memory is tagged with:
   - category: "coding"
   - project: repository name
   - session_id: unique coding session
   - timestamp: when it happened
   ↓
5. Real-time sync to all user's other MoA devices:
   - Delta encrypted → relay server → other devices apply
   - User can continue coding on another device with FULL context
```

#### Cross-Device Coding Continuity

```
Device A (Desktop, morning)          Device B (Laptop, evening)
┌────────────────────────┐          ┌────────────────────────┐
│ MoA codes auth module  │──sync──▸│ MoA remembers ALL of   │
│ 3 sessions, 47 files   │          │ Device A's coding work │
│ stored in SQLite memory│          │                        │
└────────────────────────┘          │ User: "Continue the    │
                                    │ auth module from this  │
                                    │ morning"               │
                                    │                        │
                                    │ MoA: "I recall the 3   │
                                    │ sessions. Last change  │
                                    │ was jwt.rs refresh     │
                                    │ token. Shall I proceed │
                                    │ with the OAuth2 flow?" │
                                    └────────────────────────┘
```

#### Why This Matters

| Traditional AI Coding Tool | MoA |
|---------------------------|-----|
| Forgets after session ends | Remembers everything permanently |
| Context window limit (~200K tokens) | Unlimited via SQLite + RAG retrieval |
| Single device only | Multi-device synced memory |
| No cross-session continuity | Full project history recalled |
| Manual context loading (paste code) | Automatic recall from memory |

**Implementation**: The agent loop (`src/agent/loop_.rs`) auto-saves coding
sessions to memory. The `SyncedMemory` wrapper ensures deltas propagate to
other devices via the 3-tier sync protocol.

---

## 9. Coding Sandbox (Run → Observe → Fix)

### Six-Phase Methodology

| Phase | Purpose | Key Actions |
|-------|---------|-------------|
| **1. Comprehend** | Understand before changing | Read existing code, identify patterns |
| **2. Plan** | Define scope | Acceptance criteria, minimal approach |
| **3. Prepare** | Set up environment | Snapshot working state, install deps |
| **4. Implement** | Write + verify | Code → run → observe → classify errors → fix → repeat |
| **5. Validate** | Final checks | Format, lint, type-check, build, full test suite |
| **6. Deliver** | Ship | Commit with clear message, report results |

### Recurring Error Detection

If the same error class appears **3+ times**, the sandbox:
1. **Rolls back** to last checkpoint
2. **Switches strategy** (alternative approach)
3. **Escalates** to user if strategies exhausted

---

## 10. Configuration Reference

### VoiceConfig

```toml
[voice]
enabled = true
max_sessions_per_user = 5
default_source_language = "ko"
default_target_language = "en"
default_interp_mode = "simul"      # simul | consecutive | bidirectional
min_commit_chars = 10
max_uncommitted_chars = 80
silence_commit_ms = 600
silence_duration_ms = 300
prefix_padding_ms = 100
# gemini_api_key = "..."           # or GEMINI_API_KEY env var
# openai_api_key = "..."           # or OPENAI_API_KEY env var
# default_provider = "gemini"      # gemini | openai
```

### CodingConfig

```toml
[coding]
review_enabled = false             # Enable multi-model review
gemini_model = "gemini-2.5-flash"  # Upgrade to gemini-3.1-pro when available
claude_model = "claude-sonnet-4-6"
enable_secondary_review = true     # Claude validates Gemini's findings
max_diff_chars = 120000
# gemini_api_key = "..."           # or GEMINI_API_KEY env var
# claude_api_key = "..."           # or ANTHROPIC_API_KEY env var
```

---

## 11. Patent-Relevant Innovation Areas

### Innovation 1: Server-Non-Storage E2E Encrypted Memory Sync

See [Section 3](#3-patent-server-non-storage-e2e-encrypted-memory-sync)
for full specification.

**Claims**: Delta-based sync, 5-minute TTL relay, zero-knowledge server,
device-local authoritative storage, offline reconciliation.

### Innovation 2: Commit-Point Segmentation for Simultaneous Interpretation

Real-time phrase-level audio translation using a three-pointer architecture
(committed | stable-uncommitted | unstable) with hybrid boundary detection
(punctuation, silence, length-cap). Enables translation to begin **before
the speaker finishes a sentence**.

### Innovation 3: Multi-Model Consensus Code Review Pipeline

Automated code quality assurance where Model A (Claude) generates code,
Model B (Gemini) reviews for architecture alignment, Model A validates
Model B's findings, and a pipeline merges findings with severity-weighted
deduplication. AI models **autonomously discuss and refine** code quality.

### Innovation 4: Task-Category-Aware Tool Routing

Dynamic tool availability per task category — each category exposes only
the tools relevant to its domain, reducing attack surface and improving
model focus. The coding category gets all tools; the translation category
gets minimal tools.

### Innovation 5: Six-Phase Structured Coding with Autonomous Repair Loop

Comprehend → Plan → Prepare → Implement (run→observe→fix) → Validate →
Deliver, with error classification, recurring-error detection, rollback
checkpoints, and multi-signal observation (exit code + stderr + server
health + DOM snapshots).

### Innovation 6: Structured Relational Memory (Digital Twin Graph)

A typed Object/Link/Action graph layer that models the user's real world
as a digital twin, sitting above the episodic memory (SQLite FTS5 + vec).
The graph is maintained automatically by a deterministic rule engine that
fires after every successful action — creating links, promoting objects,
and profiling channels without explicit LLM orchestration. Combined with
the E2E encrypted sync protocol, the structured graph synchronizes across
all user devices as first-class delta operations.

---

## 12. Design Principles

These are **mandatory constraints**, not guidelines:

| Principle | Rule |
|-----------|------|
| **KISS** | Prefer straightforward control flow over clever meta-programming |
| **YAGNI** | No speculative features — concrete accepted use case required |
| **DRY + Rule of Three** | Extract shared logic only after 3+ repetitions |
| **SRP + ISP** | One concern per module, narrow trait interfaces |
| **Fail Fast** | Explicit errors for unsupported states, never silently broaden |
| **Secure by Default** | Deny-by-default, no secret logging, minimal exposure |
| **Determinism** | Reproducible behavior, no flaky tests |
| **Reversibility** | Small commits, clear rollback paths |

---

## 13. Risk Tiers

| Tier | Scope | Review depth |
|------|-------|--------------|
| **Low** | docs, chore, tests-only | Lightweight checks |
| **Medium** | Most `src/**` behavior changes | Standard review |
| **High** | `src/security/**`, `src/runtime/**`, `src/gateway/**`, `src/tools/**`, `.github/workflows/**`, `src/sync/**`, `src/ontology/**` | Full validation + boundary testing |

---

## 14. Technology Stack

| Component | Technology |
|-----------|-----------|
| **Language** | Rust (edition 2021, MSRV 1.87) |
| **Async runtime** | Tokio |
| **App framework** | Tauri 2.x (desktop + mobile) |
| **HTTP client** | reqwest |
| **WebSocket** | tungstenite 0.28 |
| **Serialization** | serde + serde_json |
| **CLI** | clap |
| **Database** | SQLite (rusqlite) + sqlite-vec + FTS5 |
| **AI Models** | Gemini (Google), Claude (Anthropic), OpenAI, Ollama |
| **Default LLM** | Gemini 3.1 Flash Lite (cost-effective default for chat; task-based routing for other categories) |
| **Voice/Interp** | Gemini 2.5 Flash Native Audio (Live API) |
| **Coding review** | Claude Opus 4.6 + Gemini 3.1 Pro |
| **Document viewer** | pdf2htmlEX (layout-preserving PDF→HTML) |
| **Document editor** | Tiptap (ProseMirror) + tiptap-markdown bridge |
| **PDF extraction** | PyMuPDF / pymupdf4llm (structure→Markdown) |
| **Document OCR** | Upstage Document AI (image PDF fallback) |
| **Office conversion** | Hancom API (HWP, DOCX, XLSX, PPTX) |
| **Relay server** | Railway (WebSocket relay, no persistent storage) |
| **Encryption** | AES-256-GCM (vault, sync), ChaCha20-Poly1305 (secrets), HKDF key derivation |
| **CI** | GitHub Actions |

---

## 15. Implementation Roadmap

### Completed

- [x] ZeroClaw upstream sync (1692 commits merged)
- [x] Task category system with tool routing (7 categories)
- [x] Voice pipeline with 25-language support
- [x] Gemini Live WebSocket client with automatic VAD
- [x] Simultaneous interpretation segmentation engine
- [x] WebSocket event protocol for client-server communication
- [x] SimulSession manager (audio forwarding + event processing)
- [x] Multi-model code review pipeline (Gemini + Claude)
- [x] GitHub Actions Gemini PR review workflow
- [x] Coding sandbox 6-phase methodology
- [x] Translation UI manifest for frontend
- [x] Credit-based billing system
- [x] Architecture documentation (this document)

### Recently Completed (2026-03-02)

- [x] KakaoTalk channel implementation (550+ lines, full send/listen/webhook)
- [x] E2E encrypted memory sync (patent implementation — SyncCoordinator + SyncEngine)
- [x] RelayClient wire-up to gateway (cross-device delta exchange via WebSocket)
- [x] Web chat WebSocket streaming (client + server /ws/chat endpoint)
- [x] WebSocket gateway endpoint for voice interpretation (/ws/voice)
- [x] Coding review refactored to use ReviewPipeline (structured consensus)
- [x] Tauri sidecar auto-retry UX (3 attempts, 30s timeout, transparent to user)

### Recently Completed (2026-03-09)

- [x] Structured relational memory (ontology digital twin graph) — `src/ontology/` (types, schema, repo, dispatcher, rules, context, tools)
- [x] Ontology tool integration (3 tools registered in `src/tools/mod.rs`)
- [x] System prompt ontology section + preference auto-injection (`src/agent/prompt.rs`)
- [x] Ontology delta sync integration (3 new DeltaOperation variants in `src/memory/sync.rs`)
- [x] Sync dedup keys for ontology deltas (`src/sync/protocol.rs`)
- [x] Web dashboard (`web/` — Vite + React + TypeScript)
- [x] Main website / homepage (`site/` — Vite + React + TypeScript)
- [x] Patent dependent claims 14–18 for structured relational memory (`docs/ephemeral-relay-sync-patent.md`)

### Recently Completed (2026-03-14)

- [x] 2-layer document editor architecture (viewer + Tiptap editor split-pane) — `DocumentEditor.tsx`, `DocumentViewer.tsx`, `TiptapEditor.tsx`
- [x] PDF dual conversion pipeline (pdf2htmlEX for viewer + PyMuPDF for editor) — `convert_pdf_dual` Tauri command in `lib.rs`
- [x] Document persistence to filesystem — `save_document`/`load_document` Tauri commands (`~/.moa/documents/`)
- [x] Tiptap rich-text editor with Markdown bridge — StarterKit, Table, Underline, TextAlign, Placeholder, tiptap-markdown
- [x] Office document processing via Hancom API — HWP, HWPX, DOC, DOCX, XLS, XLSX, PPT, PPTX
- [x] Image PDF fallback via R2 + Upstage Document OCR — server-side processing for scanned PDFs
- [x] Markdown/HTML export from editor — `.md` and `.html` download buttons

### Recently Completed (2026-03-03)

- [x] Railway relay server deployment (5-minute TTL buffer) — `src/sync/relay.rs` SyncRelay + RelayClient, `deploy/railway/` config
- [x] Offline reconciliation / peer-to-peer full sync — `src/sync/coordinator.rs` Layer 2 (delta journal) + Layer 3 (manifest-based full sync)
- [x] Tauri desktop app with bundled sidecar (Windows, macOS, Linux) — `clients/tauri/` with Tauri 2.x, externalBin, multi-platform bundles
- [x] Tauri mobile app with bundled runtime (iOS, Android) — Swift/Kotlin entry points, `mobile-setup.sh`, multi-ABI Gradle config
- [x] One-click installer with first-run GUI setup wizard — `zeroclaw_install.sh` CLI + `SetupWizard.tsx` 4-step GUI wizard
- [x] Unified auto-updater (Tauri updater — frontend + sidecar atomically) — `tauri.conf.json` updater plugin configured with endpoint + dialog
- [x] User settings page (API key input, device management) — `Settings.tsx` (558 lines) with API keys, device list, sync status, language
- [x] Operator API key fallback with 2.2× credit billing — `src/billing/llm_router.rs` resolve_key() + 2.2× credit multiplier (2× margin + VAT) with tests
- [x] Credit balance display in app UI — Settings component credit section with 4-tier purchase packages
- [x] Gatekeeper SLM integration (Ollama-based local inference) — `src/gatekeeper/router.rs` GatekeeperRouter with Ollama API, keyword classification, offline queue
- [x] Channel-specific voice features (KakaoTalk, Telegram, Discord) — `src/channels/voice_features.rs` with platform-specific parsers, downloaders, capability descriptors
- [x] Multi-user simultaneous interpretation (conference mode) — `src/voice/conference.rs` ConferenceRoom + ConferenceManager with multi-participant audio broadcast
- [x] Coding sandbox integration with review pipeline — `src/coding/sandbox_bridge.rs` SandboxReviewBridge connecting ReviewPipeline to sandbox fix actions
- [x] Automated fix-apply from review findings — `src/coding/auto_fix.rs` FixPlan generator converting review findings to FileEdit/ShellCommand/LlmAssisted instructions
- [x] Image/Video/Music generation tool integrations — `src/tools/media_gen.rs` ImageGenTool (DALL-E), VideoGenTool (Runway), MusicGenTool (Suno)
- [x] iOS native bridge (Swift-Rust FFI) — Tauri 2 manages Rust↔Swift bridge transparently, `MoAApp.swift` entry point with WKWebView
- [x] Android NDK sidecar build — Gradle multi-ABI (arm64-v8a, armeabi-v7a, x86, x86_64), ProGuard config, SDK 34

---

## 16. For AI Reviewers

When reviewing a PR against this architecture:

1. **Check architecture alignment**: Does the change follow the trait-driven
   pattern? Does it belong in the right module?
2. **Check design principles**: KISS, YAGNI, SRP, fail-fast,
   secure-by-default
3. **Check MoA-specific contracts**: Voice segmentation parameters, event
   protocol compatibility, category tool routing, memory sync protocol
4. **Check risk tier**: High-risk paths (`security/`, `gateway/`, `tools/`,
   `workflows/`, `sync/`) need extra scrutiny
5. **Check backward compatibility**: Config keys are public API — changes
   need migration documentation
6. **Check platform independence**: Code must work on all 5 platforms
   (Windows, macOS, Linux, Android, iOS) — avoid platform-specific
   assumptions unless behind a `cfg` gate
7. **Check memory sync contract**: Any change to `memory/`, `sync/`, or
   `ontology/` must preserve the delta-based, E2E encrypted,
   server-non-storage invariants. Ontology deltas sync via the same
   protocol as episodic memory deltas
8. **Check API key handling**: Never log API keys, never send them to the
   relay server, always handle both user-key and operator-key paths
9. **Check unified app contract**: MoA and ZeroClaw must remain a single
   inseparable app from the user's perspective. No change may expose the
   sidecar architecture to end users (no separate install steps, no
   "ZeroClaw" branding in user-facing UI, no manual process management).
   Sidecar IPC overhead must stay below 1ms per round-trip.
