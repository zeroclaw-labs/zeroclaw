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

## Using Agents

Once defined, agents can be invoked by the main agent:

```
delegate(agent="assistant", prompt="Summarize the README.md file")
```

Or created on-the-fly and persisted:

```
spawn_agent(name="researcher", system_prompt="...", prompt="...", save=true)
```
