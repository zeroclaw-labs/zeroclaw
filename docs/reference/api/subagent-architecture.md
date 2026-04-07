# Sub-Agent Architecture

ZeroClaw supports dynamic multi-agent orchestration through two complementary mechanisms: **workspace agents** (file-based, persistent) and **ad-hoc agents** (inline, ephemeral). Both share a common execution engine and can coexist.

## Overview

```
┌─────────────────────────────────────────────────────┐
│                   Main Agent                         │
│                                                      │
│  ┌──────────────┐  ┌──────────────┐                 │
│  │  delegate     │  │ spawn_agent  │                 │
│  │  (pre-defined)│  │ (ad-hoc)     │                 │
│  └──────┬───────┘  └──────┬───────┘                 │
│         │                  │                         │
│         ▼                  ▼                         │
│  ┌─────────────────────────────────┐                │
│  │   Shared Agent Registry          │                │
│  │   Arc<RwLock<HashMap>>           │                │
│  ├─────────────────────────────────┤                │
│  │ config.toml [agents.*]           │                │
│  │ workspace/agents/*/config.toml   │  ◄── hot-reload│
│  │ ephemeral (spawn_agent)          │                │
│  └──────────────┬──────────────────┘                │
│                 │                                    │
│                 ▼                                    │
│  ┌─────────────────────────────────┐                │
│  │   run_tool_call_loop()           │                │
│  │   (isolated context per agent)   │                │
│  └─────────────────────────────────┘                │
└─────────────────────────────────────────────────────┘
```

## Workspace Agents (Definition-Based)

Workspace agents are defined as folders under `workspace/agents/`:

```
workspace/agents/
├── researcher/
│   ├── config.toml       # Agent configuration
│   ├── IDENTITY.md       # System prompt / persona
│   ├── TOOLS.md          # Tool usage guidelines (optional)
│   └── skills/           # Agent-scoped skills
│       └── deep_search.md
├── coder/
│   ├── config.toml
│   └── IDENTITY.md
└── common/               # Shared across all agents
    ├── SAFETY.md
    └── skills/
        └── memory_ops.md
```

### Agent config.toml

Each agent folder contains a `config.toml` with these fields:

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `provider` | string | global default | LLM provider (e.g. "openai", "anthropic", "ollama") |
| `model` | string | global default | Model name |
| `temperature` | float | global default | Sampling temperature |
| `agentic` | bool | `true` | Enable multi-turn tool-call loop |
| `allowed_tools` | string[] | all safe tools | Tool allowlist |
| `max_iterations` | int | `10` | Max tool-call iterations |
| `max_depth` | int | `3` | Max nested delegation depth |
| `timeout_secs` | int | `120` | Non-agentic call timeout |
| `agentic_timeout_secs` | int | `300` | Agentic run timeout |
| `memory_namespace` | string | agent name | Memory isolation key |
| `max_context_tokens` | int | `0` (unlimited) | Context window budget |
| `max_tool_result_chars` | int | `0` (unlimited) | Tool result truncation |

### Identity and Prompt Construction

The system prompt for a workspace agent is assembled from:

1. **common/*.md** — Shared context (safety rules, guidelines)
2. **IDENTITY.md** — Agent persona and instructions
3. **TOOLS.md** — Tool usage guidelines (optional)
4. **Skills** — From `skills/` + `common/skills/`
5. **Tool schemas** — Auto-injected based on `allowed_tools`

### Hot-Reload

A file watcher monitors `workspace/agents/` for changes:
- **Create/Modify** `.toml` or `.md` → agent reloaded automatically
- **Delete** agent folder → agent removed from registry
- **common/ changes** → all workspace agents reloaded
- Config-defined agents (`[agents.*]`) are never overwritten by workspace agents

## Ad-Hoc Agents (spawn_agent)

The `spawn_agent` tool creates ephemeral agents on the fly:

```json
{
  "name": "temp-researcher",
  "system_prompt": "You are a research specialist...",
  "prompt": "Find information about X",
  "provider": "openai",
  "model": "gpt-4o",
  "allowed_tools": ["web_search", "web_fetch"],
  "mode": "sync",
  "save": false
}
```

### Parameters

| Parameter | Required | Default | Description |
|-----------|----------|---------|-------------|
| `name` | yes | — | Unique agent identifier |
| `system_prompt` | yes | — | Agent persona and instructions |
| `prompt` | yes | — | Task to execute |
| `provider` | no | global default | LLM provider override |
| `model` | no | global default | Model override |
| `allowed_tools` | no | safe defaults | Tool allowlist |
| `context` | no | — | Context prepended to prompt |
| `mode` | no | `"sync"` | `"sync"` or `"background"` |
| `save` | no | `false` | Persist to workspace after execution |

### Persistence

When `save: true`, the agent is written to `workspace/agents/<name>/`:
- `config.toml` — provider, model, tools, settings
- `IDENTITY.md` — system_prompt content

The file watcher picks it up automatically, making it a permanent workspace agent.

## Static Agents (config.toml)

Agents can also be defined in the main `config.toml`:

```toml
[agents.summarizer]
provider = "openai"
model = "gpt-4o-mini"
system_prompt = "You are a concise summarizer."
agentic = true
allowed_tools = ["file_read", "web_fetch"]
max_iterations = 5
max_context_tokens = 8000
```

Static agents take priority over workspace agents with the same name.

## Concurrency Control

Sub-agents share LLM backend capacity with the main agent. To prevent slot saturation:

```toml
[delegate]
max_concurrent_subagents = 2  # 0 = unlimited (default)
```

When set, a shared semaphore limits how many sub-agents (delegate + spawn) execute simultaneously. Excess requests wait for a permit rather than failing.

**Recommended settings by backend capacity:**

| LLM Slots | Recommended `max_concurrent_subagents` |
|-----------|---------------------------------------|
| 1-2       | 1                                     |
| 3-4       | 2                                     |
| 5+        | 3-4                                   |
| Cloud API | 0 (unlimited)                         |

## Context Isolation

Each sub-agent runs with its own:
- **History** — Independent conversation context
- **System prompt** — Built from its own IDENTITY.md + tools
- **Skills** — Loaded from its own `skills/` directory
- **Memory namespace** — Isolated from other agents
- **Context budget** — Per-agent `max_context_tokens`
- **Tool set** — Filtered by `allowed_tools`

Sub-agents do NOT share:
- Conversation history with the main agent
- Each other's memory namespaces
- Tool execution state

## Invocation

### From the main agent (LLM decides):
```
delegate(agent="researcher", prompt="Find info about X")
spawn_agent(name="helper", system_prompt="...", prompt="Do Y")
```

### Background execution:
```
delegate(agent="researcher", prompt="...", background=true)
spawn_agent(name="helper", ..., mode="background")
```

Background tasks return a `task_id`. Results are retrieved with:
```
delegate(action="check_result", task_id="<uuid>")
delegate(action="list_results")
```

## Security

- Sub-agents inherit the root `SecurityPolicy`
- `workspace_only = true` restricts file access to the workspace
- Sub-agents run without an `ApprovalManager` (tools execute directly)
- The `allowed_tools` allowlist is the primary access control
- `"delegate"` and `"spawn_agent"` are excluded from sub-agent tool sets to prevent recursive spawning
