# LangGraph 集成指南

本指南解释如何使用 `jhedaiclaw-tools` Python 包与任何兼容 OpenAI 的 LLM（大语言模型，Large Language Model）提供商实现一致的工具调用。

## 背景

某些 LLM 提供商，特别是像 GLM-5（智谱 AI）这样的中文模型，在使用基于文本的工具调用时行为不一致。JhedaiClaw 的 Rust 核心通过 OpenAI API 格式使用结构化工具调用，但某些模型对不同方法的响应更好。

LangGraph 提供了有状态的图执行引擎，无论底层模型的原生能力如何，都能保证一致的工具调用行为。

## 架构

```
┌─────────────────────────────────────────────────────────────┐
│                      Your Application                        │
├─────────────────────────────────────────────────────────────┤
│                   jhedaiclaw-tools Agent                       │
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

## 快速开始

### 安装

```bash
pip install jhedaiclaw-tools
```

### 基本用法

```python
import asyncio
from jhedaiclaw_tools import create_agent, shell, file_read, file_write
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

## 可用工具

### 核心工具

| 工具         | 描述            |
| ------------ | --------------- |
| `shell`      | 执行 shell 命令 |
| `file_read`  | 读取文件内容    |
| `file_write` | 向文件写入内容  |

### 扩展工具

| 工具            | 描述                             |
| --------------- | -------------------------------- |
| `web_search`    | 网页搜索（需要 `BRAVE_API_KEY`） |
| `http_request`  | 发送 HTTP 请求                   |
| `memory_store`  | 将数据存储到持久化内存           |
| `memory_recall` | 召回存储的数据                   |

## 自定义工具

使用 `@tool` 装饰器创建你自己的工具：

```python
from jhedaiclaw_tools import tool, create_agent

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

## 提供商配置

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

### Ollama（本地）

```python
agent = create_agent(
    model="llama3.2",
    base_url="http://localhost:11434/v1"
)
```

## Discord 机器人集成

```python
import os
from jhedaiclaw_tools.integrations import DiscordBot

bot = DiscordBot(
    token=os.environ["DISCORD_TOKEN"],
    guild_id=123456789,  # 你的 Discord 服务器 ID
    allowed_users=["123456789"],  # 可以使用机器人的用户 ID
    api_key=os.environ["API_KEY"],
    model="glm-5"
)

bot.run()
```

## CLI 用法

```bash
# 设置环境变量
export API_KEY="your-key"
export BRAVE_API_KEY="your-brave-key"  # 可选，用于网页搜索

# 单条消息
jhedaiclaw-tools "What is the current date?"

# 交互模式
jhedaiclaw-tools -i
```

## 与 Rust JhedaiClaw 的对比

| 方面           | Rust JhedaiClaw    | jhedaiclaw-tools      |
| -------------- | ------------------ | --------------------- |
| **性能**       | 超快（~10ms 启动） | Python 启动（~500ms） |
| **内存**       | <5 MB              | ~50 MB                |
| **二进制大小** | ~3.4 MB            | pip 包                |
| **工具一致性** | 依赖模型           | LangGraph 保证        |
| **可扩展性**   | Rust 特征          | Python 装饰器         |
| **生态系统**   | Rust crates        | PyPI 包               |

**何时使用 Rust JhedaiClaw：**

- 生产环境边缘部署
- 资源受限环境（树莓派等）
- 最高性能要求

**何时使用 jhedaiclaw-tools：**

- 原生工具调用行为不一致的模型
- 以 Python 为中心的开发
- 快速原型开发
- 与 Python 机器学习生态系统集成

## 故障排除

### "API key required" 错误

设置 `API_KEY` 环境变量，或向 `create_agent()` 传递 `api_key` 参数。

### 工具调用未执行

确保你的模型支持函数调用。某些旧模型可能不支持工具。

### 速率限制

在调用之间添加延迟或实现你自己的速率限制：

```python
import asyncio

for message in messages:
    result = await agent.ainvoke({"messages": [message]})
    await asyncio.sleep(1)  # 速率限制
```

## 相关项目

- [rs-graph-llm](https://github.com/a-agmon/rs-graph-llm) - Rust 版 LangGraph 替代方案
- [langchain-rust](https://github.com/Abraxas-365/langchain-rust) - Rust 版 LangChain
- [llm-chain](https://github.com/sobelio/llm-chain) - Rust 中的 LLM 链
