# EverOS memory backend

ZeroClaw can use [EverOS](https://github.com/EverMind-AI/EverOS) as its pluggable
memory store. EverOS stays a separate local-first Markdown + SQLite + LanceDB
sidecar; ZeroClaw talks to it over HTTP (`/api/v2/memory/*`) through the
`EverOSMemory` backend.

This backend is available on the EverOS-integration fork
(`feat/everos-memory-backend`). Switching `memory.backend` does **not** migrate
existing SQLite or Markdown data (see [ADR-005](./decisions/ADR-005-pluggable-memory-backends.md)).

## Architecture

- ZeroClaw owns the agent loop (providers, channels, tools, sandbox).
- EverOS owns durable memory (Markdown source of truth, hybrid search, reflection).
- Glue: `memory.backend = "everos.<alias>"` → `[storage.everos.<alias>]` →
  `EverOSMemory` implementing the shared `Memory` trait.
- Optional SQLite hot cache (`local_cache = true`, default) absorbs EverOS
  downtime and index lag after flush.

## Prerequisites

1. Python 3.12+ and [EverOS](https://github.com/EverMind-AI/EverOS) installed
   (`uv pip install everos` or a local checkout).
2. An LLM key for EverOS extraction (OpenRouter is enough for keyword-tier
   setups; add embeddings for `vector` / `hybrid` search).

```bash
everos init
# edit ~/.everos/everos.toml — set [llm].api_key
everos server start   # http://127.0.0.1:8000
curl http://127.0.0.1:8000/health
```

## ZeroClaw config

```toml
[memory]
backend = "everos.default"

[storage.everos.default]
base_url = "http://127.0.0.1:8000"
app_id = "zeroclaw"
project_id = "default"
user_id = "local"
timeout_ms = 5000
search_method = "hybrid"   # keyword | vector | hybrid | agentic
local_cache = true
```

Use `search_method = "keyword"` when EverOS embeddings are not configured.

## Sidecar with Compose

From the ZeroClaw repo root:

```bash
docker compose -f docker-compose.everos.yml up -d
```

That file starts EverOS on loopback port 8000. Point ZeroClaw at it with the
config above, then run your agent as usual:

```bash
zeroclaw agent -a <alias>
```

## Behavior notes

| Operation | EverOS mapping |
| --- | --- |
| store | `POST /api/v2/memory/add` + `flush` |
| recall | `POST /api/v2/memory/search` |
| list / get | `POST /api/v2/memory/get` (+ search for key lookup) |
| forget / purge | delete endpoints; warn and keep cache if unsupported |
| principal-plane | fail closed in v1 |

Stored keys are embedded in message content as
`[zeroclaw key=<key> category=<cat>]` so recalls can round-trip ZeroClaw keys.

When EverOS is unreachable, writes still land in the SQLite cache (if enabled)
and recalls fall back to that cache for a short cooldown window.

## Manual check

1. Start EverOS (`everos server start` or compose).
2. Configure ZeroClaw as above.
3. Store a memory through the agent or `memory_store` tool.
4. Confirm Markdown under `~/.everos/` (or your EverOS root).
5. Recall the same content through the agent.
