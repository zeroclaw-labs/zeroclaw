# LangGraph 整合指南（繁體中文）

本指南說明如何使用 `zeroclaw-tools` Python 套件，搭配任何 OpenAI 相容的 LLM 供應商實現一致的工具呼叫行為。

## 背景

部分 LLM 供應商（特別是中國模型如 GLM-5（智譜 AI））在使用文字型工具呼叫時，行為不夠穩定一致。ZeroClaw 的 Rust 核心透過 OpenAI API 格式進行結構化工具呼叫，但某些模型對不同的方式有更好的回應效果。

LangGraph 提供了一個具狀態的圖形執行引擎，無論底層模型的原生能力為何，都能確保工具呼叫行為的一致性。

## 架構

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

## 快速開始

### 安裝

```bash
pip install zeroclaw-tools
```

### 基本用法

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

## 可用工具

### 核心工具

| 工具 | 說明 |
|------|------|
| `shell` | 執行 shell 指令 |
| `file_read` | 讀取檔案內容 |
| `file_write` | 將內容寫入檔案 |

### 擴充工具

| 工具 | 說明 |
|------|------|
| `web_search` | 網頁搜尋（需要 `BRAVE_API_KEY`） |
| `http_request` | 發送 HTTP 請求 |
| `memory_store` | 將資料儲存至持久化記憶體 |
| `memory_recall` | 從記憶體中召回已儲存的資料 |

## 自訂工具

使用 `@tool` 裝飾器建立自訂工具：

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

## 供應商設定

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

### Ollama（本機）

```python
agent = create_agent(
    model="llama3.2",
    base_url="http://localhost:11434/v1"
)
```

## Discord Bot 整合

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

## CLI 用法

```bash
# 設定環境變數
export API_KEY="your-key"
export BRAVE_API_KEY="your-brave-key"  # 選用，供網頁搜尋使用

# 單次訊息
zeroclaw-tools "What is the current date?"

# 互動模式
zeroclaw-tools -i
```

## 與 Rust ZeroClaw 的比較

| 面向 | Rust ZeroClaw | zeroclaw-tools |
|------|---------------|-----------------|
| **效能** | 極快（啟動約 10ms） | Python 啟動（約 500ms） |
| **記憶體** | <5 MB | 約 50 MB |
| **執行檔大小** | 約 3.4 MB | pip 套件 |
| **工具一致性** | 視模型而定 | LangGraph 保證 |
| **擴充性** | Rust traits | Python 裝飾器 |
| **生態系** | Rust crates | PyPI 套件 |

**適合使用 Rust ZeroClaw 的場景：**
- 正式環境的邊緣部署
- 資源受限的環境（Raspberry Pi 等）
- 對效能有極致要求

**適合使用 zeroclaw-tools 的場景：**
- 模型原生工具呼叫行為不穩定
- 以 Python 為主的開發環境
- 快速原型開發
- 需要與 Python 機器學習生態系整合

## 疑難排解

### 「API key required」錯誤

設定 `API_KEY` 環境變數，或在 `create_agent()` 中傳入 `api_key` 參數。

### 工具呼叫未執行

請確認你的模型支援 function calling 功能。部分較舊的模型可能不支援工具呼叫。

### 速率限制

在呼叫之間加入延遲，或自行實作速率限制機制：

```python
import asyncio

for message in messages:
    result = await agent.ainvoke({"messages": [message]})
    await asyncio.sleep(1)  # Rate limit
```

## 相關專案

- [rs-graph-llm](https://github.com/a-agmon/rs-graph-llm) - Rust 版 LangGraph 替代方案
- [langchain-rust](https://github.com/Abraxas-365/langchain-rust) - Rust 版 LangChain
- [llm-chain](https://github.com/sobelio/llm-chain) - Rust 版 LLM 鏈式呼叫
