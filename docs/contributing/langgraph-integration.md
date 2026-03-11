# LangGraph Integration Guide

This guide explains how to use the `zeroclaw-tools` Python package for consistent tool calling with any OpenAI-compatible LLM provider.

## Background

Some LLM providers, particularly Chinese models like GLM-5 (Zhipu AI), have inconsistent tool calling behavior when using text-based tool invocation. ZeroClaw's Rust core uses structured tool calling via the OpenAI API format, but some models respond better to a different approach.

LangGraph provides a stateful graph execution engine that guarantees consistent tool calling behavior regardless of the underlying model's native capabilities.

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                      Your Application                        │
├─────────────────────────────────────────────────────────────┤
│                   zeroclaw-tools Agent                       │
│                                                              │
│   ┌─────────────────────────────────────────────────────┐   │
│   │              LangGraph StateGraph                    │   │
│   │                                                      │   │
│   │    ┌────────────┐         ┌────────────┐            │   │
│   │    │   Agent    │ ──────▶ │   Tools    │            │   │
│   │    │   Node     │ ◀────── │   Node     │            │   │
│   │    └────────────┘         └────────────┘            │   │
│   │         │                       │                    │   │
│   │         ▼                       ▼                    │   │
│   │    [Continue?]            [Execute Tool]             │   │
│   │         │                       │                    │   │
│   │    Yes │ No                Result│                    │   │
│   │         ▼                       ▼                    │   │
│   │      [END]              [Back to Agent]              │   │
│   │                                                      │   │
│   └─────────────────────────────────────────────────────┘   │
│                                                              │
├─────────────────────────────────────────────────────────────┤
│            OpenAI-Compatible LLM Provider                    │
│   (Z.AI, OpenRouter, Groq, DeepSeek, Ollama, etc.)          │
└─────────────────────────────────────────────────────────────┘
```

## Quick Start

### Installation

```bash
pip install zeroclaw-tools
```

### Basic Usage

```python
import asyncio
from zeroclaw_tools import create_agent, shell, file_read, file_write
from langchain_core.messages import HumanMessage

async def main():
    agent = create_agent(
        tools=[shell, file_read, file_write],
        model="glm-5",
        api_key="your-api-key",
        base_url="https://api.z.ai/api/coding/paas/v4"
    )
    
    result = await agent.ainvoke({
        "messages": [HumanMessage(content="Read /etc/hostname and tell me the machine name")]
    })
    
    print(result["messages"][-1].content)

asyncio.run(main())
```

## Available Tools

### Core Tools

| Tool | Description |
|------|-------------|
| `shell` | Execute shell commands |
| `file_read` | Read file contents |
| `file_write` | Write content to files |

### Extended Tools

| Tool | Description |
|------|-------------|
| `web_search` | Search the web (requires `BRAVE_API_KEY`) |
| `http_request` | Make HTTP requests |
| `memory_store` | Store data in persistent memory |
| `memory_recall` | Recall stored data |

## Custom Tools

Create your own tools with the `@tool` decorator:

```python
from zeroclaw_tools import tool, create_agent

@tool
def get_weather(city: str) -> str:
    """Get the current weather for a city."""
    # Your implementation
    return f"Weather in {city}: Sunny, 25°C"

@tool
def query_database(sql: str) -> str:
    """Execute a SQL query and return results."""
    # Your implementation
    return "Query returned 5 rows"

agent = create_agent(
    tools=[get_weather, query_database],
    model="glm-5",
    api_key="your-key"
)
```

## Provider Configuration

### Z.AI / GLM-5

```python
agent = create_agent(
    model="glm-5",
    api_key="your-zhipu-key",
    base_url="https://api.z.ai/api/coding/paas/v4"
)
```

### OpenRouter

```python
agent = create_agent(
    model="anthropic/claude-sonnet-4-6",
    api_key="your-openrouter-key",
    base_url="https://openrouter.ai/api/v1"
)
```

### Groq

```python
agent = create_agent(
    model="llama-3.3-70b-versatile",
    api_key="your-groq-key",
    base_url="https://api.groq.com/openai/v1"
)
```

### Ollama (Local)

```python
agent = create_agent(
    model="llama3.2",
    base_url="http://localhost:11434/v1"
)
```

## Discord Bot Integration

```python
import os
from zeroclaw_tools.integrations import DiscordBot

bot = DiscordBot(
    token=os.environ["DISCORD_TOKEN"],
    guild_id=123456789,  # Your Discord server ID
    allowed_users=["123456789"],  # User IDs that can use the bot
    api_key=os.environ["API_KEY"],
    model="glm-5"
)

bot.run()
```

## CLI Usage

```bash
# Set environment variables
export API_KEY="your-key"
export BRAVE_API_KEY="your-brave-key"  # Optional, for web search

# Single message
zeroclaw-tools "What is the current date?"

# Interactive mode
zeroclaw-tools -i
```

## Comparison with Rust ZeroClaw

| Aspect | Rust ZeroClaw | zeroclaw-tools |
|--------|---------------|-----------------|
| **Performance** | Ultra-fast (~10ms startup) | Python startup (~500ms) |
| **Memory** | <5 MB | ~50 MB |
| **Binary size** | ~3.4 MB | pip package |
| **Tool consistency** | Model-dependent | LangGraph guarantees |
| **Extensibility** | Rust traits | Python decorators |
| **Ecosystem** | Rust crates | PyPI packages |

**When to use Rust ZeroClaw:**
- Production edge deployments
- Resource-constrained environments (Raspberry Pi, etc.)
- Maximum performance requirements

**When to use zeroclaw-tools:**
- Models with inconsistent native tool calling
- Python-centric development
- Rapid prototyping
- Integration with Python ML ecosystem

## Troubleshooting

### "API key required" error

Set the `API_KEY` environment variable or pass `api_key` to `create_agent()`.

### Tool calls not executing

Ensure your model supports function calling. Some older models may not support tools.

### Rate limiting

Add delays between calls or implement your own rate limiting:

```python
import asyncio

for message in messages:
    result = await agent.ainvoke({"messages": [message]})
    await asyncio.sleep(1)  # Rate limit
```

## Related Projects

- [rs-graph-llm](https://github.com/a-agmon/rs-graph-llm) - Rust LangGraph alternative
- [langchain-rust](https://github.com/Abraxas-365/langchain-rust) - LangChain for Rust
- [llm-chain](https://github.com/sobelio/llm-chain) - LLM chains in Rust
