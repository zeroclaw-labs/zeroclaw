# Workspace Agent Examples

This directory contains example agent definitions for the ZeroClaw workspace agent system.

## Quick Start

1. Copy an agent folder to your workspace:
   ```bash
   cp -r examples/workspace-agents/assistant /your-workspace/agents/assistant
   cp -r examples/workspace-agents/common /your-workspace/agents/common
   ```

2. Customize `config.toml` with your provider and model preferences.

3. Edit `IDENTITY.md` to define the agent's persona and behavior.

4. (Optional) Add agent-specific skills in the `skills/` subfolder.

5. ZeroClaw will detect the new agent automatically via hot-reload.

## Structure

```
workspace/agents/
├── assistant/              # An example general-purpose agent
│   ├── config.toml         # Agent configuration (provider, model, tools)
│   ├── IDENTITY.md         # System prompt / persona
│   ├── TOOLS.md            # Tool usage guidelines (optional)
│   └── skills/             # Agent-scoped skills
│       └── summarize.md
├── researcher/             # An example research-focused agent
│   ├── config.toml         # Tuned for web research (generous timeouts, research tools)
│   ├── IDENTITY.md         # Research specialist persona with structured output format
│   ├── TOOLS.md            # Guidelines for web_search, web_fetch, and memory tools
│   └── skills/             # Agent-scoped skills
│       └── deep_search.md  # Multi-source deep research with citation tracking
├── common/                 # Shared across ALL workspace agents
│   ├── SAFETY.md           # Shared safety rules
│   └── skills/
│       └── memory_ops.md   # Shared skills
└── <your-agent>/           # Create your own!
    ├── config.toml
    └── IDENTITY.md
```

## Creating a New Agent

1. Create a new folder under `workspace/agents/`
2. Add a `config.toml` (copy from `assistant/config.toml` as template)
3. Add an `IDENTITY.md` with the agent's role and instructions
4. Optionally add `TOOLS.md` and a `skills/` directory
5. The agent is immediately available via the `delegate` or `spawn_agent` tools

## Per-Agent Context Configuration

Two fields in `config.toml` let you control how much data each agent processes, which is especially useful for agents that consume large tool outputs (e.g. web pages) or run on models with smaller context windows:

| Field | Default | Purpose |
|---|---|---|
| `max_context_tokens` | unset (global limit) | Cap on total tokens sent in the agent context window. Oldest messages are truncated when the limit is exceeded. |
| `max_tool_result_chars` | unset (global limit) | Maximum characters retained from each tool result. Longer outputs are truncated with a summary marker. |

Example — a researcher agent with conservative context limits:

```toml
# researcher/config.toml
max_context_tokens = 32000
max_tool_result_chars = 5000

agentic = true
max_iterations = 20
allowed_tools = ["web_search", "web_fetch", "file_read", "file_write"]
agentic_timeout_secs = 600
```

These settings override the global values from your main `config.toml` for that agent only.

## Using Agents

Once defined, agents can be invoked by the main agent:

```
delegate(agent="assistant", prompt="Summarize the README.md file")
```

Or created on-the-fly and persisted:

```
spawn_agent(name="researcher", system_prompt="...", prompt="...", save=true)
```
