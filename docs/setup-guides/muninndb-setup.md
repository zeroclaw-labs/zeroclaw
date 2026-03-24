# MuninnDB Memory Backend

MuninnDB is a cognitive memory engine with semantic search, Hebbian reinforcement, and Ebbinghaus decay. When used as ZeroClaw's memory backend, all embedding and recall logic runs server-side — no local embedder required.

## Install MuninnDB

### From binary

```bash
# Linux / macOS
curl -fsSL https://github.com/muninndb/muninndb/releases/latest/download/muninn-$(uname -s)-$(uname -m) -o /usr/local/bin/muninndb-server
chmod +x /usr/local/bin/muninndb-server
```

### From source

```bash
git clone https://github.com/muninndb/muninndb.git
cd muninndb
go build -o muninndb-server ./cmd/muninn/
sudo mv muninndb-server /usr/local/bin/
```

## Quick start

```bash
# Initialize (interactive — configures embedder, ports, auth)
muninndb-server init

# Start all services (REST API, MCP, Web UI)
muninndb-server start
```

Default ports:

| Service | Port |
|---------|------|
| REST API | 8475 |
| MCP (SSE) | 8750 |
| Web UI | 8476 |

## Configure ZeroClaw

```toml
[memory]
backend = "muninndb"

[memory.muninndb]
url = "http://127.0.0.1:8475"
vault = "default"
# api_key = "mk_..."  # required for non-default vaults
```

Environment variable overrides (take effect when config value is empty/default):

| Env var | Overrides |
|---------|-----------|
| `MUNINNDB_URL` | `url` |
| `MUNINNDB_VAULT` | `vault` |
| `MUNINNDB_API_KEY` | `api_key` |

## Vault isolation (multi-persona)

Each ZeroClaw instance can use a separate vault on the same MuninnDB server. Useful for A2A multi-agent setups where each persona needs isolated memory.

```bash
# Create vaults and API keys
muninndb-server vault create researcher
muninndb-server api-key create --vault researcher

muninndb-server vault create coder
muninndb-server api-key create --vault coder
```

Then in each persona's `config.toml`:

```toml
# researcher
[memory.muninndb]
vault = "researcher"
api_key = "mk_..."

# coder
[memory.muninndb]
vault = "coder"
api_key = "mk_..."
```

The `default` vault is open (no API key needed). All other vaults require a key.

## Embeddings

MuninnDB embeds engrams server-side. Configure an embedder via environment variables in `~/.muninn/muninn.env`:

```bash
# Pick one:
MUNINN_GOOGLE_KEY=AIza...          # Google (gemini-embedding-001)
MUNINN_OPENAI_KEY=sk-...           # OpenAI (text-embedding-3-small)
MUNINN_VOYAGE_KEY=pa-...           # Voyage AI
MUNINN_COHERE_KEY=...              # Cohere (embed-v4)
```

Restart MuninnDB after changing the env file. Existing engrams are retroactively embedded on startup.

## LLM enrichment (optional)

MuninnDB can auto-generate summaries, key points, and entity extraction for stored memories:

```bash
# In ~/.muninn/muninn.env
MUNINN_ENRICH_URL=google://gemini-2.5-flash
# Uses MUNINN_GOOGLE_KEY for auth, or set MUNINN_ENRICH_API_KEY separately
```

## Verify

```bash
# Health check
curl http://127.0.0.1:8475/api/health

# Store a test memory via ZeroClaw
# (ZeroClaw will use the configured muninndb backend)
curl http://127.0.0.1:8475/api/stats?vault=default
```
