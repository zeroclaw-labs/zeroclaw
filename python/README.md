# zeroclaw-tools

Python companion package for [ZeroClaw](https://github.com/zeroclaw-labs/zeroclaw) — LangGraph-based tool calling for consistent LLM agent execution.

## Why This Package?

Some LLM providers (particularly GLM-5/Zhipu and similar models) have inconsistent tool calling behavior when using text-based tool invocation. This package provides a LangGraph-based approach that delivers:

- **Consistent tool calling** across all OpenAI-compatible providers
- **Automatic tool loop** — keeps calling tools until the task is complete
- **Easy extensibility** — add new tools with a simple `@tool` decorator
- **Framework agnostic** — works with any OpenAI-compatible API

## Installation

```bash
pip install zeroclaw-tools
```

With Discord integration:

```bash
pip install zeroclaw-tools[discord]
```

## Quick Start

### Basic Agent

```python
import asyncio
from zeroclaw_tools import create_agent, shell, file_read, file_write
from langchain_core.messages import HumanMessage

async def main():
    # Create agent with tools
    agent = create_agent(
        tools=[shell, file_read, file_write],
        model="glm-5",
        api_key="your-api-key",
        base_url="https://api.z.ai/api/coding/paas/v4"
    )
    
    # Execute a task
    result = await agent.ainvoke({
        "messages": [HumanMessage(content="List files in /tmp directory")]
    })
    
    print(result["messages"][-1].content)

asyncio.run(main())
```

### CLI Usage

```bash
# Set environment variables
export API_KEY="your-api-key"
export API_BASE="https://api.z.ai/api/coding/paas/v4"

# Run the CLI
zeroclaw-tools "List files in the current directory"

# Interactive mode (no message required)
zeroclaw-tools -i
```

### Discord Bot

```python
import os
from zeroclaw_tools.integrations import DiscordBot

bot = DiscordBot(
    token=os.environ["DISCORD_TOKEN"],
    guild_id=123456789,
    allowed_users=["123456789"]
)

bot.run()
```

## Available Tools

| Tool | Description |
|------|-------------|
| `shell` | Execute shell commands |
| `file_read` | Read file contents |
| `file_write` | Write content to files |
| `web_search` | Search the web (requires Brave API key) |
| `http_request` | Make HTTP requests |
| `memory_store` | Store data in memory |
| `memory_recall` | Recall stored data |

## Creating Custom Tools

```python
from zeroclaw_tools import tool

@tool
def my_custom_tool(query: str) -> str:
    """Description of what this tool does."""
    # Your implementation here
    return f"Result for: {query}"

# Use with agent
agent = create_agent(tools=[my_custom_tool])
```

## Provider Compatibility

Works with any OpenAI-compatible provider:

- **Z.AI / GLM-5** — `https://api.z.ai/api/coding/paas/v4`
- **OpenRouter** — `https://openrouter.ai/api/v1`
- **Groq** — `https://api.groq.com/openai/v1`
- **DeepSeek** — `https://api.deepseek.com`
- **Ollama** — `http://localhost:11434/v1`
- **And many more...**

## Architecture

```
┌─────────────────────────────────────────────┐
│              Your Application               │
├─────────────────────────────────────────────┤
│           zeroclaw-tools Agent              │
│  ┌─────────────────────────────────────┐   │
│  │         LangGraph StateGraph         │   │
│  │    ┌───────────┐    ┌──────────┐    │   │
│  │    │   Agent   │───▶│   Tools  │    │   │
│  │    │   Node    │◀───│   Node   │    │   │
│  │    └───────────┘    └──────────┘    │   │
│  └─────────────────────────────────────┘   │
├─────────────────────────────────────────────┤
│        OpenAI-Compatible LLM Provider       │
└─────────────────────────────────────────────┘
```

## Comparison with Rust ZeroClaw

| Feature | Rust ZeroClaw | zeroclaw-tools |
|---------|---------------|----------------|
| **Binary size** | ~3.4 MB | Python package |
| **Memory** | <5 MB | ~50 MB |
| **Startup** | <10ms | ~500ms |
| **Tool consistency** | Model-dependent | LangGraph guarantees |
| **Extensibility** | Rust traits | Python decorators |

Use **Rust ZeroClaw** for production edge deployments. Use **zeroclaw-tools** when you need guaranteed tool calling consistency or Python ecosystem integration.

## License

MIT License — see [LICENSE](../LICENSE)
